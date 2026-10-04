//! Rootless execution of assigned CI preview worlds. CI containers are never
//! represented as account servers and run with the same DockerSandbox policy.

use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{IpAddr, SocketAddr},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use serde::Deserialize;
use serde_json::json;
use tar::Archive;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    process::Command,
    time::{sleep, timeout},
};
use uuid::Uuid;

use crate::{
    ci::{CI_RUNTIME_DISK_BYTES, CI_RUNTIME_MEMORY_BYTES, CiRuntimePlacement, CiRuntimeState},
    config::StorageMode,
    pumpkin_control::PumpkinControl,
    worker::runtime::settings::render_pumpkin_config,
    worker::{
        control_plane::WorkerControl,
        ingress::{GatewayIngressPolicy, PublishedWorkloadPorts, docker_publish_binding},
        sandbox::{
            DockerSandbox, WorkloadLimits, ensure_rootless_volume_owner,
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
    sandbox: DockerSandbox,
    runtime_api_proxy_image: String,
    runtime_api_proxy_upstream: String,
    runtime_api_proxy_egress_network: String,
    runtime_api_proxy_allow_insecure: bool,
}

impl CiPreviewWorker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        control_plane: WorkerControl,
        root: PathBuf,
        image: String,
        worker_id: Uuid,
        worker_key: String,
        worker_address: IpAddr,
        signal_address: IpAddr,
        gateway_address: IpAddr,
        storage_mode: StorageMode,
        runtime_api_proxy_image: String,
        runtime_api_proxy_upstream: String,
        runtime_api_proxy_egress_network: String,
        runtime_api_proxy_allow_insecure: bool,
    ) -> Self {
        let runtime_root = root.join("ci-preview-runtime");
        let storage = StorageManager::new(root.join("ci-preview-storage"), storage_mode);
        Self {
            control_plane,
            runtime_root,
            storage,
            image,
            worker_id,
            worker_key,
            worker_address,
            signal_address,
            gateway_address,
            sandbox: DockerSandbox,
            runtime_api_proxy_image,
            runtime_api_proxy_upstream,
            runtime_api_proxy_egress_network,
            runtime_api_proxy_allow_insecure,
        }
    }

    pub async fn reconcile(&self) -> Result<()> {
        for placement in self.control_plane.ci_runtime_assignments().await? {
            if placement.worker_id != self.worker_id {
                continue;
            }
            let result = match placement.state {
                CiRuntimeState::Starting => self.start(&placement).await,
                CiRuntimeState::Stopping => self.stop(&placement, true).await,
                CiRuntimeState::Deleting => self.stop(&placement, false).await.and_then(|_| Ok(())),
                CiRuntimeState::Running => self.observe(&placement).await,
                CiRuntimeState::Stopped => Ok(()),
            };
            if let Err(error) = result {
                tracing::error!(preview_id = %placement.preview_id, generation = placement.generation,
                    state = ?placement.state, ?error, "CI runtime reconciliation failed");
            }
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
            let network = crate::worker::sandbox::workload_network_name(
                placement.preview_id,
                placement.generation,
            );
            crate::runtime_api_proxy::remove_container(&crate::runtime_api_proxy::container_name(
                placement.preview_id,
            ))
            .await?;
            crate::runtime_api_proxy::start_container(
                &self.runtime_api_proxy_image,
                &crate::runtime_api_proxy::container_name(placement.preview_id),
                "ci",
                placement.preview_id,
                placement.generation,
                &capability,
                &self.runtime_api_proxy_upstream,
                &network,
                &self.runtime_api_proxy_egress_network,
                self.runtime_api_proxy_allow_insecure,
            )
            .await?;
            let container = container_name(placement.preview_id);
            self.sandbox
                .verify_isolated_network(
                    &network,
                    &[
                        container,
                        crate::runtime_api_proxy::container_name(placement.preview_id),
                    ],
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
            // Host firewall state is volatile across worker restarts. Reinstall
            // the exact generation policy before admitting an already-running
            // container back into the routing plane.
            ingress
                .install(placement.preview_id, placement.generation, published_ports)
                .await?;
            return self.wait_for_java(placement).await;
        }
        self.remove_stale(placement.preview_id, placement.generation)
            .await?;
        self.sandbox
            .verify_with_limits(&self.image, ci_limits())
            .await
            .context("verify rootless preview runtime limits")?;
        ingress.verify_host_policy().await?;
        let world = self.storage.ensure(placement.preview_id).await?;
        validate_quota_volume(&world, CI_RUNTIME_DISK_BYTES as u64)
            .await
            .context("CI world must have a hard quota filesystem")?;
        let (daemon_uid, daemon_gid) = self.sandbox.rootless_daemon_identity().await?;
        ensure_rootless_volume_owner(&world, daemon_uid, daemon_gid).await?;
        let runtime_dir = self.runtime_dir(placement.preview_id);
        remove_owned_runtime_dir(&self.runtime_root, placement.preview_id)?;
        fs::create_dir_all(&runtime_dir)?;
        write_runtime_owner(&runtime_dir, placement.preview_id)?;
        ensure_rootless_volume_owner(&runtime_dir, daemon_uid, daemon_gid).await?;
        let archive = self.control_plane.download_ci_artifact(placement).await?;
        let metadata = unpack_artifact(&archive, &runtime_dir)?;
        let target = format!("{}-linux", std::env::consts::ARCH);
        if metadata.repository != placement.repository
            || metadata.commit_sha != placement.commit_sha
            || metadata.target_architecture != target
        {
            bail!("CI artifact source identity does not match assigned preview");
        }
        let pumpkin_config = render_pumpkin_config(
            &json!({"motd": format!("Kurbis preview {}", &placement.commit_sha[..12]), "view_distance": 10}),
            25565,
            19132,
            "0.0.0.0:19132"
                .parse()
                .expect("valid internal NetherNet endpoint"),
            self.worker_address,
            placement.bedrock_port as u16,
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
        let proxy_name = crate::runtime_api_proxy::container_name(placement.preview_id);
        crate::runtime_api_proxy::start_container(
            &self.runtime_api_proxy_image,
            &proxy_name,
            "ci",
            placement.preview_id,
            placement.generation,
            &capability,
            &self.runtime_api_proxy_upstream,
            &network,
            &self.runtime_api_proxy_egress_network,
            self.runtime_api_proxy_allow_insecure,
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
        let java = docker_publish_binding(
            published_ports.java_tcp.ip(),
            published_ports.java_tcp.port(),
            25565,
            "tcp",
        )?;
        let java_health = docker_publish_binding(
            if published_ports.java_tcp.is_ipv4() {
                IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
            } else {
                IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
            },
            published_ports.java_tcp.port(),
            25565,
            "tcp",
        )?;
        let bedrock = docker_publish_binding(
            published_ports.bedrock_udp.ip(),
            published_ports.bedrock_udp.port(),
            19132,
            "udp",
        )?;
        let signal = docker_publish_binding(
            published_ports.bedrock_signal_tcp.ip(),
            published_ports.bedrock_signal_tcp.port(),
            19132,
            "tcp",
        )?;
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
            .run_security_args(ci_limits(), "0:0", &network, true)?;
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
            "--publish".into(),
            java,
            "--publish".into(),
            java_health,
            "--publish".into(),
            bedrock,
            "--publish".into(),
            signal,
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
            .verify_isolated_network(&network, &[container, proxy_name])
            .await?;
        self.wait_for_java(placement).await
    }

    async fn wait_for_java(&self, placement: &CiRuntimePlacement) -> Result<()> {
        timeout(JAVA_READY_TIMEOUT, async {
            loop {
                if java_status_probe(SocketAddr::new(
                    if self.worker_address.is_ipv4() {
                        IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
                    } else {
                        IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
                    },
                    placement.java_port as u16,
                ))
                .await
                .is_ok()
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
        if self.container_generation(placement.preview_id).await? == Some(placement.generation) {
            let capability = fs::read_to_string(
                self.runtime_dir(placement.preview_id)
                    .join("runtime.capability"),
            )?;
            let socket = self
                .storage
                .path(placement.preview_id)
                .join("run/host-control.sock");
            let control = PumpkinControl::new(capability.trim().to_owned());
            // Save explicitly before shutdown so the persistent world is durable
            // before the DB releases ports and active-resource reservation.
            control
                .save(&socket)
                .await
                .context("save CI world before idle stop")?;
            control
                .shutdown(&socket)
                .await
                .context("gracefully stop CI Pumpkin")?;
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
        }
        self.remove_stopped_container(placement.preview_id).await?;
        crate::runtime_api_proxy::remove_container(&crate::runtime_api_proxy::container_name(
            placement.preview_id,
        ))
        .await?;
        self.sandbox
            .remove_isolated_networks(placement.preview_id)
            .await?;
        GatewayIngressPolicy::new(self.gateway_address)?
            .remove_server(placement.preview_id)
            .await?;
        Ok(())
    }

    async fn force_stop(&self, placement: &CiRuntimePlacement) -> Result<()> {
        let name = container_name(placement.preview_id);
        if let Some(generation) = self
            .container_label_generation(placement.preview_id)
            .await?
        {
            if generation != placement.generation {
                bail!(
                    "refusing to stop CI preview generation {generation} with stale assignment {}",
                    placement.generation
                );
            }
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
        crate::runtime_api_proxy::remove_container(&crate::runtime_api_proxy::container_name(
            placement.preview_id,
        ))
        .await?;
        self.sandbox
            .remove_isolated_network(placement.preview_id, placement.generation)
            .await?;
        // Firewall rules are generation-tagged and retained when container
        // removal fails. Remove them only after Docker confirms teardown, so a
        // failed start or expiry cannot leak a port lease or leave open access.
        GatewayIngressPolicy::new(self.gateway_address)?
            .remove_server(placement.preview_id)
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
            crate::runtime_api_proxy::remove_container(&crate::runtime_api_proxy::container_name(
                preview_id,
            ))
            .await?;
            self.sandbox
                .remove_isolated_network(preview_id, generation)
                .await?;
        }
        crate::runtime_api_proxy::remove_container(&crate::runtime_api_proxy::container_name(
            preview_id,
        ))
        .await?;
        self.sandbox.remove_isolated_networks(preview_id).await?;
        // A crashed worker may have lost the container but left firewall state.
        // Clear only this preview's uniquely tagged rules before installing a
        // new generation or allowing worker reconciliation to continue.
        GatewayIngressPolicy::new(self.gateway_address)?
            .remove_server(preview_id)
            .await?;
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
        labels
            .get("kurbis.generation")
            .and_then(serde_json::Value::as_str)
            .map(str::parse::<i64>)
            .transpose()
            .context("invalid CI runtime generation label")
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

#[derive(Debug, Deserialize)]
struct ArtifactMetadata {
    repository: String,
    commit_sha: String,
    target_architecture: String,
    format_version: u32,
}

fn unpack_artifact(bytes: &[u8], target: &Path) -> Result<ArtifactMetadata> {
    if bytes.is_empty() || bytes.len() > 1024 * 1024 * 1024 {
        bail!("CI artifact has invalid size");
    }
    let decoder = GzDecoder::new(bytes);
    let mut archive = Archive::new(decoder);
    let mut found = std::collections::HashSet::new();
    let mut metadata = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        if path.components().count() != 1 {
            bail!("CI artifact contains a nested path");
        }
        let name = path
            .to_str()
            .context("CI artifact entry name is not UTF-8")?
            .to_owned();
        if ![
            "pumpkin",
            "kurbis-control.wasm",
            "metadata.json",
            "BUILD.log",
            "LICENSE",
            "REVISION",
        ]
        .contains(&name.as_str())
            || !found.insert(name.clone())
        {
            bail!("CI artifact contains an unexpected or duplicate entry");
        }
        if !entry.header().entry_type().is_file() {
            bail!("CI artifact contains a non-regular file");
        }
        let entry_size = entry.size();
        let limit = if name == "pumpkin" {
            512 * 1024 * 1024
        } else {
            64 * 1024 * 1024
        };
        if entry_size > limit {
            bail!("CI artifact entry is too large");
        }
        let output = target.join(&name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(if name == "pumpkin" { 0o755 } else { 0o644 })
            .open(output)?;
        let mut copy = (&mut entry).take(limit as u64 + 1);
        let mut content = Vec::new();
        copy.read_to_end(&mut content)?;
        if content.len() as u64 != entry_size {
            bail!("CI artifact entry was truncated");
        }
        if name == "metadata.json" {
            metadata = Some(serde_json::from_slice::<ArtifactMetadata>(&content)?);
        }
        file.write_all(&content)?;
        file.sync_all()?;
    }
    for required in ["pumpkin", "kurbis-control.wasm", "metadata.json"] {
        if !found.contains(required) {
            bail!("CI artifact is missing {required}");
        }
    }
    let metadata = metadata.context("CI artifact metadata is missing")?;
    if metadata.format_version != 1 {
        bail!("unsupported CI artifact format");
    }
    Ok(metadata)
}

fn read_managed_artifact(source: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(source)
        .with_context(|| format!("open managed CI artifact at {}", source.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        bail!("managed CI artifact is not a bounded regular file");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() {
        bail!("managed CI artifact changed while it was read");
    }
    Ok(bytes)
}

fn open_directory(path: &Path) -> Result<fs::File> {
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("open non-symlink CI world directory {}", path.display()))?)
}

fn ensure_child_directory(
    parent: &fs::File,
    name: &str,
    mode: u32,
    uid: u32,
    gid: u32,
) -> Result<fs::File> {
    let mode = libc::mode_t::try_from(mode).context("directory mode is out of range")?;
    let name = std::ffi::CString::new(name)?;
    let mut fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), mode) } < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.into());
            }
        }
        fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
    }
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("open safe CI world subdirectory");
    }
    let directory = unsafe { fs::File::from_raw_fd(fd) };
    set_owner_mode(&directory, uid, gid, u32::from(mode))?;
    Ok(directory)
}

