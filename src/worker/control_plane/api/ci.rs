use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::super::WorkerControl;
use crate::ci::CiRuntimePlacement;

#[derive(Debug, Deserialize)]
struct AssignmentResponse {
    runtimes: Vec<CiRuntimePlacement>,
}
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct Fence {
    generation: i64,
    lease_token: Uuid,
}
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct CapabilityRegistration {
    generation: i64,
    lease_token: Uuid,
    capability: String,
}
impl WorkerControl {
    pub async fn ci_runtime_assignments(&self) -> Result<Vec<CiRuntimePlacement>> {
        Ok(self
            .get_json::<AssignmentResponse>("/internal/worker/control/ci/runtimes")
            .await?
            .runtimes)
    }

    pub async fn ci_runtime_register_capability(
        &self,
        placement: &CiRuntimePlacement,
        capability: &str,
    ) -> Result<()> {
        if capability.len() != 43
            || !capability
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            bail!("CI runtime capability has invalid format");
        }
        self.post_no_content(
            &format!(
                "/internal/worker/control/ci/runtimes/{}/capability",
                placement.preview_id
            ),
            &CapabilityRegistration {
                generation: placement.generation,
                lease_token: placement.lease_token,
                capability: capability.to_owned(),
            },
        )
        .await
    }

    pub async fn ci_runtime_started(&self, placement: &CiRuntimePlacement) -> Result<()> {
        self.post_no_content(
            &format!(
                "/internal/worker/control/ci/runtimes/{}/started",
                placement.preview_id
            ),
            &Fence {
                generation: placement.generation,
                lease_token: placement.lease_token,
            },
        )
        .await
    }

    pub async fn ci_runtime_start_failed(&self, placement: &CiRuntimePlacement) -> Result<()> {
        self.post_no_content(
            &format!(
                "/internal/worker/control/ci/runtimes/{}/start-failed",
                placement.preview_id
            ),
            &Fence {
                generation: placement.generation,
                lease_token: placement.lease_token,
            },
        )
        .await
    }

    pub async fn ci_runtime_stopped(&self, placement: &CiRuntimePlacement) -> Result<()> {
        self.post_no_content(
            &format!(
                "/internal/worker/control/ci/runtimes/{}/stopped",
                placement.preview_id
            ),
            &Fence {
                generation: placement.generation,
                lease_token: placement.lease_token,
            },
        )
        .await
    }

    pub async fn ci_runtime_deleted(&self, placement: &CiRuntimePlacement) -> Result<()> {
        self.post_no_content(
            &format!(
                "/internal/worker/control/ci/runtimes/{}/deleted",
                placement.preview_id
            ),
            &Fence {
                generation: placement.generation,
                lease_token: placement.lease_token,
            },
        )
        .await
    }

    pub async fn download_ci_artifact(&self, placement: &CiRuntimePlacement) -> Result<Vec<u8>> {
        let mut url = self.base_url().clone();
        {
            let mut segments = url.path_segments_mut().map_err(|_| {
                anyhow::anyhow!("control-plane URL cannot accept CI artifact route")
            })?;
            segments.pop_if_empty();
            segments.extend([
                "internal",
                "worker",
                "control",
                "ci",
                "runtimes",
                &placement.preview_id.to_string(),
                "artifacts",
                &placement.generation.to_string(),
                &placement.lease_token.to_string(),
            ]);
        }
        let response = self
            .authorized_request(reqwest::Method::GET, url)?
            .send()
            .await
            .context("request assigned CI artifact")?;
        if !response.status().is_success() {
            bail!(
                "control plane rejected CI artifact download ({})",
                response.status()
            );
        }
        let bytes = response
            .bytes()
            .await
            .context("read CI artifact response")?;
        if bytes.len() as i64 > 1024 * 1024 * 1024 {
            bail!("CI artifact exceeds worker transfer limit");
        }
        if blake3::hash(&bytes).to_hex().as_str() != placement.artifact_digest {
            bail!("downloaded CI artifact digest does not match its assigned digest");
        }
        Ok(bytes.to_vec())
    }
}

impl WorkerControl {
    pub async fn ci_runtime_report_players(
        &self,
        placement: &CiRuntimePlacement,
        players: &[serde_json::Value],
    ) -> Result<()> {
        #[derive(Serialize)]
        struct PlayerReport<'a> {
            generation: i64,
            lease_token: Uuid,
            players: &'a [serde_json::Value],
        }
        self.post_no_content(
            &format!(
                "/internal/worker/control/ci/runtimes/{}/activity",
                placement.preview_id
            ),
            &PlayerReport {
                generation: placement.generation,
                lease_token: placement.lease_token,
                players,
            },
        )
        .await
    }
}

impl WorkerControl {
    pub async fn ci_runtime_lost(&self, placement: &CiRuntimePlacement) -> Result<()> {
        self.post_no_content(
            &format!(
                "/internal/worker/control/ci/runtimes/{}/runtime-lost",
                placement.preview_id
            ),
            &Fence {
                generation: placement.generation,
                lease_token: placement.lease_token,
            },
        )
        .await
    }
}
