//! Pushes operator alerts to an ntfy topic.
//!
//! Without a live worker no server can start, and nothing else tells anyone:
//! the control plane stays healthy and the panel only shows it to someone who
//! looks. This sends one alert when every worker has been silent for a while,
//! and one more when a worker is back.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio::sync::Mutex;

use crate::config::AlertConfig;

/// Every worker silent for this long counts as an outage. Long enough that a
/// release, which restarts the worker, never raises one.
pub const WORKERS_SILENT_SECONDS: i64 = 180;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fleet {
    /// No worker is registered, so there is nothing to miss.
    Empty,
    Live,
    Silent {
        since: DateTime<Utc>,
    },
}

#[derive(Debug, PartialEq, Eq)]
struct Alert {
    title: &'static str,
    message: String,
    priority: &'static str,
    tags: &'static str,
}

/// What to send when the fleet is now `fleet`, given whether an outage was
/// already reported.
fn alert_for(fleet: Fleet, reported: bool) -> Option<Alert> {
    match (fleet, reported) {
        (Fleet::Silent { since }, false) => Some(Alert {
            title: "Kurbis: no worker is running",
            message: format!(
                "No worker has sent a heartbeat since {} UTC. Servers cannot start until one is back.",
                since.format("%Y-%m-%d %H:%M")
            ),
            priority: "urgent",
            tags: "rotating_light",
        }),
        (Fleet::Live, true) => Some(Alert {
            title: "Kurbis: a worker is back",
            message: "A worker is sending heartbeats again; servers can start.".into(),
            priority: "default",
            tags: "white_check_mark",
        }),
        _ => None,
    }
}

#[derive(Clone)]
pub struct WorkerAlerts {
    pool: PgPool,
    http: reqwest::Client,
    config: AlertConfig,
    reported: Arc<Mutex<bool>>,
}

impl WorkerAlerts {
    pub fn new(pool: PgPool, config: AlertConfig) -> Result<Self> {
        Ok(Self {
            pool,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            config,
            reported: Arc::new(Mutex::new(false)),
        })
    }

    async fn fleet(&self) -> Result<Fleet> {
        let (workers, last_heartbeat, silent): (i64, Option<DateTime<Utc>>, bool) = sqlx::query_as(
            "SELECT count(*), max(heartbeat_at),
                        coalesce(max(heartbeat_at) < now() - make_interval(secs => $1), false)
                 FROM workers",
        )
        .bind(WORKERS_SILENT_SECONDS as f64)
        .fetch_one(&self.pool)
        .await?;
        Ok(match last_heartbeat {
            Some(since) if silent => Fleet::Silent { since },
            _ if workers == 0 => Fleet::Empty,
            _ => Fleet::Live,
        })
    }

    /// Check the fleet once and send what changed. A failed send is retried
    /// on the next check, because the reported state only moves once it went out.
    pub async fn run_once(&self) -> Result<()> {
        let fleet = self.fleet().await?;
        let mut reported = self.reported.lock().await;
        let Some(alert) = alert_for(fleet, *reported) else {
            return Ok(());
        };
        self.send(&alert).await?;
        tracing::warn!(title = alert.title, "operator alert sent");
        *reported = matches!(fleet, Fleet::Silent { .. });
        Ok(())
    }

    async fn send(&self, alert: &Alert) -> Result<()> {
        let mut request = self
            .http
            .post(&self.config.ntfy_url)
            .header("Title", alert.title)
            .header("Priority", alert.priority)
            .header("Tags", alert.tags)
            .body(alert.message.clone());
        if let Some(token) = &self.config.ntfy_token {
            request = request.bearer_auth(token.expose());
        }
        let response = request.send().await.context("send ntfy alert")?;
        let status = response.status();
        ensure!(
            status.is_success(),
            "ntfy refused the alert with HTTP {status}"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outage_is_reported_once_and_its_end_once() {
        let since = DateTime::parse_from_rfc3339("2026-10-29T19:14:16Z")
            .unwrap()
            .with_timezone(&Utc);
        let silent = Fleet::Silent { since };
        let alert = alert_for(silent, false).unwrap();
        assert_eq!(alert.priority, "urgent");
        assert!(
            alert.message.contains("2026-10-29 19:14 UTC"),
            "{}",
            alert.message
        );
        assert_eq!(alert_for(silent, true), None);
        assert_eq!(alert_for(Fleet::Live, true).unwrap().priority, "default");
        assert_eq!(alert_for(Fleet::Live, false), None);
        assert_eq!(alert_for(Fleet::Empty, false), None);
        // Removing the last worker is not an outage worth a recovery note.
        assert_eq!(alert_for(Fleet::Empty, true), None);
    }
}