fn set_owner_mode(file: &fs::File, uid: u32, gid: u32, mode: u32) -> Result<()> {
    let mode = libc::mode_t::try_from(mode).context("file mode is out of range")?;
    if unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } < 0 {
        return Err(std::io::Error::last_os_error()).context("set safe CI world ownership");
    }
    if unsafe { libc::fchmod(file.as_raw_fd(), mode) } < 0 {
        return Err(std::io::Error::last_os_error()).context("set safe CI world permissions");
    }
    Ok(())
}

fn atomic_write_at(
    parent: &fs::File,
    name: &str,
    bytes: &[u8],
    mode: u32,
    uid: u32,
    gid: u32,
) -> Result<()> {
    let name = std::ffi::CString::new(name)?;
    let temporary = std::ffi::CString::new(format!(".ci-{}.tmp", Uuid::new_v4()))?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            temporary.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("create CI world temporary file");
    }
    let mut file = unsafe { fs::File::from_raw_fd(fd) };
    let result = (|| -> Result<()> {
        set_owner_mode(&file, uid, gid, mode)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if unsafe {
            libc::renameat(
                parent.as_raw_fd(),
                temporary.as_ptr(),
                parent.as_raw_fd(),
                name.as_ptr(),
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error())
                .context("atomically publish CI world file");
        }
        Ok(())
    })();
    if result.is_err() {
        unsafe {
            libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), 0);
        }
    }
    result
}

