//! Rootless execution of assigned CI preview worlds. CI containers are never
//! represented as account servers and run with the same DockerSandbox policy.

use std::{
    collections::HashSet,
    fs,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::json;
use tokio::{
    process::Command,
    time::{sleep, timeout},
};
use uuid::Uuid;

use crate::{
    ci::{CI_RUNTIME_DISK_BYTES, CI_RUNTIME_MEMORY_BYTES, CiRuntimePlacement, CiRuntimeState},
    config::StorageMode,
    pumpkin_control::PumpkinControl,
    worker::runtime::settings::{AccountLookups, PermissionLevel, render_pumpkin_config},
    worker::{
        control_plane::WorkerControl,
        ingress::{
            GatewayIngressPolicy, PublishedWorkloadPorts, cleanup_ingress_through,
            ensure_runtime_ingress,
        },
        sandbox::{
            DockerSandbox, PUMPKIN_GID, PUMPKIN_UID, PUMPKIN_USER, WorkloadLimits,
            ensure_rootless_volume_owner,
            scoped_ci_preview_capability, scoped_runtime_token, validate_quota_volume,
        },
        storage::StorageManager,
    },
};

const JAVA_READY_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub struct CiPreviewWorker {
    control_plane: WorkerControl,
    runtime_root: PathBuf,
    storage: StorageManager,
    image: String,
    worker_id: Uuid,
    worker_key: String,
    worker_address: IpAddr,
    signal_address: IpAddr,
    gateway_address: IpAddr,
    gateway_bedrock_ip: std::net::Ipv4Addr,
    gateway_bedrock_port: u16,
    sandbox: DockerSandbox,
    limits: WorkloadLimits,
    runtime_api_proxy_image: String,
    runtime_api_proxy_upstream: String,
    runtime_api_proxy_egress_network: String,
    runtime_api_proxy_allow_insecure: bool,
    runtime_api_relay: std::sync::Arc<crate::worker::runtime_api_relay::WorkerRuntimeApiRelay>,
}

impl CiPreviewWorker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        control_plane: WorkerControl,
        root: PathBuf,
        worker_cpu_millis: u64,
        image: String,
        worker_id: Uuid,
        worker_key: String,
        worker_address: IpAddr,
        signal_address: IpAddr,
        gateway_address: IpAddr,
        gateway_bedrock_ip: std::net::Ipv4Addr,
        gateway_bedrock_port: u16,
        storage_mode: StorageMode,
        runtime_api_proxy_image: String,
        runtime_api_proxy_upstream: String,
        runtime_api_proxy_egress_network: String,
        runtime_api_proxy_allow_insecure: bool,
        runtime_api_relay: std::sync::Arc<crate::worker::runtime_api_relay::WorkerRuntimeApiRelay>,
    ) -> Result<Self> {
        let runtime_root = root.join("ci-preview-runtime");
        let storage = StorageManager::new(root.join("ci-preview-storage"), storage_mode);
        Ok(Self {
            control_plane,
            runtime_root,
            storage,
            image,
            worker_id,
            worker_key,
            worker_address,
            signal_address,
            gateway_address,
            gateway_bedrock_ip,
            gateway_bedrock_port,
            sandbox: DockerSandbox,
            limits: WorkloadLimits::server(
                CI_RUNTIME_MEMORY_BYTES as u64,
                CI_RUNTIME_DISK_BYTES as u64,
                worker_cpu_millis,
            )
            .validate()?,
            runtime_api_proxy_image,
            runtime_api_proxy_upstream,
            runtime_api_proxy_egress_network,
            runtime_api_proxy_allow_insecure,
            runtime_api_relay,
        })
    }

    pub async fn reconcile(&self) -> Result<()> {
        let assignments = self.control_plane.ci_runtime_assignments().await?;
        let assigned = assignments
            .iter()
            .filter(|placement| placement.worker_id == self.worker_id)
            .map(|placement| placement.preview_id)
            .collect::<HashSet<_>>();
        for placement in assignments {
            if placement.worker_id != self.worker_id {
                continue;
            }
            let result = match placement.state {
                CiRuntimeState::Starting => self.start(&placement).await,
                CiRuntimeState::Stopping => self.stop(&placement, true).await,
                CiRuntimeState::Deleting => self.stop(&placement, false).await.map(|_| ()),
                CiRuntimeState::Running => self.observe(&placement).await,
                CiRuntimeState::Stopped => Ok(()),
            };
            if let Err(error) = result {
                tracing::error!(preview_id = %placement.preview_id, generation = placement.generation,
                    state = ?placement.state, ?error, "CI runtime reconciliation failed");
            }
        }
        // Retained idle worlds appear as `stopped` assignments above. A world
        // transferred to another worker after a durable snapshot no longer does;
        // clean its old local quota volume if this worker later comes back.
        let mut managed = self.storage.managed_server_ids().await?;
        managed.extend(owned_runtime_directory_ids(&self.runtime_root)?);
        for preview_id in orphaned_ci_preview_ids(managed, &assigned) {
            // With no current assignment this local UUID is an orphan. The
            // container may already be gone while later-generation relays,
            // networks, or nftables rules remain, so cleanup must inspect and
            // remove every validated generation rather than guessing generation 1.
            self.remove_all_orphan_resources(preview_id).await?;
            self.storage.remove(preview_id).await?;
            remove_owned_runtime_dir(&self.runtime_root, preview_id)?;
            tracing::info!(%preview_id, "removed orphaned worker-local CI preview world");
        }
        Ok(())
    }

    async fn start(&self, placement: &CiRuntimePlacement) -> Result<()> {
        let result = self.start_inner(placement).await;
        match result {
            Ok(()) => {
                if let Err(error) = self.control_plane.ci_runtime_started(placement).await {
                    self.force_stop(placement).await?;
                    return Err(error).context("report started CI runtime");
                }
                Ok(())
            }
            Err(error) => {
                let _ = self.force_stop(placement).await;
                let _ = self.control_plane.ci_runtime_start_failed(placement).await;
                Err(error)
            }
        }
    }

    async fn start_inner(&self, placement: &CiRuntimePlacement) -> Result<()> {
        ensure_linux()?;
        if self.image.trim().is_empty() || self.worker_key.len() < 32 {
            bail!("CI runtime needs a configured image and strong worker key");
        }
        let needs_bootstrap_snapshot = !placement.world_initialized;
        let ingress = GatewayIngressPolicy::new(self.gateway_address)?;
        let capability = scoped_ci_preview_capability(
            &self.worker_key,
            placement.preview_id,
            placement.generation,
        )?;
        self.control_plane
            .ci_runtime_register_capability(placement, &capability)
            .await?;
        if self.container_generation(placement.preview_id).await? == Some(placement.generation) {
            if needs_bootstrap_snapshot {
                bail!(
                    "refusing to admit a running CI process without its durable world-write fence"
                );
            }
            // Release an interrupted app/container barrier before relay
            // health checks can wait on the paused game process.
            self.recover_snapshot_barrier(placement).await?;
            let network = crate::worker::sandbox::workload_network_name(
                placement.preview_id,
                placement.generation,
            );
            crate::runtime_api_proxy::remove_container(placement.preview_id, placement.generation)
                .await?;
            crate::runtime_api_proxy::start_container(
                &self.runtime_api_proxy_image,
                "ci",
                placement.preview_id,
                placement.generation,
                &capability,
                &self.runtime_api_proxy_upstream,
                &network,
                &self.runtime_api_proxy_egress_network,
                self.runtime_api_proxy_allow_insecure,
                Some(self.runtime_api_relay.socket_path()),
            )
            .await?;
            let container = container_name(placement.preview_id);
            let published_ports = PublishedWorkloadPorts {
                java_tcp: SocketAddr::new(self.worker_address, u16::try_from(placement.java_port)?),
                bedrock_udp: SocketAddr::new(
                    self.worker_address,
                    u16::try_from(placement.bedrock_port)?,
                ),
                bedrock_signal_tcp: SocketAddr::new(
                    self.signal_address,
                    u16::try_from(placement.signal_port)?,
                ),
            };
            // Host firewall state is volatile across worker restarts. Reinstall
            // the exact generation policy before admitting an already-running
            // container back into the routing plane.
            self.ensure_ingress_relay(
                placement.preview_id,
                placement.generation,
                &network,
                &container,
                &[
                    container.clone(),
                    crate::runtime_api_proxy::container_name(
                        placement.preview_id,
                        placement.generation,
                    ),
                ],
                published_ports,
            )
            .await?;
            self.wait_for_java(placement).await?;
            return Ok(());
        }
        self.remove_stale(placement.preview_id, placement.generation)
            .await?;
        self.sandbox
            .verify_with_limits(&self.image, self.limits)
            .await
            .context("verify rootless preview runtime limits")?;
        ingress.verify_host_policy().await?;
        let local_world_exists = self
            .storage
            .managed_server_ids()
            .await?
            .contains(&placement.preview_id);
        let world = self.storage.ensure(placement.preview_id).await?;
        validate_quota_volume(&world, CI_RUNTIME_DISK_BYTES as u64)
            .await
            .context("CI world must have a hard quota filesystem")?;
        let (daemon_uid, daemon_gid) = self
            .sandbox
            .rootless_container_identity(PUMPKIN_UID, PUMPKIN_GID)
            .await?;
        ensure_rootless_volume_owner(&world, daemon_uid, daemon_gid).await?;
        let runtime_dir = self.runtime_dir(placement.preview_id);
        remove_owned_runtime_dir(&self.runtime_root, placement.preview_id)?;
        fs::create_dir_all(&runtime_dir)?;
        write_runtime_owner(&runtime_dir, placement.preview_id)?;
        ensure_rootless_volume_owner(&runtime_dir, daemon_uid, daemon_gid).await?;
        let snapshot_path = runtime_dir.join("world-snapshot.tar.gz");
        let restored_snapshot = self
            .control_plane
            .ci_world_snapshot_download(placement, &snapshot_path, CI_RUNTIME_DISK_BYTES as u64)
            .await?;
        if placement.world_initialized && restored_snapshot.is_none() && !local_world_exists {
            bail!("initialized CI world has no verified durable snapshot or retained local volume");
        }
        if let Some(_snapshot_metadata) = restored_snapshot {
            // The local volume may belong to a different host after CI failover.
            // Replace it only after the authenticated shared snapshot is fully
            // downloaded and its size/checksum have been verified.
            self.storage.reset(placement.preview_id).await?;
            unpack_world_snapshot(
                &snapshot_path,
                &world,
                CI_RUNTIME_DISK_BYTES as u64,
                daemon_uid,
                daemon_gid,
            )?;
            fs::remove_file(&snapshot_path)?;
        }
        let artifact_path = runtime_dir.join("artifact.tar.gz");
        self.control_plane
            .download_ci_artifact(placement, &artifact_path)
            .await?;
        let metadata = unpack_artifact_file(&artifact_path, &runtime_dir)?;
        fs::remove_file(&artifact_path)?;
        let target = format!("{}-linux", std::env::consts::ARCH);
        if metadata.repository != placement.repository
            || metadata.commit_sha != placement.commit_sha
            || metadata.target_architecture != target
        {
            bail!("CI artifact source identity does not match assigned preview");
        }
        let pumpkin_config = render_pumpkin_config(
            &json!({"motd": format!("Kurbis preview {}", &placement.commit_sha[..12]), "view_distance": 10}),
            AccountLookups::ThroughApiProxy,
            PermissionLevel::Gamemaster,
            25565,
            19132,
            "0.0.0.0:19132"
                .parse()
                .expect("valid internal NetherNet endpoint"),
            IpAddr::V4(self.gateway_bedrock_ip),
            self.gateway_bedrock_port,
        );
        write_world_file(
            &world,
            "pumpkin.toml",
            pumpkin_config.as_bytes(),
            0o600,
            daemon_uid,
            daemon_gid,
        )?;
        let world_root = open_directory(&world)?;
        let plugins =
            ensure_child_directory(&world_root, "plugins", 0o700, daemon_uid, daemon_gid)?;
        let data = ensure_child_directory(&plugins, "data", 0o700, daemon_uid, daemon_gid)?;
        let plugin_data =
            ensure_child_directory(&data, "kurbis-control", 0o700, daemon_uid, daemon_gid)?;
        let plugin_bytes =
            read_managed_artifact(&runtime_dir.join("kurbis-control.wasm"), 64 * 1024 * 1024)?;
        atomic_write_at(
            &plugins,
            "kurbis-control.wasm",
            &plugin_bytes,
            0o644,
            daemon_uid,
            daemon_gid,
        )?;
        let plugin_config = json!({
            "api_base_url": crate::runtime_api_proxy::SERVICE_BASE_URL,
            "allow_insecure_local_http": true,
            "server_id": placement.preview_id,
            "capability": capability,
            "ci_preview": true,
        });
        atomic_write_at(
            &plugin_data,
            "config.json",
            &serde_json::to_vec(&plugin_config)?,
            0o600,
            daemon_uid,
            daemon_gid,
        )?;

        let network = self
            .sandbox
            .create_isolated_network(placement.preview_id, placement.generation)
            .await?;
        let proxy_name =
            crate::runtime_api_proxy::container_name(placement.preview_id, placement.generation);
        crate::runtime_api_proxy::start_container(
            &self.runtime_api_proxy_image,
            "ci",
            placement.preview_id,
            placement.generation,
            &capability,
            &self.runtime_api_proxy_upstream,
            &network,
            &self.runtime_api_proxy_egress_network,
            self.runtime_api_proxy_allow_insecure,
            Some(self.runtime_api_relay.socket_path()),
        )
        .await?;
        let published_ports = PublishedWorkloadPorts {
            java_tcp: SocketAddr::new(self.worker_address, u16::try_from(placement.java_port)?),
            bedrock_udp: SocketAddr::new(
                self.worker_address,
                u16::try_from(placement.bedrock_port)?,
            ),
            bedrock_signal_tcp: SocketAddr::new(
                self.signal_address,
                u16::try_from(placement.signal_port)?,
            ),
        };
        let world_mount = format!(
            "type=bind,source={},target=/server,bind-propagation=rprivate",
            world.display()
        );
        let artifact_mount = format!(
            "type=bind,source={},target=/runtime,readonly,bind-propagation=rprivate",
            runtime_dir.display()
        );
        let runtime_capability =
            scoped_runtime_token(&self.worker_key, placement.preview_id, placement.generation)?;
        write_secret(&runtime_dir.join("runtime.capability"), &runtime_capability)?;
        let runtime_environment = [
            "PUMPKIN_HOST_CONTROL_SOCKET=/server/run/host-control.sock".to_owned(),
            format!("PUMPKIN_HOST_CONTROL_TOKEN={runtime_capability}"),
        ];
        let container = container_name(placement.preview_id);
        let security = self
            .sandbox
            .run_security_args(self.limits, PUMPKIN_USER, &network, true)?;
        let mut owned = [
            "run",
            "--detach",
            "--name",
            &container,
            "--restart",
            "no",
            "--stop-timeout",
            "10",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        owned.extend(security);
        owned.extend([
            "--label".into(),
            format!("kurbis.ci_preview={}", placement.preview_id),
            "--label".into(),
            format!("kurbis.generation={}", placement.generation),
            "--env".into(),
            runtime_environment[0].clone(),
            "--env".into(),
            runtime_environment[1].clone(),
            "--mount".into(),
            world_mount,
            "--mount".into(),
            artifact_mount,
            "--workdir".into(),
            "/server".into(),
            "--entrypoint".into(),
            "/runtime/pumpkin".into(),
            self.image.clone(),
        ]);
        ingress
            .install(placement.preview_id, placement.generation, published_ports)
            .await?;
        // Persist the world-write fence before the Pumpkin process can start
        // generating or mutating world state. A worker crash at any later boot
        // point now requires a verified checkpoint instead of a cold replacement.
        if needs_bootstrap_snapshot {
            self.control_plane
                .ci_runtime_world_initialized(placement)
                .await?;
        }
        let output = timeout(
            Duration::from_secs(30),
            Command::new("docker")
                .args(owned)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("CI Docker startup timed out")??;
        if !output.status.success() {
            bail!(
                "rootless Docker rejected CI runtime: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        self.sandbox
            .verify_isolated_network(&network, &[container.clone(), proxy_name.clone()])
            .await?;
        self.ensure_ingress_relay(
            placement.preview_id,
            placement.generation,
            &network,
            &container,
            &[container.clone(), proxy_name.clone()],
            published_ports,
        )
        .await?;
        self.wait_for_java(placement).await?;
        if needs_bootstrap_snapshot {
            self.publish_quiescent_snapshot(placement, false).await?;
        }
        Ok(())
    }

    async fn ensure_ingress_relay(
        &self,
        preview_id: Uuid,
        generation: i64,
        internal_network: &str,
        backend_container: &str,
        internal_peers: &[String],
        ports: PublishedWorkloadPorts,
    ) -> Result<()> {
        let policy = GatewayIngressPolicy::new(self.gateway_address)?;
        ensure_runtime_ingress(
            &policy,
            &self.sandbox,
            &self.runtime_api_proxy_image,
            preview_id,
            generation,
            internal_network,
            backend_container,
            internal_peers,
            ports,
        )
        .await
        .context("ensure generation-scoped Gateway-only CI preview ingress")
    }

    async fn wait_for_java(&self, placement: &CiRuntimePlacement) -> Result<()> {
        timeout(JAVA_READY_TIMEOUT, async {
            loop {
                // The relay pings Pumpkin's Java status itself. A probe from
                // this host would need the Java port published a second time
                // on loopback, which rootless Docker cannot do.
                if crate::worker::ingress::relay_is_healthy(
                    placement.preview_id,
                    placement.generation,
                )
                .await?
                {
                    return Ok(());
                }
                if self.container_generation(placement.preview_id).await?
                    != Some(placement.generation)
                {
                    bail!("CI Pumpkin runtime exited or changed generation before Java readiness");
                }
                sleep(Duration::from_millis(500)).await;
            }
        })
        .await
        .context("CI Java status ping did not become ready")?
    }

    async fn observe(&self, placement: &CiRuntimePlacement) -> Result<()> {
        if self.container_generation(placement.preview_id).await? != Some(placement.generation) {
            self.control_plane.ci_runtime_lost(placement).await?;
            return Ok(());
        }
        self.recover_snapshot_barrier(placement).await?;
        if self
            .control_plane
            .ci_world_snapshot_requested(placement)
            .await?
        {
            self.publish_quiescent_snapshot(placement, true).await?;
        }
        let capability = fs::read_to_string(
            self.runtime_dir(placement.preview_id)
                .join("runtime.capability"),
        )?;
        let socket = self
            .storage
            .path(placement.preview_id)
            .join("run/host-control.sock");
        let control = PumpkinControl::new(capability.trim().to_owned());
        control.status(&socket).await?;
        let players = control.players(&socket).await?;
        let report = players
            .into_iter()
            .map(|player| {
                json!({
                    "id": player.id,
                    "name": player.name,
                    "edition": player.edition,
                })
            })
            .collect::<Vec<_>>();
        self.control_plane
            .ci_runtime_report_players(placement, &report)
            .await?;
        Ok(())
    }

    async fn stop(&self, placement: &CiRuntimePlacement, retain_world: bool) -> Result<()> {
        if retain_world {
            self.graceful_stop(placement).await?;
            self.control_plane.ci_runtime_stopped(placement).await?;
        } else {
            self.force_stop(placement).await?;
            self.storage.remove(placement.preview_id).await?;
            remove_owned_runtime_dir(&self.runtime_root, placement.preview_id)?;
            self.control_plane.ci_runtime_deleted(placement).await?;
        }
        Ok(())
    }

    async fn graceful_stop(&self, placement: &CiRuntimePlacement) -> Result<()> {
        let capability = fs::read_to_string(
            self.runtime_dir(placement.preview_id)
                .join("runtime.capability"),
        )?;
        let control = PumpkinControl::new(capability.trim().to_owned());
        let socket = self
            .storage
            .path(placement.preview_id)
            .join("run/host-control.sock");
        if self.container_generation(placement.preview_id).await? == Some(placement.generation) {
            self.recover_snapshot_barrier(placement).await?;
            let mut boundary = WorkerSnapshotBoundary {
                worker: self,
                placement,
                control: control.clone(),
                socket: socket.clone(),
            };
            boundary
                .begin_application_barrier()
                .await
                .context("establish durable application barrier before idle stop")?;
            if let Err(shutdown_error) = control.shutdown(&socket).await {
                if let Err(release_error) = boundary.end_application_barrier().await {
                    return Err(shutdown_error).context(format!(
                        "shutdown CI Pumpkin after quiescent save; barrier release failed: {release_error:#}"
                    ));
                }
                return Err(shutdown_error).context("shutdown CI Pumpkin after quiescent save");
            }
            timeout(Duration::from_secs(60), async {
                loop {
                    if self
                        .container_generation(placement.preview_id)
                        .await?
                        .is_none()
                    {
                        break;
                    }
                    sleep(Duration::from_millis(250)).await;
                }
                Ok::<(), anyhow::Error>(())
            })
            .await
            .context("CI Pumpkin did not stop within 60 seconds")??;
            // Shutdown completed all final world writes; with the process gone,
            // this archive is a point-in-time checkpoint of the stopped world.
            self.persist_world_snapshot(placement).await?;
            self.clear_snapshot_barrier_marker(placement)?;
        } else {
            // If the process disappeared during the stop, never archive its
            // potentially partial local write set. Restore the last verified
            // shared checkpoint or leave the stop claim retryable.
            let runtime_dir = self.runtime_dir(placement.preview_id);
            let snapshot_path = runtime_dir.join("idle-recovery.tar.gz");
            let Some(_) = self
                .control_plane
                .ci_world_snapshot_download(placement, &snapshot_path, CI_RUNTIME_DISK_BYTES as u64)
                .await?
            else {
                bail!("CI runtime exited before a durable idle-stop checkpoint existed");
            };
            let world = self.storage.path(placement.preview_id);
            self.storage.reset(placement.preview_id).await?;
            let (daemon_uid, daemon_gid) = self
                .sandbox
                .rootless_container_identity(PUMPKIN_UID, PUMPKIN_GID)
                .await?;
            unpack_world_snapshot(
                &snapshot_path,
                &world,
                CI_RUNTIME_DISK_BYTES as u64,
                daemon_uid,
                daemon_gid,
            )?;
            fs::remove_file(snapshot_path)?;
            self.clear_snapshot_barrier_marker(placement)?;
        }
        self.remove_stopped_container(placement.preview_id).await?;
        crate::runtime_api_proxy::remove_container(placement.preview_id, placement.generation)
            .await?;
        let ingress = GatewayIngressPolicy::new(self.gateway_address)?;
        cleanup_ingress_through(&ingress, placement.preview_id, placement.generation).await?;
        self.sandbox
            .remove_isolated_networks(placement.preview_id)
            .await?;
        Ok(())
    }

    async fn publish_quiescent_snapshot(
        &self,
        placement: &CiRuntimePlacement,
        complete_request: bool,
    ) -> Result<()> {
        let capability = fs::read_to_string(
            self.runtime_dir(placement.preview_id)
                .join("runtime.capability"),
        )?;
        let control = PumpkinControl::new(capability.trim().to_owned());
        let socket = self
            .storage
            .path(placement.preview_id)
            .join("run/host-control.sock");
        let mut boundary = WorkerSnapshotBoundary {
            worker: self,
            placement,
            control,
            socket,
        };
        publish_at_quiescent_boundary(&mut boundary).await?;
        if complete_request {
            self.control_plane
                .ci_world_snapshot_complete(placement)
                .await?;
        }
        Ok(())
    }

    async fn recover_snapshot_barrier(&self, placement: &CiRuntimePlacement) -> Result<()> {
        let marker = self
            .runtime_dir(placement.preview_id)
            .join("snapshot-barrier-active");
        if !marker.exists() {
            return Ok(());
        }
        let marker_generation = self.read_snapshot_barrier_marker(placement)?;
        if marker_generation.as_deref() != Some(placement.generation.to_string().as_str()) {
            bail!("snapshot barrier marker belongs to a different CI runtime generation");
        }
        let name = container_name(placement.preview_id);
        let capability = fs::read_to_string(
            self.runtime_dir(placement.preview_id)
                .join("runtime.capability"),
        )?;
        let socket = self
            .storage
            .path(placement.preview_id)
            .join("run/host-control.sock");
        let control = PumpkinControl::new(capability.trim().to_owned());
        recover_interrupted_boundary(self.ensure_container_unpaused(&name), async move {
            control
                .end_snapshot_barrier(&socket)
                .await
                .context("release interrupted Pumpkin application barrier")?;
            self.clear_snapshot_barrier_marker(placement)?;
            Ok(())
        })
        .await
    }

    fn read_snapshot_barrier_marker(
        &self,
        placement: &CiRuntimePlacement,
    ) -> Result<Option<String>> {
        let marker = self
            .runtime_dir(placement.preview_id)
            .join("snapshot-barrier-active");
        let metadata = match fs::symlink_metadata(&marker) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("CI snapshot barrier marker is not a regular managed file");
        }
        let bytes = read_managed_artifact(&marker, 64)?;
        Ok(Some(
            String::from_utf8(bytes).context("CI snapshot barrier marker is not UTF-8")?,
        ))
    }

    fn clear_snapshot_barrier_marker(&self, placement: &CiRuntimePlacement) -> Result<()> {
        let Some(generation) = self.read_snapshot_barrier_marker(placement)? else {
            return Ok(());
        };
        if generation.trim() != placement.generation.to_string() {
            bail!("refusing to clear another CI generation's snapshot barrier marker");
        }
        fs::remove_file(
            self.runtime_dir(placement.preview_id)
                .join("snapshot-barrier-active"),
        )?;
        Ok(())
    }

    async fn ensure_container_unpaused(&self, name: &str) -> Result<()> {
        let inspect = Command::new("docker")
            .args(["inspect", "--format", "{{.State.Paused}}", name])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        if !inspect.status.success() {
            if String::from_utf8_lossy(&inspect.stderr).contains("No such object")
                || String::from_utf8_lossy(&inspect.stderr).contains("No such container")
            {
                return Ok(());
            }
            bail!(
                "failed to inspect paused CI runtime {name}: {}",
                String::from_utf8_lossy(&inspect.stderr).trim()
            );
        }
        if String::from_utf8_lossy(&inspect.stdout).trim() != "true" {
            return Ok(());
        }
        let output = Command::new("docker")
            .args(["unpause", name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await?;
        if !output.status.success() {
            bail!(
                "failed to unpause CI runtime {name}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    async fn persist_world_snapshot(&self, placement: &CiRuntimePlacement) -> Result<()> {
        let runtime_dir = self.runtime_dir(placement.preview_id);
        let path = runtime_dir.join(format!("world-snapshot-{}.tar.gz", placement.generation));
        let world = self.storage.path(placement.preview_id);
        let (size_bytes, _checksum) = pack_world(&world, &path, CI_RUNTIME_DISK_BYTES as u64)?;
        let result = self
            .control_plane
            .ci_world_snapshot_upload(placement, &path, size_bytes)
            .await;
        let _ = fs::remove_file(&path);
        result
    }

    async fn force_stop(&self, placement: &CiRuntimePlacement) -> Result<()> {
        let name = container_name(placement.preview_id);
        if let Some(generation) = self
            .container_label_generation(placement.preview_id)
            .await?
            && generation != placement.generation
        {
            bail!(
                "refusing to stop CI preview generation {generation} with stale assignment {}",
                placement.generation
            );
        }
        let output = Command::new("docker")
            .args(["rm", "--force", &name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await?;
        if !output.status.success()
            && !String::from_utf8_lossy(&output.stderr).contains("No such container")
        {
            bail!(
                "failed to stop CI runtime {name}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        crate::runtime_api_proxy::remove_container(placement.preview_id, placement.generation)
            .await?;
        let ingress = GatewayIngressPolicy::new(self.gateway_address)?;
        cleanup_ingress_through(&ingress, placement.preview_id, placement.generation).await?;
        self.sandbox
            .remove_isolated_network(placement.preview_id, placement.generation)
            .await?;
        Ok(())
    }

    async fn remove_stopped_container(&self, preview_id: Uuid) -> Result<()> {
        let name = container_name(preview_id);
        let output = Command::new("docker")
            .args(["rm", &name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await?;
        if output.status.success()
            || String::from_utf8_lossy(&output.stderr).contains("No such container")
        {
            return Ok(());
        }
        bail!(
            "failed to remove stopped CI runtime {name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    async fn remove_all_orphan_resources(&self, preview_id: Uuid) -> Result<()> {
        // The caller established that this preview has no assignment on this
        // worker. Resource enumeration remains ownership-fenced by the UUID's
        // labels, while the maximal bound deliberately covers stale generations
        // whose main runtime container has already disappeared.
        self.remove_stale(preview_id, i64::MAX).await
    }

    async fn remove_stale(&self, preview_id: Uuid, target_generation: i64) -> Result<()> {
        let name = container_name(preview_id);
        if let Some(generation) = self.container_label_generation(preview_id).await? {
            if generation > target_generation {
                bail!(
                    "refusing to remove newer CI generation {generation} for stale target {target_generation}"
                );
            }
            let output = Command::new("docker")
                .args(["rm", "--force", &name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .await?;
            if !output.status.success()
                && !String::from_utf8_lossy(&output.stderr).contains("No such container")
            {
                bail!(
                    "failed to remove stale CI runtime: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            self.sandbox
                .remove_isolated_network(preview_id, generation)
                .await?;
        }
        crate::runtime_api_proxy::remove_containers_through(preview_id, target_generation).await?;
        let ingress = GatewayIngressPolicy::new(self.gateway_address)?;
        cleanup_ingress_through(&ingress, preview_id, target_generation).await?;
        self.sandbox.remove_isolated_networks(preview_id).await?;
        Ok(())
    }

    async fn container_label_generation(&self, preview_id: Uuid) -> Result<Option<i64>> {
        let output = Command::new("docker")
            .args([
                "inspect",
                "--format",
                "{{json .Config.Labels}}",
                &container_name(preview_id),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            if error.contains("No such object") || error.contains("No such container") {
                return Ok(None);
            }
            bail!("unable to inspect CI runtime labels: {}", error.trim());
        }
        let labels: serde_json::Value = serde_json::from_slice(&output.stdout)
            .context("Docker returned invalid CI label JSON")?;
        if labels
            .get("kurbis.ci_preview")
            .and_then(serde_json::Value::as_str)
            != Some(preview_id.to_string().as_str())
        {
            bail!("container name collision: runtime is not owned by this CI preview");
        }
        let generation = labels
            .get("kurbis.generation")
            .and_then(serde_json::Value::as_str)
            .context("CI runtime generation label is missing")?
            .parse::<i64>()
            .context("invalid CI runtime generation label")?;
        if generation <= 0 {
            bail!("CI runtime generation label must be positive");
        }
        Ok(Some(generation))
    }

    async fn container_generation(&self, preview_id: Uuid) -> Result<Option<i64>> {
        let output = Command::new("docker")
            .args([
                "inspect",
                "--format",
                "{{if .State.Running}}{{index .Config.Labels \"kurbis.generation\"}}{{end}}",
                &container_name(preview_id),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            if error.contains("No such object") || error.contains("No such container") {
                return Ok(None);
            }
            bail!("unable to inspect CI runtime: {}", error.trim());
        }
        let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if value.is_empty() {
            Ok(None)
        } else {
            Ok(Some(
                value
                    .parse()
                    .context("invalid CI runtime generation label")?,
            ))
        }
    }

    fn runtime_dir(&self, preview_id: Uuid) -> PathBuf {
        self.runtime_root.join(preview_id.to_string())
    }
}

fn owned_runtime_directory_ids(root: &std::path::Path) -> Result<HashSet<Uuid>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(error) => return Err(error).context("enumerate managed CI runtime directories"),
    };
    let mut ids = HashSet::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Ok(preview_id) = Uuid::parse_str(name) else {
            continue;
        };
        if preview_id.to_string() != name {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        let owner_path = entry.path().join(".ci-preview-owner");
        match fs::symlink_metadata(&owner_path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("inspect managed CI owner marker"),
        }
        let owner = read_managed_artifact(&owner_path, 64)?;
        if owner == preview_id.to_string().as_bytes() {
            ids.insert(preview_id);
        }
    }
    Ok(ids)
}

fn orphaned_ci_preview_ids(managed: HashSet<Uuid>, assigned: &HashSet<Uuid>) -> Vec<Uuid> {
    let mut orphaned = managed
        .into_iter()
        .filter(|preview_id| !assigned.contains(preview_id))
        .collect::<Vec<_>>();
    orphaned.sort_unstable();
    orphaned
}

struct WorkerSnapshotBoundary<'a> {
    worker: &'a CiPreviewWorker,
    placement: &'a CiRuntimePlacement,
    control: PumpkinControl,
    socket: PathBuf,
}

#[async_trait::async_trait]
impl SnapshotBarrier for WorkerSnapshotBoundary<'_> {
    async fn begin_application_barrier(&mut self) -> Result<()> {
        let marker = self
            .worker
            .runtime_dir(self.placement.preview_id)
            .join("snapshot-barrier-active");
        fs::write(&marker, self.placement.generation.to_string())?;
        self.control
            .begin_snapshot_barrier(&self.socket)
            .await
            .context("establish Pumpkin snapshot quiescence barrier")
            .map_err(|error| {
                error.context("snapshot barrier marker retained for crash recovery")
            })?;
        Ok(())
    }

    async fn pause_runtime_container(&mut self) -> Result<()> {
        let name = container_name(self.placement.preview_id);
        let output = Command::new("docker")
            .args(["pause", &name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await?;
        if !output.status.success() {
            bail!(
                "failed to pause CI Pumpkin for snapshot: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    async fn archive_and_publish(&mut self) -> Result<()> {
        self.worker.persist_world_snapshot(self.placement).await
    }

    async fn unpause_runtime_container(&mut self) -> Result<()> {
        self.worker
            .ensure_container_unpaused(&container_name(self.placement.preview_id))
            .await
    }

    async fn end_application_barrier(&mut self) -> Result<()> {
        self.control
            .end_snapshot_barrier(&self.socket)
            .await
            .context("release Pumpkin snapshot barrier")?;
        self.worker.clear_snapshot_barrier_marker(self.placement)
    }
}

mod helpers;
mod snapshot_barrier;
mod world_snapshot;
use helpers::*;
use snapshot_barrier::{
    SnapshotBarrier, publish_at_quiescent_boundary, recover_interrupted_boundary,
};
use world_snapshot::{pack_world, unpack_world_snapshot};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_worker_reconciliation_selects_only_unassigned_ci_volumes() {
        let assigned = HashSet::from([Uuid::from_u128(1), Uuid::from_u128(3)]);
        let managed = HashSet::from([Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)]);
        assert_eq!(
            orphaned_ci_preview_ids(managed, &assigned),
            vec![Uuid::from_u128(2)],
        );
    }
    #[test]
    fn orphan_discovery_includes_only_owned_runtime_directories() {
        let root = tempfile::tempdir().unwrap();
        let owned_id = Uuid::new_v4();
        let unowned_id = Uuid::new_v4();
        let owned = root.path().join(owned_id.to_string());
        let unowned = root.path().join(unowned_id.to_string());
        fs::create_dir(&owned).unwrap();
        fs::create_dir(&unowned).unwrap();
        write_runtime_owner(&owned, owned_id).unwrap();
        fs::write(
            unowned.join("runtime.capability"),
            "not an ownership marker",
        )
        .unwrap();

        let retained_storage_id = Uuid::new_v4();
        let assigned_id = Uuid::new_v4();
        let runtime_dirs = owned_runtime_directory_ids(root.path()).unwrap();
        assert_eq!(runtime_dirs, HashSet::from([owned_id]));

        let mut managed = HashSet::from([retained_storage_id, assigned_id]);
        managed.extend(runtime_dirs);
        let mut expected = vec![retained_storage_id, owned_id];
        expected.sort_unstable();
        assert_eq!(
            orphaned_ci_preview_ids(managed, &HashSet::from([assigned_id])),
            expected,
            "owned worker metadata joins volume discovery while unowned directories and assigned worlds are never reclaimed"
        );
    }

    #[test]
    fn ci_container_name_is_separate_from_account_runtime() {
        assert!(container_name(Uuid::nil()).starts_with("kurbis-ci-"));
    }
    #[test]
    fn ci_artifact_metadata_rejects_malformed() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(unpack_artifact(b"no", tmp.path()).is_err());
    }
}
