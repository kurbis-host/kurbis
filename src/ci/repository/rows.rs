use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::super::{
    domain::{BuildKey, CiBuild, CiHostname, CiPreview, CiPreviewState, CiRefKind},
    source_control::PollCheckpoint,
};
use super::{CheckRunReconciliation, CiRefRecord, WatchedRepository};

#[derive(Debug, sqlx::FromRow)]
pub(super) struct CheckRunReconciliationRow {
    pub(super) build_id: Uuid,
    pub(super) check_run_id: Option<i64>,
    pub(super) repository: String,
    pub(super) commit_sha: String,
    pub(super) patch_fingerprint: String,
    pub(super) toolchain_version: String,
    pub(super) target_architecture: String,
    pub(super) stage: String,
    pub(super) artifact_digest: Option<String>,
    pub(super) failure_stage: Option<String>,
    pub(super) failure_message: Option<String>,
}

impl TryFrom<CheckRunReconciliationRow> for CheckRunReconciliation {
    type Error = anyhow::Error;

    fn try_from(row: CheckRunReconciliationRow) -> Result<Self> {
        let key = BuildKey::new(
            row.repository,
            row.commit_sha,
            row.patch_fingerprint,
            row.toolchain_version,
            row.target_architecture,
        )?;
        Ok(Self {
            build_id: row.build_id,
            check_run_id: row
                .check_run_id
                .context("Check Run reconciliation row has no external id")?,
            key,
            stage: row.stage.parse()?,
            artifact_digest: row.artifact_digest,
            failure_stage: row.failure_stage.map(|stage| stage.parse()).transpose()?,
            failure_message: row.failure_message,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct WatchedRepositoryRow {
    pub(super) id: Uuid,
    pub(super) repository: String,
    pub(super) default_branch: String,
    pub(super) enabled: bool,
    pub(super) etag: Option<String>,
    pub(super) branch_etag: Option<String>,
    pub(super) pull_request_page: i32,
    pub(super) branch_cursor: Option<String>,
    pub(super) last_successful_poll: Option<DateTime<Utc>>,
}

impl From<WatchedRepositoryRow> for WatchedRepository {
    fn from(row: WatchedRepositoryRow) -> Self {
        Self {
            id: row.id,
            repository: row.repository,
            default_branch: row.default_branch,
            enabled: row.enabled,
            checkpoint: PollCheckpoint {
                etag: row.etag,
                branch_etag: row.branch_etag,
                pull_request_page: row.pull_request_page.max(1) as u32,
                branch_cursor: row.branch_cursor,
                last_successful_poll: row.last_successful_poll,
            },
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct CiRefRecordRow {
    pub(super) id: Uuid,
    pub(super) repository_id: Uuid,
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) number: Option<i64>,
    pub(super) commit_sha: String,
    pub(super) closed: bool,
    pub(super) deleted: bool,
    pub(super) updated_at: DateTime<Utc>,
}

impl TryFrom<CiRefRecordRow> for CiRefRecord {
    type Error = anyhow::Error;

    fn try_from(row: CiRefRecordRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            repository_id: row.repository_id,
            kind: row.kind.parse()?,
            name: row.name,
            number: row.number,
            commit_sha: row.commit_sha,
            closed: row.closed,
            deleted: row.deleted,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct CiBuildValidityRow {
    pub(super) id: Uuid,
    pub(super) repository: String,
    pub(super) commit_sha: String,
    pub(super) patch_fingerprint: String,
    pub(super) toolchain_version: String,
    pub(super) target_architecture: String,
    pub(super) stage: String,
    pub(super) artifact_digest: Option<String>,
    pub(super) failure_stage: Option<String>,
    pub(super) failure_message: Option<String>,
    pub(super) build_log: String,
    pub(super) created_at: DateTime<Utc>,
    pub(super) expires_at: DateTime<Utc>,
    pub(super) artifact_valid: bool,
}
impl CiBuildValidityRow {
    pub(super) fn into_build_row(self) -> CiBuildRow {
        CiBuildRow {
            id: self.id,
            repository: self.repository,
            commit_sha: self.commit_sha,
            patch_fingerprint: self.patch_fingerprint,
            toolchain_version: self.toolchain_version,
            target_architecture: self.target_architecture,
            stage: self.stage,
            artifact_digest: self.artifact_digest,
            failure_stage: self.failure_stage,
            failure_message: self.failure_message,
            build_log: self.build_log,
            created_at: self.created_at,
            expires_at: self.expires_at,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct CiBuildRow {
    pub(super) id: Uuid,
    pub(super) repository: String,
    pub(super) commit_sha: String,
    pub(super) patch_fingerprint: String,
    pub(super) toolchain_version: String,
    pub(super) target_architecture: String,
    pub(super) stage: String,
    pub(super) artifact_digest: Option<String>,
    pub(super) failure_stage: Option<String>,
    pub(super) failure_message: Option<String>,
    pub(super) build_log: String,
    pub(super) created_at: DateTime<Utc>,
    pub(super) expires_at: DateTime<Utc>,
}

impl TryFrom<CiBuildRow> for CiBuild {
    type Error = anyhow::Error;

    fn try_from(row: CiBuildRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            key: BuildKey::new(
                row.repository,
                row.commit_sha,
                row.patch_fingerprint,
                row.toolchain_version,
                row.target_architecture,
            )?,
            stage: row.stage.parse()?,
            artifact_digest: row.artifact_digest,
            failure_stage: row.failure_stage.map(|stage| stage.parse()).transpose()?,
            failure_message: row.failure_message,
            build_log: row.build_log,
            created_at: row.created_at,
            expires_at: row.expires_at,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct CiPreviewRow {
    pub(super) id: Uuid,
    pub(super) repository: String,
    pub(super) commit_sha: String,
    pub(super) kind: String,
    pub(super) hostname: String,
    pub(super) build_id: Uuid,
    pub(super) state: String,
    pub(super) runtime_generation: i64,
    pub(super) failure_message: Option<String>,
    pub(super) build_log: String,
    pub(super) expires_at: DateTime<Utc>,
}

impl TryFrom<CiPreviewRow> for CiPreview {
    type Error = anyhow::Error;

    fn try_from(row: CiPreviewRow) -> Result<Self> {
        let kind = row.kind.parse::<CiRefKind>()?;
        Ok(Self {
            id: row.id,
            repository: row.repository,
            commit_sha: row.commit_sha,
            alias: CiHostname {
                value: row.hostname,
                kind,
            },
            state: match row.state.as_str() {
                "lobby" => CiPreviewState::Lobby,
                "queued" => CiPreviewState::Queued,
                "fetching" => CiPreviewState::Fetching,
                "patching" => CiPreviewState::Patching,
                "compiling" => CiPreviewState::Compiling,
                "packaging" => CiPreviewState::Packaging,
                "booting" => CiPreviewState::Booting,
                "ready" => CiPreviewState::Ready,
                "running" => CiPreviewState::Running,
                "stopping" => CiPreviewState::Stopping,
                "failed" => CiPreviewState::Failed,
                "expired" => CiPreviewState::Expired,
                _ => bail!("unknown CI preview state"),
            },
            build_id: row.build_id,
            runtime_generation: row.runtime_generation,
            failure_message: row.failure_message,
            build_log: row.build_log,
            expires_at: row.expires_at,
        })
    }
}
