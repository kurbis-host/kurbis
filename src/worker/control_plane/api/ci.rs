use anyhow::{Context, Result, bail};
use futures::StreamExt;
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

    pub async fn ci_runtime_world_initialized(&self, placement: &CiRuntimePlacement) -> Result<()> {
        self.post_no_content(
            &format!(
                "/internal/worker/control/ci/runtimes/{}/world-initialized",
                placement.preview_id
            ),
            &Fence {
                generation: placement.generation,
                lease_token: placement.lease_token,
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

    pub async fn download_ci_artifact(
        &self,
        placement: &CiRuntimePlacement,
        destination: &std::path::Path,
    ) -> Result<(u64, String)> {
        use tokio::io::AsyncWriteExt;
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
        let expected_size = response
            .content_length()
            .context("CI artifact response omitted its size")?;
        let max_artifact_bytes = crate::ci::execution::MAX_ARTIFACT_BYTES as u64;
        if expected_size == 0 || expected_size > max_artifact_bytes {
            bail!("CI artifact exceeds worker transfer limit");
        }
        let expected_digest = response
            .headers()
            .get("x-ci-artifact-digest")
            .and_then(|value| value.to_str().ok())
            .filter(|value| value == &placement.artifact_digest)
            .context("CI artifact response omitted its assigned digest")?
            .to_owned();
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .await
            .with_context(|| {
                format!("create CI artifact staging file {}", destination.display())
            })?;
        let mut stream = response.bytes_stream();
        let mut hasher = blake3::Hasher::new();
        let mut total = 0_u64;
        let result = async {
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.context("read CI artifact response")?;
                total = total
                    .checked_add(chunk.len() as u64)
                    .context("CI artifact size overflow")?;
                if total > max_artifact_bytes || total > expected_size {
                    bail!("CI artifact exceeded its declared size");
                }
                hasher.update(&chunk);
                file.write_all(&chunk).await?;
            }
            file.flush().await?;
            file.sync_all().await?;
            let digest = hasher.finalize().to_hex().to_string();
            anyhow::ensure!(
                total == expected_size && digest == expected_digest,
                "downloaded CI artifact failed size or digest verification"
            );
            Ok::<_, anyhow::Error>((total, digest))
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(destination).await;
        }
        result
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

impl WorkerControl {
    fn ci_world_snapshot_url(&self, placement: &CiRuntimePlacement) -> Result<reqwest::Url> {
        let mut url = self.base_url().clone();
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("control-plane URL cannot accept CI snapshot route"))?
            .pop_if_empty()
            .extend([
                "internal",
                "worker",
                "control",
                "ci",
                "runtimes",
                &placement.preview_id.to_string(),
                "world-snapshot",
                &placement.generation.to_string(),
                &placement.lease_token.to_string(),
            ]);
        Ok(url)
    }

    pub async fn ci_world_snapshot_requested(
        &self,
        placement: &CiRuntimePlacement,
    ) -> Result<bool> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct SnapshotRequestState {
            requested: bool,
        }
        let url = {
            let mut url = self.base_url().clone();
            url.path_segments_mut()
                .map_err(|_| anyhow::anyhow!("control-plane URL cannot accept CI snapshot route"))?
                .pop_if_empty()
                .extend([
                    "internal",
                    "worker",
                    "control",
                    "ci",
                    "runtimes",
                    &placement.preview_id.to_string(),
                    "world-snapshot-requested",
                    &placement.generation.to_string(),
                    &placement.lease_token.to_string(),
                ]);
            url
        };
        let response = self
            .authorized_request(reqwest::Method::GET, url)?
            .send()
            .await
            .context("request CI snapshot state")?;
        if !response.status().is_success() {
            bail!(
                "control plane rejected CI snapshot state ({})",
                response.status()
            );
        }
        Ok(response.json::<SnapshotRequestState>().await?.requested)
    }

    pub async fn ci_world_snapshot_upload(
        &self,
        placement: &CiRuntimePlacement,
        archive: &std::path::Path,
        size_bytes: u64,
    ) -> Result<()> {
        use tokio::io::AsyncReadExt;

        anyhow::ensure!(size_bytes > 0, "CI world snapshot is empty");
        let file = tokio::fs::File::open(archive).await?;
        anyhow::ensure!(
            file.metadata().await?.len() == size_bytes,
            "CI world snapshot changed before upload"
        );
        let stream = futures::stream::try_unfold(file, |mut file| async move {
            let mut chunk = vec![0_u8; 1024 * 1024];
            let read = file.read(&mut chunk).await?;
            if read == 0 {
                return Ok::<_, std::io::Error>(None);
            }
            chunk.truncate(read);
            Ok(Some((bytes::Bytes::from(chunk), file)))
        });
        let response = self
            .authorized_request(reqwest::Method::PUT, self.ci_world_snapshot_url(placement)?)?
            .header(reqwest::header::CONTENT_TYPE, "application/gzip")
            .header(reqwest::header::CONTENT_LENGTH, size_bytes)
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await
            .context("upload CI world snapshot")?;
        if !response.status().is_success() {
            bail!(
                "control plane rejected CI world snapshot upload ({})",
                response.status()
            );
        }
        Ok(())
    }

    pub async fn ci_world_snapshot_download(
        &self,
        placement: &CiRuntimePlacement,
        destination: &std::path::Path,
        maximum_bytes: u64,
    ) -> Result<Option<(u64, String)>> {
        use tokio::io::AsyncWriteExt;
        let response = self
            .authorized_request(reqwest::Method::GET, self.ci_world_snapshot_url(placement)?)?
            .send()
            .await
            .context("download CI world snapshot")?;
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if !response.status().is_success() {
            bail!(
                "control plane rejected CI world snapshot download ({})",
                response.status()
            );
        }
        let expected_size = response
            .content_length()
            .context("CI world snapshot response omitted its size")?;
        if expected_size == 0 || expected_size > maximum_bytes {
            bail!("CI world snapshot exceeds its transfer limit");
        }
        let expected_checksum = response
            .headers()
            .get("x-ci-world-snapshot-checksum")
            .and_then(|value| value.to_str().ok())
            .filter(|value| {
                value.len() == 64
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            })
            .context("CI world snapshot response omitted a valid checksum")?
            .to_owned();
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .await?;
        let mut stream = response.bytes_stream();
        let mut hasher = blake3::Hasher::new();
        let mut total = 0_u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("read CI world snapshot response")?;
            total = total
                .checked_add(chunk.len() as u64)
                .context("CI world snapshot size overflow")?;
            if total > maximum_bytes || total > expected_size {
                bail!("CI world snapshot exceeded its declared size");
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        file.sync_all().await?;
        let checksum = hasher.finalize().to_hex().to_string();
        if total != expected_size || checksum != expected_checksum {
            let _ = tokio::fs::remove_file(destination).await;
            bail!("CI world snapshot failed size or checksum verification");
        }
        Ok(Some((total, checksum)))
    }

    pub async fn ci_world_snapshot_complete(&self, placement: &CiRuntimePlacement) -> Result<()> {
        self.post_no_content(
            &format!(
                "/internal/worker/control/ci/runtimes/{}/world-snapshot-complete",
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
