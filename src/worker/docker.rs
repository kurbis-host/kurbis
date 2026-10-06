use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use super::{
    ingress::{
        GatewayIngressPolicy, PublishedWorkloadPorts, cleanup_ingress_through,
        ensure_runtime_ingress,
    },
    runtime::{ServerRuntime, settings::render_pumpkin_config},
};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;
use uuid::Uuid;

use crate::{
    model::{PlacementRecord, SERVER_MEMORY_BYTES, SERVER_STORAGE_BYTES},
    pumpkin_control::PumpkinControl,
    runtime_api_proxy,
    worker::sandbox::{
        DockerSandbox, WorkloadLimits, ensure_rootless_volume_owner,
        scoped_runtime_environment_args, scoped_runtime_token, validate_quota_volume,
    },
};

#[derive(Clone)]
pub struct DockerRuntime {
    image: String,
    worker_control_key: String,
    root: PathBuf,
    sandbox: DockerSandbox,
    ingress: GatewayIngressPolicy,
    public_address: IpAddr,
    signal_address: IpAddr,
    gateway_bedrock_ip: std::net::Ipv4Addr,
    gateway_bedrock_port: u16,
    runtime_api_proxy_image: String,
    runtime_api_proxy_upstream: String,
    runtime_api_proxy_egress_network: String,
    runtime_api_proxy_allow_insecure: bool,
}

impl DockerRuntime {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        image: String,
        host_control_token: String,
        root: PathBuf,
        public_address: IpAddr,
        signal_address: IpAddr,
        gateway_address: IpAddr,
        gateway_bedrock_ip: std::net::Ipv4Addr,
        gateway_bedrock_port: u16,
        runtime_api_proxy_image: String,
        runtime_api_proxy_upstream: String,
        runtime_api_proxy_egress_network: String,
        runtime_api_proxy_allow_insecure: bool,
    ) -> Result<Self> {
        let ingress = GatewayIngressPolicy::new(gateway_address)?;
        crate::runtime_api_proxy::validate_upstream_url(
            &runtime_api_proxy_upstream,
            runtime_api_proxy_allow_insecure,
        )?;
        Ok(Self {
            image,
            worker_control_key: host_control_token,
            root,
            sandbox: DockerSandbox,
            public_address,
            signal_address,
            gateway_bedrock_ip,
            gateway_bedrock_port,
            ingress,
            runtime_api_proxy_image,
            runtime_api_proxy_upstream,
            runtime_api_proxy_egress_network,
            runtime_api_proxy_allow_insecure,
        })
    }

    async fn ensure_ingress_relay(
        &self,
        server_id: Uuid,
        generation: i64,
        internal_network: &str,
        backend_container: &str,
        internal_peers: &[String],
        ports: PublishedWorkloadPorts,
    ) -> Result<()> {
        ensure_runtime_ingress(
            &self.ingress,
            &self.sandbox,
            &self.runtime_api_proxy_image,
            server_id,
            generation,
            internal_network,
            backend_container,
            internal_peers,
            ports,
        )
        .await
        .context("ensure generation-scoped Gateway-only workload ingress")
    }
}

#[async_trait]
impl ServerRuntime for DockerRuntime {
    async fn control_for(&self, server_dir: &Path) -> Result<PumpkinControl> {
        let server_id = server_id_from_directory(server_dir)?;
        let generation = tokio::fs::read_to_string(runtime_generation_path(&self.root, server_id))
            .await
            .context("runtime generation capability is missing")?
            .trim()
            .parse::<i64>()
            .context("runtime generation capability is invalid")?;
        Ok(PumpkinControl::new(scoped_runtime_token(
            &self.worker_control_key,
            server_id,
            generation,
        )?))
    }
    fn socket_path(&self, server_dir: &Path) -> Result<PathBuf> {
        let server_id = server_id_from_directory(server_dir)?;
        Ok(control_socket_alias_for_server(server_id))
    }

