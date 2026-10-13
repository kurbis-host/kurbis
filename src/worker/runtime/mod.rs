use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

use crate::{
    model::PlacementRecord,
    pumpkin_control::{
        HostStats, InstalledPluginFile, PlayerStatus, PumpkinControl, WaitingPlayer, WhitelistEntry,
    },
    runtime_stats::RuntimeStats,
};

pub mod native;
pub mod settings;

fn runtime_stats_from_host(stats: HostStats, generation: i64) -> RuntimeStats {
    RuntimeStats {
        generation,
        cpu_percent: stats.cpu_percent,
        cpu_limit_millis: stats.cpu_limit_millis,
        memory_used_bytes: stats.memory_used_bytes,
        memory_limit_bytes: stats.memory_limit_bytes,
        disk_used_bytes: stats.disk_used_bytes,
        disk_limit_bytes: stats.disk_limit_bytes,
        tps: stats.tps,
        measured_at: chrono::Utc::now(),
    }
}

/// Lifecycle and persistence use the same contract for containers and host processes.
#[async_trait]
pub trait ServerRuntime: Send + Sync {
    async fn control_for(&self, server_dir: &Path) -> Result<PumpkinControl>;
    fn socket_path(&self, server_dir: &Path) -> Result<PathBuf>;
    async fn start(
        &self,
        server_id: Uuid,
        placement: &PlacementRecord,
        server_dir: &Path,
        settings: &Value,
        api_capability: Option<&str>,
    ) -> Result<()>;
    async fn server_generation(&self, server_id: Uuid) -> Result<Option<i64>>;
    async fn logs_since(&self, server_id: Uuid, since: &str) -> Result<Vec<String>>;
    async fn remove_runtime(&self, server_id: Uuid) -> Result<()>;

    async fn server_is_running(&self, server_id: Uuid) -> Result<bool> {
        Ok(self.server_generation(server_id).await?.is_some())
    }

    async fn runtime_stats(
        &self,
        server_id: Uuid,
        server_dir: &Path,
        generation: i64,
    ) -> Result<RuntimeStats> {
        ensure!(generation > 0, "runtime generation must be positive");
        let runtime_generation = self.server_generation(server_id).await?;
        if runtime_generation != Some(generation) {
            bail!(
                "runtime generation {runtime_generation:?} does not match stats assignment generation {generation}"
            );
        }
        let stats = self
            .control_for(server_dir)
            .await?
            .stats(&self.socket_path(server_dir)?)
            .await?;
        Ok(runtime_stats_from_host(stats, generation))
    }

    async fn player_status(&self, server_dir: &Path) -> Result<PlayerStatus> {
        self.control_for(server_dir)
            .await?
            .status(&self.socket_path(server_dir)?)
            .await
    }

    async fn players(&self, server_dir: &Path) -> Result<Vec<WaitingPlayer>> {
        self.control_for(server_dir)
            .await?
            .players(&self.socket_path(server_dir)?)
            .await
    }

    async fn replace_whitelist(
        &self,
        server_dir: &Path,
        enabled: bool,
        entries: &[WhitelistEntry],
    ) -> Result<()> {
        self.control_for(server_dir)
            .await?
            .replace_whitelist(&self.socket_path(server_dir)?, enabled, entries)
            .await
    }

    async fn replace_operators(&self, server_dir: &Path, owners: &[WhitelistEntry]) -> Result<()> {
        self.control_for(server_dir)
            .await?
            .replace_operators(&self.socket_path(server_dir)?, owners)
            .await
    }

    async fn sync_plugins(&self, server_dir: &Path) -> Result<Vec<InstalledPluginFile>> {
        self.control_for(server_dir)
            .await?
            .sync_plugins(&self.socket_path(server_dir)?)
            .await
    }

    async fn execute_command(&self, server_dir: &Path, command: &str) -> Result<Vec<String>> {
        self.control_for(server_dir)
            .await?
            .execute_command(&self.socket_path(server_dir)?, command)
            .await
    }

    async fn save(&self, server_dir: &Path) -> Result<()> {
        self.control_for(server_dir)
            .await?
            .save(&self.socket_path(server_dir)?)
            .await
    }

    async fn graceful_stop(&self, server_id: Uuid, server_dir: &Path) -> Result<()> {
        self.control_for(server_dir)
            .await?
            .shutdown(&self.socket_path(server_dir)?)
            .await?;
        timeout(Duration::from_secs(60), async {
            while self.server_is_running(server_id).await? {
                sleep(Duration::from_millis(250)).await;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await
        .context("Pumpkin did not exit within 60 seconds")??;
        self.remove_runtime(server_id).await
    }

    async fn wait_ready(
        &self,
        server_id: Uuid,
        server_dir: &Path,
        max_wait: Duration,
    ) -> Result<()> {
        let mut last_error = None;
        let result = timeout(max_wait, async {
            loop {
                match self.player_status(server_dir).await {
                    Ok(_) => return Ok(()),
                    Err(error) => {
                        last_error = Some(error.to_string());
                        if !self.server_is_running(server_id).await? {
                            return Err(error).context("Pumpkin exited before readiness");
                        }
                        tracing::debug!(?error, "waiting for Pumpkin host-control readiness");
                        // Pumpkin is ready a few hundred milliseconds after it
                        // is spawned; a coarser poll would dominate that.
                        sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        })
        .await;
        match result {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!(readiness_timeout_message(
                last_error.as_deref()
            ))),
        }
    }
}

fn readiness_timeout_message(last_error: Option<&str>) -> String {
    let Some(error) = last_error else {
        return "Pumpkin readiness deadline exceeded".to_owned();
    };
    let bounded: String = error.chars().take(512).collect();
    format!("Pumpkin readiness deadline exceeded; last host-control error: {bounded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_stats_use_the_worker_assignment_generation_and_worker_timestamp() {
        let before = chrono::Utc::now();
        let stats = runtime_stats_from_host(
            HostStats {
                cpu_percent: Some(12.5),
                cpu_limit_millis: Some(500),
                memory_used_bytes: Some(100),
                memory_limit_bytes: None,
                disk_used_bytes: Some(200),
                disk_limit_bytes: Some(300),
                tps: Some(20.0),
            },
            7,
        );
        let after = chrono::Utc::now();
        assert_eq!(stats.generation, 7);
        assert_eq!(stats.cpu_percent, Some(12.5));
        assert!(stats.measured_at >= before && stats.measured_at <= after);
    }

    #[test]
    fn readiness_timeout_reports_bounded_host_control_error() {
        let message = readiness_timeout_message(Some("host-control connection timed out"));
        assert_eq!(
            message,
            "Pumpkin readiness deadline exceeded; last host-control error: host-control connection timed out"
        );

        let message = readiness_timeout_message(Some(&"x".repeat(600)));
        assert!(message.len() < 600);
        assert!(
            message.starts_with("Pumpkin readiness deadline exceeded; last host-control error: ")
        );
    }
}