fn write_world_file(
    world: &Path,
    name: &str,
    bytes: &[u8],
    mode: u32,
    uid: u32,
    gid: u32,
) -> Result<()> {
    let root = open_directory(world)?;
    atomic_write_at(&root, name, bytes, mode, uid, gid)
}

fn write_secret(path: &Path, value: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(value.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn write_runtime_owner(runtime_dir: &Path, preview_id: Uuid) -> Result<()> {
    let root = open_directory(runtime_dir)?;
    atomic_write_at(
        &root,
        ".ci-preview-owner",
        preview_id.to_string().as_bytes(),
        0o600,
        unsafe { libc::geteuid() },
        unsafe { libc::getegid() },
    )
}

fn remove_owned_runtime_dir(allowed_root: &Path, preview_id: Uuid) -> Result<()> {
    let path = allowed_root.join(preview_id.to_string());
    let root =
        fs::canonicalize(allowed_root).context("CI runtime allowlisted root is unavailable")?;
    let resolved = match fs::canonicalize(&path) {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if resolved.parent() != Some(root.as_path()) || resolved == root {
        bail!("refusing CI runtime deletion outside its managed root");
    }
    let owner = read_managed_artifact(&resolved.join(".ci-preview-owner"), 64)?;
    if owner != preview_id.to_string().as_bytes() {
        bail!("CI runtime cleanup ownership marker does not match preview");
    }
    let metadata = fs::symlink_metadata(&resolved)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("refusing to remove non-directory CI runtime path");
    }
    fs::remove_dir_all(resolved)?;
    Ok(())
}

fn container_name(preview_id: Uuid) -> String {
    format!("kurbis-ci-{}", preview_id.simple())
}
fn ci_limits() -> WorkloadLimits {
    WorkloadLimits::server(CI_RUNTIME_MEMORY_BYTES as u64, CI_RUNTIME_DISK_BYTES as u64)
}

fn ensure_linux() -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("CI previews require a Linux rootless Docker worker");
    }
    Ok(())
}

