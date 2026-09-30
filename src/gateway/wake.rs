use std::time::Duration;

use anyhow::{Result, bail};

use crate::{config::GatewayConfig, gateway::routes::normalize_requested_hostname};

pub async fn request_wake(config: &GatewayConfig, hostname: &str) -> Result<()> {
    let hostname = normalize_requested_hostname(hostname)
        .ok_or_else(|| anyhow::anyhow!("invalid hostname"))?;
    let url = format!(
        "{}/internal/wake/{hostname}",
        config.control_plane_url.trim_end_matches('/'),
    );
    let response = reqwest::Client::new()
        .post(url)
        .bearer_auth(&config.internal_token)
        .timeout(Duration::from_secs(10))
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("control plane rejected wake with {}", response.status());
    }
    Ok(())
}
