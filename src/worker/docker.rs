use std::{
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
}

impl DockerRuntime {
    pub fn new(image: String, host_control_token: String) -> Self {
        Self {
            image,
            host_control: PumpkinControl::new(host_control_token),
        }
    }
}

#[async_trait]
impl ServerRuntime for DockerRuntime {
    fn control(&self) -> &PumpkinControl {
        &self.host_control
    }
    fn socket_path(&self, server_dir: &Path) -> Result<PathBuf> {
        Ok(server_dir.join("run/host-control.sock"))
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
            render_pumpkin_config(settings, 25565, 19132),
        )
        .await?;
        let (runtime_uid, runtime_gid) = ensure_pumpkin_ownership(server_dir).await?;

        let java = format!("{}:25565/tcp", placement.java_port);
        let bedrock_udp = format!("{}:19132/udp", placement.bedrock_port);
        let volume = format!("{}:/pumpkin", server_dir.display());
        let generation_label = format!("kurbis.generation={}", placement.generation);
        let lease_label = format!("kurbis.lease={}", placement.lease_token);
        let server_label = format!("kurbis.server_id={server_id}");
        let memory = SERVER_MEMORY_BYTES.to_string();
        let runtime_user = format!("{runtime_uid}:{runtime_gid}");
        let status = Command::new("docker")
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
                "--volume",
                &volume,
                &self.image,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status()
            .await?;
        if !status.success() {
            bail!("docker failed to start {name} with status {status}");
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
        self.running_generation(&container_name(server_id)).await
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

    async fn abort_start(&self, server_id: Uuid) -> Result<()> {
        let output = Command::new("docker")
            .args(["rm", "-f", &container_name(server_id)])
            .output()
            .await?;
        if output.status.success()
            || String::from_utf8_lossy(&output.stderr).contains("No such container")
        {
            return Ok(());
        }
        bail!(
            "failed to remove failed-start container: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
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
}