async fn java_status_probe(address: SocketAddr) -> Result<()> {
    let mut stream = timeout(Duration::from_secs(2), TcpStream::connect(address)).await??;
    let host = "ci-preview";
    let mut handshake = Vec::new();
    write_varint(0, &mut handshake);
    write_varint(-1, &mut handshake);
    write_varint(host.len() as i32, &mut handshake);
    handshake.extend_from_slice(host.as_bytes());
    handshake.extend_from_slice(&25565u16.to_be_bytes());
    write_varint(1, &mut handshake);
    let mut frame = Vec::new();
    write_varint(handshake.len() as i32, &mut frame);
    frame.extend(handshake);
    stream.write_all(&frame).await?;
    let mut packet = Vec::new();
    write_varint(0, &mut packet);
    let mut frame = Vec::new();
    write_varint(packet.len() as i32, &mut frame);
    frame.extend(packet);
    stream.write_all(&frame).await?;
    let size = read_varint_async(&mut stream).await?;
    if !(1..=8192).contains(&size) {
        bail!("invalid Minecraft status frame");
    }
    let mut response = vec![0; size as usize];
    stream.read_exact(&mut response).await?;
    let mut cursor = 0;
    if read_varint(&response, &mut cursor)? != 0 {
        bail!("unexpected Java status packet");
    }
    let string_len = read_varint(&response, &mut cursor)?;
    if string_len <= 0 || string_len as usize > response.len().saturating_sub(cursor) {
        bail!("invalid Java status JSON");
    }
    let status: serde_json::Value =
        serde_json::from_slice(&response[cursor..cursor + string_len as usize])?;
    if status.get("version").is_none() || status.get("players").is_none() {
        bail!("Java status response missing required fields");
    }
    Ok(())
}
fn write_varint(mut value: i32, out: &mut Vec<u8>) {
    loop {
        if value & !0x7f == 0 {
            out.push(value as u8);
            return;
        }
        out.push(((value & 0x7f) | 0x80) as u8);
        value = ((value as u32) >> 7) as i32;
    }
}
fn read_varint(bytes: &[u8], cursor: &mut usize) -> Result<i32> {
    let mut value = 0i32;
    for shift in (0..35).step_by(7) {
        let byte = *bytes.get(*cursor).context("truncated Java status varint")?;
        *cursor += 1;
        value |= ((byte & 0x7f) as i32) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    bail!("oversized Java status varint")
}
async fn read_varint_async(stream: &mut TcpStream) -> Result<i32> {
    let mut value = 0i32;
    for shift in (0..35).step_by(7) {
        let byte = stream.read_u8().await?;
        value |= ((byte & 0x7f) as i32) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    bail!("oversized Java frame length")
}

#[cfg(test)]
mod tests {
    use super::*;
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

#[cfg(test)]
mod readiness_tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn readiness_uses_a_real_java_status_handshake_and_validates_the_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let backend = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let handshake_size = read_varint_async(&mut stream).await.unwrap() as usize;
            let mut handshake = vec![0; handshake_size];
            stream.read_exact(&mut handshake).await.unwrap();
            let mut cursor = 0;
            assert_eq!(read_varint(&handshake, &mut cursor).unwrap(), 0);
            assert_eq!(read_varint(&handshake, &mut cursor).unwrap(), -1);
            let host_len = read_varint(&handshake, &mut cursor).unwrap() as usize;
            assert_eq!(&handshake[cursor..cursor + host_len], b"ci-preview");

            let status_size = read_varint_async(&mut stream).await.unwrap() as usize;
            let mut status_request = vec![0; status_size];
            stream.read_exact(&mut status_request).await.unwrap();
            let mut cursor = 0;
            assert_eq!(read_varint(&status_request, &mut cursor).unwrap(), 0);

            let json = serde_json::json!({
                "version": { "name": "Pumpkin", "protocol": 767 },
                "players": { "online": 0, "max": 0 },
                "description": { "text": "ready" }
            })
            .to_string();
            let mut packet = Vec::new();
            write_varint(0, &mut packet);
            write_varint(json.len() as i32, &mut packet);
            packet.extend_from_slice(json.as_bytes());
            let mut frame = Vec::new();
            write_varint(packet.len() as i32, &mut frame);
            frame.extend(packet);
            stream.write_all(&frame).await.unwrap();
        });

        java_status_probe(address).await.unwrap();
        backend.await.unwrap();
    }
}