    async fn start(
        &self,
        server_id: Uuid,
        placement: &PlacementRecord,
        server_dir: &Path,
        settings: &Value,
        api_capability: Option<&str>,
    ) -> Result<()> {
        let api_capability =
            api_capability.context("managed plugin capability is required for Docker runtime")?;
        let name = container_name(server_id);
        let proxy_name = runtime_api_proxy::container_name(server_id, placement.generation);
        match self.running_generation(&name).await? {
            Some(generation) if generation == placement.generation => {
                let network = crate::worker::sandbox::workload_network_name(server_id, generation);
                runtime_api_proxy::remove_container(server_id, placement.generation).await?;
                runtime_api_proxy::start_container(
                    &self.runtime_api_proxy_image,
                    "server",
                    server_id,
                    generation,
                    api_capability,
                    &self.runtime_api_proxy_upstream,
                    &network,
                    &self.runtime_api_proxy_egress_network,
                    self.runtime_api_proxy_allow_insecure,
                )
                .await?;
                let ports = workload_ports(self.public_address, self.signal_address, placement)?;
                self.ensure_ingress_relay(
                    server_id,
                    generation,
                    &network,
                    &name,
                    &[name.clone(), proxy_name.clone()],
                    ports,
                )
                .await?;
                return self
                    .wait_ready(server_id, server_dir, Duration::from_secs(120))
                    .await;
            }
            Some(generation) => {
                bail!(
                    "container {name} is already running generation {generation}; refusing generation {}",
                    placement.generation
                );
            }
            None => {}
        }
        self.remove_stopped(&name).await?;
        runtime_api_proxy::remove_containers_through(server_id, placement.generation).await?;
        cleanup_ingress_through(&self.ingress, server_id, placement.generation).await?;
        self.sandbox.remove_isolated_networks(server_id).await?;
        self.sandbox
            .verify_with_limits(
                &self.image,
                WorkloadLimits::server(SERVER_MEMORY_BYTES as u64, SERVER_STORAGE_BYTES as u64),
            )
            .await?;
        validate_quota_volume(server_dir, SERVER_STORAGE_BYTES as u64).await?;
        self.ingress.verify_host_policy().await?;
        tokio::fs::create_dir_all(server_dir.join("run")).await?;
        tokio::fs::write(
            server_dir.join("pumpkin.toml"),
            render_pumpkin_config(
                settings,
                25565,
                19132,
                "0.0.0.0:19132".parse()?,
                IpAddr::V4(self.gateway_bedrock_ip),
                self.gateway_bedrock_port,
            ),
        )
        .await?;
        let (runtime_uid, runtime_gid) = self.sandbox.rootless_daemon_identity().await?;
        ensure_rootless_volume_owner(server_dir, runtime_uid, runtime_gid).await?;
        persist_runtime_generation(&self.root, server_id, placement.generation).await?;
        prepare_control_socket_alias(server_id, server_dir).await?;

        let published_ports = workload_ports(self.public_address, self.signal_address, placement)?;
        let volume = format!(
            "type=bind,source={},target=/pumpkin,bind-propagation=rprivate",
            server_dir.display()
        );
        let generation_label = format!("kurbis.generation={}", placement.generation);
        let server_label = format!("kurbis.server_id={server_id}");
        let runtime_user = "0:0";
        let network =
            crate::worker::sandbox::workload_network_name(server_id, placement.generation);
        let mut args = vec![
            "run".to_owned(),
            "--detach".to_owned(),
            "--name".to_owned(),
            name.clone(),
            "--restart".to_owned(),
            "no".to_owned(),
            "--stop-timeout".to_owned(),
            "10".to_owned(),
        ];
        args.extend(self.sandbox.run_security_args(
            WorkloadLimits::server(SERVER_MEMORY_BYTES as u64, SERVER_STORAGE_BYTES as u64),
            runtime_user,
            &network,
            true,
        )?);
        let runtime_environment_args = scoped_runtime_environment_args(
            &self.worker_control_key,
            server_id,
            placement.generation,
        )?;
        args.extend([
            "--label".to_owned(),
            server_label,
            "--label".to_owned(),
            generation_label,
        ]);
        args.extend(runtime_environment_args);
        args.extend(["--mount".to_owned(), volume, self.image.clone()]);
        self.sandbox
            .create_isolated_network(server_id, placement.generation)
            .await?;
        if let Err(error) = runtime_api_proxy::start_container(
            &self.runtime_api_proxy_image,
            "server",
            server_id,
            placement.generation,
            api_capability,
            &self.runtime_api_proxy_upstream,
            &network,
            &self.runtime_api_proxy_egress_network,
            self.runtime_api_proxy_allow_insecure,
        )
        .await
        {
            let _ = self
                .sandbox
                .remove_isolated_network(server_id, placement.generation)
                .await;
            remove_runtime_generation(&self.root, server_id).await?;
            remove_control_socket_alias(server_id).await?;
            return Err(error).context("start scoped managed-plugin API proxy");
        }
        let output = match tokio::time::timeout(
            Duration::from_secs(30),
            Command::new("docker")
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output(),
        )
        .await
        {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                let cleanup = self.remove_runtime(server_id).await.err();
                if let Some(cleanup) = cleanup {
                    bail!("Docker start failed: {error}; cleanup also failed: {cleanup:#}");
                }
                return Err(error).context("start Pumpkin container");
            }
            Err(_) => {
                let cleanup = self.remove_runtime(server_id).await.err();
                if let Some(cleanup) = cleanup {
                    bail!(
                        "Docker did not start the workload within 30 seconds; cleanup also failed: {cleanup:#}"
                    );
                }
                bail!("Docker did not start the workload within 30 seconds");
            }
        };
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let cleanup = self.remove_runtime(server_id).await.err();
            if let Some(cleanup) = cleanup {
                bail!(
                    "docker failed to start {name} with status {}: {}; cleanup also failed: {cleanup:#}",
                    output.status,
                    detail,
                );
            }
            bail!(
                "docker failed to start {name} with status {}: {}",
                output.status,
                detail,
            );
        }
        if let Err(error) = self
            .sandbox
            .verify_isolated_network(&network, &[name.clone(), proxy_name.clone()])
            .await
        {
            let _ = self.remove_runtime(server_id).await;
            return Err(error).context("workload network isolation check failed");
        }
        self.ensure_ingress_relay(
            server_id,
            placement.generation,
            &network,
            &name,
            &[name.clone(), proxy_name.clone()],
            published_ports,
        )
        .await?;
        if let Err(error) = self
            .wait_ready(server_id, server_dir, Duration::from_secs(120))
            .await
        {
            let logs = Command::new("docker")
                .args(["logs", "--tail", "80", &name])
                .output()
                .await
                .ok()
                .map(|output| {
                    let mut bytes = output.stdout;
                    bytes.extend_from_slice(&output.stderr);
                    String::from_utf8_lossy(&bytes).into_owned()
                })
                .unwrap_or_else(|| "unable to read container logs".into());
            let cleanup_error = self.remove_runtime(server_id).await.err();
            if let Some(cleanup_error) = cleanup_error {
                bail!(
                    "Pumpkin readiness failed: {error}; cleanup also failed: {cleanup_error:#}; container logs:\n{logs}"
                );
            }
            bail!("Pumpkin readiness failed: {error}; container logs:\n{logs}");
        }
        Ok(())
    }

    async fn server_generation(&self, server_id: Uuid) -> Result<Option<i64>> {
        let generation = self.running_generation(&container_name(server_id)).await?;
        if generation.is_none() {
            remove_control_socket_alias(server_id).await?;
            let max_generation = read_runtime_generation(&self.root, server_id)
                .await?
                .unwrap_or(1);
            runtime_api_proxy::remove_containers_through(server_id, max_generation).await?;
            cleanup_ingress_through(&self.ingress, server_id, max_generation).await?;
            self.sandbox.remove_isolated_networks(server_id).await?;
            remove_runtime_generation(&self.root, server_id).await?;
        }
        Ok(generation)
    }

    async fn logs_since(&self, server_id: Uuid, since: &str) -> Result<Vec<String>> {
        let output = Command::new("docker")
            .args([
                "logs",
                "--since",
                since,
                "--timestamps",
                &container_name(server_id),
            ])
            .output()
            .await?;
        if !output.status.success() {
            return Ok(Vec::new());
        }
        let mut bytes = output.stdout;
        bytes.extend_from_slice(&output.stderr);
        Ok(String::from_utf8_lossy(&bytes)
            .lines()
            .map(str::to_owned)
            .collect())
    }

    async fn remove_runtime(&self, server_id: Uuid) -> Result<()> {
        let output = Command::new("docker")
            .args(["rm", "-f", &container_name(server_id)])
            .output()
            .await?;
        if output.status.success()
            || String::from_utf8_lossy(&output.stderr).contains("No such container")
        {
            remove_control_socket_alias(server_id).await?;
            let max_generation = read_runtime_generation(&self.root, server_id)
                .await?
                .unwrap_or(1);
            runtime_api_proxy::remove_containers_through(server_id, max_generation).await?;
            cleanup_ingress_through(&self.ingress, server_id, max_generation).await?;
            self.sandbox.remove_isolated_networks(server_id).await?;
            remove_runtime_generation(&self.root, server_id).await?;
            return Ok(());
        }
        bail!(
            "failed to remove Docker runtime: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

fn workload_ports(
    worker_address: IpAddr,
    signal_address: IpAddr,
    placement: &PlacementRecord,
) -> Result<PublishedWorkloadPorts> {
    Ok(PublishedWorkloadPorts {
        java_tcp: SocketAddr::new(worker_address, u16::try_from(placement.java_port)?),
        bedrock_udp: SocketAddr::new(worker_address, u16::try_from(placement.bedrock_port)?),
        bedrock_signal_tcp: SocketAddr::new(signal_address, u16::try_from(placement.signal_port)?),
    })
}

fn server_id_from_directory(server_dir: &Path) -> Result<Uuid> {
    server_dir
        .file_name()
        .and_then(|name| name.to_str())
        .context("Docker server directory has no UUID")?
        .parse()
        .context("Docker server directory has an invalid UUID")
}

fn control_socket_alias_for_server(server_id: Uuid) -> PathBuf {
    let name = format!("kurbis-docker-{}.sock", server_id.simple());
    let system_temp = std::env::temp_dir().join(&name);
    if system_temp.to_string_lossy().len() < 100 {
        return system_temp;
    }
    PathBuf::from("/tmp").join(name)
}

impl DockerRuntime {
    pub async fn verify_sandbox(&self) -> Result<()> {
        self.sandbox
            .verify_with_limits(
                &self.image,
                WorkloadLimits::server(SERVER_MEMORY_BYTES as u64, SERVER_STORAGE_BYTES as u64),
            )
            .await?;
        self.ingress
            .verify_host_policy()
            .await
            .context("Gateway-only workload ingress requires nftables host firewall access")
    }

    async fn running_generation(&self, name: &str) -> Result<Option<i64>> {
        let output = Command::new("docker")
            .args([
                "inspect",
                "--format",
                "{{if .State.Running}}{{index .Config.Labels \"kurbis.generation\"}}{{end}}",
                name,
            ])
            .output()
            .await?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            if error.contains("No such object") || error.contains("No such container") {
                return Ok(None);
            }
            bail!("cannot inspect container {name}: {}", error.trim());
        }
        let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if value.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            value
                .parse()
                .context("invalid kurbis.generation Docker label")?,
        ))
    }

    async fn remove_stopped(&self, name: &str) -> Result<()> {
        let output = Command::new("docker").args(["rm", name]).output().await?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("No such container") {
            return Ok(());
        }
        bail!("failed to remove stale container {name}: {}", stderr.trim());
    }
}

