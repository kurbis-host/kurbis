use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::{
    process::Command,
    time::{sleep, timeout},
};
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

    pub async fn start(
        &self,
        server_id: Uuid,
        placement: &PlacementRecord,
        server_dir: &Path,
        settings: &Value,
    ) -> Result<()> {
        let name = container_name(server_id);
        match self.running_generation(&name).await? {
            Some(generation) if generation == placement.generation => {
                return self.wait_ready(server_dir, Duration::from_secs(120)).await;
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
            render_pumpkin_config(settings),
        )
        .await?;
        ensure_pumpkin_ownership(server_dir).await?;

        let java = format!("{}:25565/tcp", placement.java_port);
        let bedrock_udp = format!("{}:19132/udp", placement.bedrock_port);
        let bedrock_tcp = format!("{}:19132/tcp", placement.bedrock_port);
        let volume = format!("{}:/pumpkin", server_dir.display());
        let generation_label = format!("melon.generation={}", placement.generation);
        let lease_label = format!("melon.lease={}", placement.lease_token);
        let server_label = format!("melon.server_id={server_id}");
        let memory = SERVER_MEMORY_BYTES.to_string();
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
                &bedrock_tcp,
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
        self.wait_ready(server_dir, Duration::from_secs(120)).await
    }

    pub async fn player_count(&self, server_dir: &Path) -> Result<usize> {
        self.host_control.status(&socket_path(server_dir)).await
    }

    pub async fn save(&self, server_dir: &Path) -> Result<()> {
        self.host_control.save(&socket_path(server_dir)).await
    }

    pub async fn graceful_stop(&self, server_id: Uuid, server_dir: &Path) -> Result<()> {
        let name = container_name(server_id);
        self.host_control.shutdown(&socket_path(server_dir)).await?;
        let wait = async {
            loop {
                if !self.is_running(&name).await? {
                    return Ok::<(), anyhow::Error>(());
                }
                sleep(Duration::from_millis(250)).await;
            }
        };
        timeout(Duration::from_secs(60), wait)
            .await
            .context("Pumpkin did not exit within 60 seconds")??;
        Ok(())
    }

    pub async fn is_running(&self, name: &str) -> Result<bool> {
        Ok(self.running_generation(name).await?.is_some())
    }

    pub async fn server_generation(&self, server_id: Uuid) -> Result<Option<i64>> {
        self.running_generation(&container_name(server_id)).await
    }

    pub async fn server_is_running(&self, server_id: Uuid) -> Result<bool> {
        self.is_running(&container_name(server_id)).await
    }

    pub async fn logs_since(&self, server_id: Uuid, since: &str) -> Result<Vec<String>> {
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

    pub async fn abort_start(&self, server_id: Uuid) -> Result<()> {
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

    async fn wait_ready(&self, server_dir: &Path, max_wait: Duration) -> Result<()> {
        let started = tokio::time::Instant::now();
        loop {
            match self.host_control.status(&socket_path(server_dir)).await {
                Ok(_) => return Ok(()),
                Err(error) if started.elapsed() < max_wait => {
                    tracing::debug!(?error, "waiting for Pumpkin host-control readiness");
                    sleep(Duration::from_millis(500)).await;
                }
                Err(error) => return Err(error).context("Pumpkin did not become ready"),
            }
        }
    }

    async fn running_generation(&self, name: &str) -> Result<Option<i64>> {
        let output = Command::new("docker")
            .args([
                "inspect",
                "--format",
                "{{if .State.Running}}{{index .Config.Labels \"melon.generation\"}}{{end}}",
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
                .context("invalid melon.generation Docker label")?,
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

fn socket_path(server_dir: &Path) -> PathBuf {
    server_dir.join("run/host-control.sock")
}

fn container_name(server_id: Uuid) -> String {
    format!("melon-{server_id}")
}

fn render_pumpkin_config(settings: &Value) -> String {
    let motd = setting_str(settings, "motd", "Hosted by Melon");
    let difficulty = setting_str(settings, "difficulty", "Normal");
    let gamemode = setting_str(settings, "gamemode", "Survival");
    let view_distance = setting_u64(settings, "view_distance", 12).clamp(2, 32);
    let simulation_distance = setting_u64(settings, "simulation_distance", 8).clamp(2, 32);
    let hardcore = setting_bool(settings, "hardcore", false);
    let whitelist = setting_bool(settings, "whitelist", false);
    format!(
        "default_difficulty = {difficulty}\n\
         default_gamemode = {gamemode}\n\
         hardcore = {hardcore}\n\
         white_list = {whitelist}\n\
         enforce_whitelist = {whitelist}\n\
         accepts_transfers = true\n\
         [networking.java]\n\
         enabled = true\n\
         address = \"0.0.0.0:25565\"\n\
         max_players = 0\n\
         motd = {motd}\n\
         view_distance = {view_distance}\n\
         simulation_distance = {simulation_distance}\n\
         [networking.bedrock]\n\
         enabled = true\n\
         max_players = 0\n\
         motd = {motd}\n\
         view_distance = {view_distance}\n\
         simulation_distance = {simulation_distance}\n\
         [networking.bedrock.nethernet]\n\
         enabled = true\n\
         address = \"0.0.0.0:19132\"\n",
        difficulty = serde_json::to_string(difficulty).expect("string serialization cannot fail"),
        gamemode = serde_json::to_string(gamemode).expect("string serialization cannot fail"),
        motd = serde_json::to_string(motd).expect("string serialization cannot fail"),
    )
}

fn setting_str<'a>(settings: &'a Value, key: &str, default: &'a str) -> &'a str {
    settings.get(key).and_then(Value::as_str).unwrap_or(default)
}

fn setting_u64(settings: &Value, key: &str, default: u64) -> u64 {
    settings.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn setting_bool(settings: &Value, key: &str, default: bool) -> bool {
    settings
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

async fn ensure_pumpkin_ownership(server_dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let status = Command::new("chown")
            .arg("-R")
            .arg("2613:2613")
            .arg(server_dir)
            .status()
            .await?;
        if !status.success() {
            bail!("failed to set Pumpkin volume ownership");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_has_no_player_limit_and_escapes_strings() {
        let config = render_pumpkin_config(&json!({"motd":"hello \"melon\"","view_distance":8}));
        assert!(config.contains("max_players = 0"));
        assert!(config.contains("motd = \"hello \\\"melon\\\"\""));
        assert!(config.contains("view_distance = 8"));
    }
}
