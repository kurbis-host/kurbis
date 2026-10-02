use std::{
    net::{IpAddr, SocketAddr},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use super::runtime::{ServerRuntime, settings::render_pumpkin_config};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;
use uuid::Uuid;

use crate::{
    model::{PlacementRecord, SERVER_MEMORY_BYTES},
    pumpkin_control::PumpkinControl,
};

#[derive(Clone)]
pub struct DockerRuntime {
    image: String,
    host_control: PumpkinControl,
    public_address: IpAddr,
    signal_address: IpAddr,
}

impl DockerRuntime {
    pub fn new(
        image: String,
        host_control_token: String,
        public_address: IpAddr,
        signal_address: IpAddr,
    ) -> Self {
        Self {
            image,
            host_control: PumpkinControl::new(host_control_token),
            public_address,
            signal_address,
        }
    }
}

#[async_trait]
impl ServerRuntime for DockerRuntime {
    fn control(&self) -> &PumpkinControl {
        &self.host_control
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
    ) -> Result<()> {
        let name = container_name(server_id);
        match self.running_generation(&name).await? {
            Some(generation) if generation == placement.generation => {
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
        tokio::fs::create_dir_all(server_dir.join("run")).await?;
        tokio::fs::write(
            server_dir.join("pumpkin.toml"),
            render_pumpkin_config(
                settings,
                25565,
                19132,
                "0.0.0.0:19132".parse()?,
                self.public_address,
                u16::try_from(placement.bedrock_port)?,
            ),
        )
        .await?;
        let (runtime_uid, runtime_gid) = ensure_pumpkin_ownership(server_dir).await?;
        prepare_control_socket_alias(server_id, server_dir).await?;

        let java = format!("{}:25565/tcp", placement.java_port);
        let bedrock_udp = format!("{}:19132/udp", placement.bedrock_port);
        let signal_host =
            SocketAddr::new(self.signal_address, u16::try_from(placement.signal_port)?);
        let bedrock_signal = format!("{signal_host}:19132/tcp");
        let volume = format!("{}:/pumpkin", server_dir.display());
        let generation_label = format!("kurbis.generation={}", placement.generation);
        let lease_label = format!("kurbis.lease={}", placement.lease_token);
        let server_label = format!("kurbis.server_id={server_id}");
        let memory = SERVER_MEMORY_BYTES.to_string();
        let runtime_user = format!("{runtime_uid}:{runtime_gid}");
        let output = Command::new("docker")
            .args([
                "run",
                "--detach",
                "--name",
                &name,
                "--restart",
                "no",
                "--memory",
                &memory,
                "--memory-swap",
                &memory,
                "--cpus",
                "1.0",
                "--pids-limit",
                "512",
                "--user",
                &runtime_user,
                "--read-only",
                "--security-opt",
                "no-new-privileges:true",
                "--cap-drop",
                "ALL",
                "--tmpfs",
                "/tmp:rw,noexec,nosuid,size=64m",
                "--label",
                &server_label,
                "--label",
                &generation_label,
                "--label",
                &lease_label,
                "--env",
                "PUMPKIN_HOST_CONTROL_SOCKET=/pumpkin/run/host-control.sock",
                "--env",
                &format!("PUMPKIN_HOST_CONTROL_TOKEN={}", self.host_control.token()),
                "--publish",
                &java,
                "--publish",
                &bedrock_udp,
                "--publish",
                &bedrock_signal,
                "--volume",
                &volume,
                &self.image,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await?;
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr);
            bail!(
                "docker failed to start {name} with status {}: {}",
                output.status,
                detail.trim()
            );
        }
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
            bail!("Pumpkin readiness failed: {error}; container logs:\n{logs}");
        }
        Ok(())
    }

    async fn server_generation(&self, server_id: Uuid) -> Result<Option<i64>> {
        let generation = self.running_generation(&container_name(server_id)).await?;
        if generation.is_none() {
            remove_control_socket_alias(server_id).await?;
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
            return Ok(());
        }
        bail!(
            "failed to remove Docker runtime: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
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

async fn ensure_pumpkin_ownership(server_dir: &Path) -> Result<(u32, u32)> {
    let metadata = tokio::fs::metadata(server_dir).await?;
    let (uid, gid) = (metadata.uid(), metadata.gid());
    if uid == 0 {
        let status = Command::new("chown")
            .arg("-R")
            .arg("2613:2613")
            .arg(server_dir)
            .status()
            .await?;
        if !status.success() {
            bail!("failed to set Pumpkin volume ownership");
        }
        return Ok(container_identity(uid, gid));
    }
    Ok(container_identity(uid, gid))
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

fn container_identity(uid: u32, gid: u32) -> (u32, u32) {
    if uid == 0 { (2613, 2613) } else { (uid, gid) }
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

    #[test]
    fn unprivileged_worker_keeps_server_files_owned_by_its_uid() {
        assert_eq!(container_identity(501, 20), (501, 20));
    }

    #[test]
    fn root_worker_uses_the_pumpkin_image_user() {
        assert_eq!(container_identity(0, 0), (2613, 2613));
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
