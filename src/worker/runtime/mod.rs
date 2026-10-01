use anyhow::{Context, Result};
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
    pumpkin_control::{PlayerStatus, PumpkinControl, WaitingPlayer, WhitelistEntry},
};

pub mod native;
pub mod settings;

/// Lifecycle and persistence use the same contract for containers and host processes.
#[async_trait]
pub trait ServerRuntime: Send + Sync {
    fn control(&self) -> &PumpkinControl;
    fn socket_path(&self, server_dir: &Path) -> Result<PathBuf>;
    async fn start(
        &self,
        server_id: Uuid,
        placement: &PlacementRecord,
        server_dir: &Path,
        settings: &Value,
    ) -> Result<()>;
    async fn server_generation(&self, server_id: Uuid) -> Result<Option<i64>>;
    async fn logs_since(&self, server_id: Uuid, since: &str) -> Result<Vec<String>>;
    async fn abort_start(&self, server_id: Uuid) -> Result<()>;

    async fn server_is_running(&self, server_id: Uuid) -> Result<bool> {
        Ok(self.server_generation(server_id).await?.is_some())
    }

    async fn player_status(&self, server_dir: &Path) -> Result<PlayerStatus> {
        self.control().status(&self.socket_path(server_dir)?).await
    }

    async fn players(&self, server_dir: &Path) -> Result<Vec<WaitingPlayer>> {
        self.control().players(&self.socket_path(server_dir)?).await
    }

    async fn replace_whitelist(
        &self,
        server_dir: &Path,
        enabled: bool,
        entries: &[WhitelistEntry],
    ) -> Result<()> {
        self.control()
            .replace_whitelist(&self.socket_path(server_dir)?, enabled, entries)
            .await
    }

    async fn replace_operators(&self, server_dir: &Path, owners: &[WhitelistEntry]) -> Result<()> {
        self.control()
            .replace_operators(&self.socket_path(server_dir)?, owners)
            .await
    }

    async fn save(&self, server_dir: &Path) -> Result<()> {
        self.control().save(&self.socket_path(server_dir)?).await
    }

    async fn graceful_stop(&self, server_id: Uuid, server_dir: &Path) -> Result<()> {
        self.control()
            .shutdown(&self.socket_path(server_dir)?)
            .await?;
        timeout(Duration::from_secs(60), async {
            while self.server_is_running(server_id).await? {
                sleep(Duration::from_millis(250)).await;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await
        .context("Pumpkin did not exit within 60 seconds")?
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
                        sleep(Duration::from_millis(500)).await;
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
    use super::readiness_timeout_message;

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
