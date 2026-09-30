//! Confirm activation through the companion's official Pumpkin console commands.
use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use tokio::{fs, time};
use uuid::Uuid;
use vibe_protocol::{now_ms, write_json};

use crate::{paths::RuntimePaths, progress::Recorder};

#[derive(Deserialize)]
struct Ready {
    updated_unix_ms: u128,
}
#[derive(Deserialize)]
struct Response {
    ok: bool,
    message: String,
    events: Vec<String>,
}

pub async fn reload(paths: &RuntimePaths, plugin: &str, job: Uuid) -> anyhow::Result<String> {
    let root = paths.bridge_data().join("activation");
    let ready: Ready = serde_json::from_slice(
        &fs::read(root.join("ready.json"))
            .await
            .context("Server runner unavailable; start with scripts/run.sh")?,
    )?;
    // A console transaction can take up to 40 seconds. The runner updates its
    // heartbeat between transactions; do not mistake a busy runner for a dead one.
    anyhow::ensure!(
        now_ms().saturating_sub(ready.updated_unix_ms) < 60_000,
        "Server runner unavailable; start the server with scripts/run.sh"
    );
    let id = Uuid::new_v4();
    let request = root.join("requests").join(format!("{id}.json"));
    let result = root.join("results").join(format!("{id}.json"));
    write_json(&request, &serde_json::json!({"plugin": plugin}))?;
    let mut recorder = Recorder::new(&paths.bridge_data(), job)?;
    recorder.step(&format!(
        "Reloading {plugin} through the official server console"
    ))?;
    let outcome = time::timeout(Duration::from_secs(45), async {
        loop {
            if fs::try_exists(&result).await? {
                let response: Response = serde_json::from_slice(&fs::read(&result).await?)
                    .context("invalid activation response")?;
                for event in response.events {
                    recorder.step(&event)?;
                }
                fs::remove_file(&result).await?;
                anyhow::ensure!(response.ok, "{}", response.message);
                return Ok(response.message);
            }
            time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    let _ = fs::remove_file(&request).await;
    outcome.context("Timed out waiting for server reload")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stale_runner_is_rejected_before_any_console_request() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = RuntimePaths::new(temporary.path());
        let activation = paths.bridge_data().join("activation");
        write_json(
            &activation.join("ready.json"),
            &serde_json::json!({"updated_unix_ms": 0}),
        )
        .unwrap();
        let error = reload(&paths, "demo", Uuid::new_v4()).await.unwrap_err();
        assert!(error.to_string().contains("Server runner unavailable"));
        assert!(!activation.join("requests").exists());
    }
}
