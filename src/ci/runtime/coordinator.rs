use std::{fmt, sync::Arc};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Duration;
use thiserror::Error;
use uuid::Uuid;

use crate::ci::{CiPreviewRepository, CiPreviewState};

use super::{CI_RUNTIME_IDLE_SECONDS, CiPreviewRuntimeRepository, CiRuntimePlacement};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CiRuntimeState {
    Starting,
    Running,
    Stopping,
    Stopped,
    Deleting,
}

impl std::str::FromStr for CiRuntimeState {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "starting" => Ok(Self::Starting),
            "running" => Ok(Self::Running),
            "stopping" => Ok(Self::Stopping),
            "stopped" => Ok(Self::Stopped),
            "deleting" => Ok(Self::Deleting),
            _ => anyhow::bail!("unknown CI runtime state"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeLease {
    NoWorker,
    AlreadyRunning(CiRuntimePlacement),
    Starting(CiRuntimePlacement),
    Claimed(CiRuntimePlacement),
}

/// A single-generation host-control capability. The raw value is only sent to
/// the trusted worker/runtime launch channel; only its digest is persisted.
#[derive(Clone, PartialEq, Eq)]
pub struct RuntimeCapability {
    preview_id: Uuid,
    generation: i64,
    secret: String,
}
impl RuntimeCapability {
    fn issue(preview_id: Uuid, generation: i64) -> Self {
        let random: [u8; 32] = rand::random();
        Self {
            preview_id,
            generation,
            secret: URL_SAFE_NO_PAD.encode(random),
        }
    }
    fn digest(&self) -> [u8; 32] {
        *blake3::hash(self.secret.as_bytes()).as_bytes()
    }
    pub const fn preview_id(&self) -> Uuid {
        self.preview_id
    }
    pub const fn generation(&self) -> i64 {
        self.generation
    }
    pub fn expose_to_trusted_worker(&self) -> &str {
        &self.secret
    }
}
impl fmt::Debug for RuntimeCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeCapability")
            .field("preview_id", &self.preview_id)
            .field("generation", &self.generation)
            .field("secret", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum CiRuntimeError {
    #[error("CI preview runtime failed: {0}")]
    Failed(String),
}

/// Implemented by the authenticated worker control client. Each method must
/// enforce the worker ID, lease, preview ID, and generation from the provided
/// placement. `stop(..., retain_world=true)` snapshots/flushed persistent
/// world data before releasing ephemeral process/port resources.
#[async_trait]
pub trait CiPreviewRuntimeHost: Send + Sync {
    async fn verify_sandbox(&self) -> anyhow::Result<()>;
    async fn start(
        &self,
        placement: &CiRuntimePlacement,
        capability: &RuntimeCapability,
    ) -> anyhow::Result<()>;
    async fn stop(&self, placement: &CiRuntimePlacement, retain_world: bool) -> anyhow::Result<()>;
    async fn delete_world(&self, placement: &CiRuntimePlacement) -> anyhow::Result<()>;
}

/// Orchestrates worker-owned CI preview runtimes without ever creating an
/// account server record. DB state is the source of truth and every worker side
/// effect is generation/lease fenced by its `CiRuntimePlacement`.
pub struct CiPreviewRuntimeCoordinator<H> {
    previews: CiPreviewRepository,
    runtimes: CiPreviewRuntimeRepository,
    host: Arc<H>,
}

impl<H> CiPreviewRuntimeCoordinator<H>
where
    H: CiPreviewRuntimeHost + 'static,
{
    pub fn new(
        previews: CiPreviewRepository,
        runtimes: CiPreviewRuntimeRepository,
        host: Arc<H>,
    ) -> Self {
        Self {
            previews,
            runtimes,
            host,
        }
    }

    /// Ensure a built preview is online. A branch lobby remains a build lobby;
    /// its caller enqueues/executes the lazy build and retries after ready.
    pub async fn ensure_running(&self, hostname: &str) -> anyhow::Result<CiPreviewState> {
        let preview = self
            .previews
            .lookup_preview(hostname)
            .await?
            .ok_or_else(|| anyhow::anyhow!("CI preview is unknown or expired"))?;
        if preview.state == CiPreviewState::Lobby {
            self.previews.ensure_lazy_branch_build(preview.id).await?;
            return Ok(CiPreviewState::Queued);
        }
        if preview.state == CiPreviewState::Running {
            self.runtimes.touch_player_activity(preview.id).await?;
            return Ok(CiPreviewState::Running);
        }
        if preview.state != CiPreviewState::Ready {
            return Ok(preview.state);
        }

        let capability = RuntimeCapability::issue(preview.id, preview.runtime_generation + 1);
        let lease = self
            .runtimes
            .claim_start(preview.id, &capability.digest())
            .await?;
        match lease {
            RuntimeLease::NoWorker => Ok(CiPreviewState::Ready),
            RuntimeLease::AlreadyRunning(placement) => {
                self.runtimes
                    .touch_player_activity(placement.preview_id)
                    .await?;
                Ok(CiPreviewState::Running)
            }
            RuntimeLease::Starting(_) => Ok(CiPreviewState::Booting),
            RuntimeLease::Claimed(placement) => {
                if capability.generation() != placement.generation {
                    anyhow::bail!("CI runtime capability generation does not match placement");
                }
                let result = async {
                    self.host.verify_sandbox().await?;
                    self.host.start(&placement, &capability).await?;
                    if !self
                        .runtimes
                        .mark_running(
                            placement.preview_id,
                            placement.worker_id,
                            placement.generation,
                            placement.lease_token,
                        )
                        .await?
                    {
                        // The assignment changed or the preview expired while
                        // the runtime was booting. Tear down the stale process.
                        self.host.stop(&placement, false).await?;
                        anyhow::bail!("CI runtime start lost its generation fence");
                    }
                    Ok::<(), anyhow::Error>(())
                }
                .await;
                if let Err(error) = result {
                    let _ = self.host.stop(&placement, false).await;
                    let _ = self
                        .runtimes
                        .mark_start_failed(
                            placement.preview_id,
                            placement.worker_id,
                            placement.generation,
                            placement.lease_token,
                        )
                        .await;
                    return Err(error);
                }
                Ok(CiPreviewState::Running)
            }
        }
    }

    /// Called by the gateway after a playable session is established and by
    /// worker activity reports. Extends the preview/build/artifact TTL together.
    pub async fn touch_player_activity(&self, preview_id: Uuid) -> anyhow::Result<bool> {
        self.runtimes.touch_player_activity(preview_id).await
    }

    /// Apply the shared five-minute idle stop policy to CI runtimes. Worlds are
    /// retained on the same worker and resume with a new generation and lease.
    pub async fn stop_idle(&self) -> anyhow::Result<u64> {
        let due = self
            .runtimes
            .claim_idle(Duration::seconds(CI_RUNTIME_IDLE_SECONDS), 128)
            .await?;
        let mut stopped = 0;
        for placement in due {
            match self.host.stop(&placement, true).await {
                Ok(()) => {
                    if self.runtimes.complete_idle_stop(&placement, true).await? {
                        stopped += 1;
                    }
                }
                Err(error) => {
                    let _ = self.runtimes.release_idle_claim(&placement).await;
                    tracing::error!(preview_id = %placement.preview_id, generation = placement.generation,
                        ?error, "failed to idle-stop CI runtime");
                }
            }
        }
        Ok(stopped)
    }

    /// Expire aliases, worlds, and their worker-local runtime data as one
    /// retryable cleanup operation. A failed remote delete leaves the placement
    /// tombstone and capacity reservation in place for the next sweep.
    pub async fn expire(&self, now: chrono::DateTime<chrono::Utc>) -> anyhow::Result<u64> {
        let expired = self.runtimes.claim_expired(now, 128).await?;
        let mut cleaned = 0;
        for item in expired {
            let placement = &item.placement;
            let result = async {
                if placement.active {
                    self.host.stop(placement, false).await?;
                }
                self.host.delete_world(placement).await?;
                if !self.runtimes.complete_expiry(placement).await? {
                    anyhow::bail!("CI runtime expiry lost its worker/generation fence");
                }
                Ok::<(), anyhow::Error>(())
            }
            .await;
            match result {
                Ok(()) => cleaned += 1,
                Err(error) => {
                    let _ = self.runtimes.release_expiry_claim(placement).await;
                    tracing::error!(preview_id = %placement.preview_id, generation = placement.generation,
                        ?error, "failed to delete expired CI runtime world");
                }
            }
        }
        Ok(cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_capability_is_generation_scoped_and_redacts_secret() {
        let preview = Uuid::from_u128(7);
        let capability = RuntimeCapability::issue(preview, 3);
        assert_eq!(capability.preview_id(), preview);
        assert_eq!(capability.generation(), 3);
        assert_eq!(capability.digest().len(), 32);
        assert!(!format!("{capability:?}").contains(capability.expose_to_trusted_worker()));
        assert_ne!(
            capability.digest(),
            *blake3::hash(
                RuntimeCapability::issue(preview, 3)
                    .expose_to_trusted_worker()
                    .as_bytes()
            )
            .as_bytes()
        );
    }

    #[test]
    fn runtime_state_parser_rejects_unknown_worker_states() {
        assert_eq!(
            "starting".parse::<CiRuntimeState>().unwrap(),
            CiRuntimeState::Starting
        );
        assert!("running-old".parse::<CiRuntimeState>().is_err());
    }
}