fn container_name(server_id: Uuid) -> String {
    format!("kurbis-{server_id}")
}

fn runtime_generation_path(root: &Path, server_id: Uuid) -> PathBuf {
    root.join("runtime-generations").join(server_id.to_string())
}

async fn persist_runtime_generation(root: &Path, server_id: Uuid, generation: i64) -> Result<()> {
    if generation <= 0 {
        bail!("runtime generation must be positive");
    }
    let directory = root.join("runtime-generations");
    tokio::fs::create_dir_all(&directory).await?;
    let path = runtime_generation_path(root, server_id);
    let temporary = path.with_extension("tmp");
    tokio::fs::write(&temporary, generation.to_string()).await?;
    tokio::fs::rename(temporary, path).await?;
    Ok(())
}

async fn read_runtime_generation(root: &Path, server_id: Uuid) -> Result<Option<i64>> {
    let path = runtime_generation_path(root, server_id);
    match tokio::fs::read_to_string(path).await {
        Ok(value) => Ok(Some(
            value
                .trim()
                .parse()
                .context("invalid persisted runtime generation")?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("read persisted runtime generation"),
    }
}

async fn remove_runtime_generation(root: &Path, server_id: Uuid) -> Result<()> {
    for path in [
        runtime_generation_path(root, server_id),
        root.join("runtime-generations")
            .join(format!("{server_id}.tmp")),
    ] {
        match tokio::fs::remove_file(path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

async fn prepare_control_socket_alias(server_id: Uuid, server_dir: &Path) -> Result<()> {
    let socket = server_dir.join("run/host-control.sock");
    match tokio::fs::remove_file(&socket).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("remove stale Docker host-control socket"),
    }

    let alias = control_socket_alias_for_server(server_id);
    match tokio::fs::symlink_metadata(&alias).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            tokio::fs::remove_file(&alias)
                .await
                .context("remove stale Docker host-control socket alias")?;
        }
        Ok(_) => bail!(
            "refusing to replace non-symlink Docker host-control socket alias {}",
            alias.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect Docker host-control socket alias"),
    }

    tokio::fs::symlink(&socket, &alias)
        .await
        .context("create Docker host-control socket alias")?;
    Ok(())
}

async fn remove_control_socket_alias(server_id: Uuid) -> Result<()> {
    match tokio::fs::remove_file(control_socket_alias_for_server(server_id)).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("remove Docker host-control socket alias"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_has_no_player_limit_and_escapes_strings() {
        let config = render_pumpkin_config(
            &json!({"motd":"hello \"kurbis\"","view_distance":8}),
            25565,
            19132,
            "0.0.0.0:19132".parse().unwrap(),
            "192.0.2.20".parse().unwrap(),
            19132,
        );
        assert!(config.contains("max_players = 0"));
        assert!(config.contains("motd = \"hello \\\"kurbis\\\"\""));
        assert!(config.contains("view_distance = 8"));
    }

    #[tokio::test]
    async fn runtime_capability_is_not_persisted_under_the_mounted_server_volume() {
        let server_id = Uuid::new_v4();
        let root = tempfile::tempdir().unwrap();
        let server_dir = root.path().join("servers").join(server_id.to_string());
        tokio::fs::create_dir_all(server_dir.join("run"))
            .await
            .unwrap();
        persist_runtime_generation(root.path(), server_id, 11)
            .await
            .unwrap();

        let environment = scoped_runtime_environment_args(
            "worker-root-key-that-is-long-enough-for-testing",
            server_id,
            11,
        )
        .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(runtime_generation_path(root.path(), server_id))
                .await
                .unwrap(),
            "11"
        );
        assert!(!server_dir.join("run/host-control.env").exists());
        assert!(!environment.iter().any(|argument| argument == "--env-file"));
        assert!(environment[3].starts_with("PUMPKIN_HOST_CONTROL_TOKEN="));
        remove_runtime_generation(root.path(), server_id)
            .await
            .unwrap();
    }

    #[test]
    fn docker_host_control_socket_uses_a_short_alias_into_the_quota_volume() {
        let server_id = Uuid::new_v4();
        let server_dir = PathBuf::from(format!(
            "/opt/melon-hoster-verification/melon-runtime-long-root/worker-0/servers/{server_id}"
        ));
        let socket = control_socket_alias_for_server(server_id);
        assert!(socket.to_string_lossy().len() < 100);
        assert!(socket.ends_with(format!("kurbis-docker-{}.sock", server_id.simple())));
        assert_eq!(
            server_dir.join("run/host-control.sock"),
            server_dir.join("run").join("host-control.sock")
        );
    }

    #[tokio::test]
    async fn docker_host_control_alias_points_inside_server_storage() {
        let server_id = Uuid::new_v4();
        let root = tempfile::tempdir().unwrap();
        let server_dir = root.path().join(server_id.to_string());
        tokio::fs::create_dir_all(server_dir.join("run"))
            .await
            .unwrap();

        prepare_control_socket_alias(server_id, &server_dir)
            .await
            .unwrap();
        let alias = control_socket_alias_for_server(server_id);
        assert_eq!(
            tokio::fs::read_link(&alias).await.unwrap(),
            server_dir.join("run/host-control.sock")
        );
        tokio::fs::remove_file(alias).await.unwrap();
    }
}
