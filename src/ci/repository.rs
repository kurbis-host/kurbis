use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::domain::{
    BUILD_TTL_HOURS, BuildKey, CiBuild, CiBuildStage, CiHostname, CiPreview, CiPreviewState,
    CiRefKind, validate_commit_sha,
};
use super::source_control::{PollCheckpoint, SourceRefKind};

#[derive(Debug, Clone)]
pub struct WatchedRepository {
    pub id: Uuid,
    pub repository: String,
    pub default_branch: String,
    pub enabled: bool,
    pub checkpoint: PollCheckpoint,
}

#[derive(Debug, Clone)]
pub struct CheckRunReconciliation {
    pub build_id: Uuid,
    pub check_run_id: i64,
    pub key: BuildKey,
    pub stage: CiBuildStage,
    pub artifact_digest: Option<String>,
    pub failure_stage: Option<CiBuildStage>,
    pub failure_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CiRefRecord {
    pub id: Uuid,
    pub repository_id: Uuid,
    pub kind: CiRefKind,
    pub name: String,
    pub number: Option<i64>,
    pub commit_sha: String,
    pub closed: bool,
    pub deleted: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct CiPreviewRepository {
    pool: PgPool,
}

impl CiPreviewRepository {
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn repository_name(&self, repository_id: Uuid) -> Result<String> {
        sqlx::query_scalar("SELECT repository FROM ci_watched_repositories WHERE id = $1")
            .bind(repository_id)
            .fetch_optional(&self.pool)
            .await?
            .context("watched CI repository not found")
    }

    pub async fn register_repository(
        &self,
        repository: &str,
        default_branch: &str,
    ) -> Result<WatchedRepository> {
        validate_repository_name(repository)?;
        if default_branch.trim().is_empty() {
            bail!("default branch cannot be empty");
        }
        let row = sqlx::query_as::<_, WatchedRepositoryRow>(
            "INSERT INTO ci_watched_repositories (repository, default_branch)
             VALUES ($1, $2)
             ON CONFLICT (repository) DO UPDATE SET default_branch = EXCLUDED.default_branch,
                 enabled = TRUE, updated_at = now()
             RETURNING id, repository, default_branch, enabled, etag, branch_etag, pull_request_page,
                       branch_cursor, last_successful_poll",
        )
        .bind(repository)
        .bind(default_branch)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.into())
    }

    /// Poll and persist one repository while holding its row lock. The lock spans the
    /// provider request and persistence so two pollers cannot interleave checkpoints or
    /// mark refs from an older page as deleted.
    pub async fn poll_repository<C: crate::ci::source_control::SourceControlClient + ?Sized>(
        &self,
        repository_id: Uuid,
        source: &C,
    ) -> Result<crate::ci::source_control::PollResult> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query_as::<_, WatchedRepositoryRow>(
            "SELECT id, repository, default_branch, enabled, etag, branch_etag, pull_request_page,
                    branch_cursor, last_successful_poll
             FROM ci_watched_repositories WHERE id = $1 AND enabled FOR UPDATE",
        )
        .bind(repository_id)
        .fetch_optional(&mut *tx)
        .await?
        .context("watched CI repository not found or disabled")?;
        let poll = source
            .poll(
                &row.repository,
                &PollCheckpoint {
                    etag: row.etag.clone(),
                    branch_etag: row.branch_etag.clone(),
                    pull_request_page: row.pull_request_page.max(1) as u32,
                    branch_cursor: row.branch_cursor.clone(),
                    last_successful_poll: row.last_successful_poll,
                },
            )
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        sqlx::query(
            "UPDATE ci_watched_repositories SET etag = $2, branch_etag = $3,
                    pull_request_page = $4, branch_cursor = $5, last_successful_poll = $6,
                    updated_at = now() WHERE id = $1",
        )
        .bind(repository_id)
        .bind(&poll.checkpoint.etag)
        .bind(&poll.checkpoint.branch_etag)
        .bind(
            i32::try_from(poll.checkpoint.pull_request_page.max(1))
                .context("poll page overflow")?,
        )
        .bind(&poll.checkpoint.branch_cursor)
        .bind(poll.checkpoint.last_successful_poll)
        .execute(&mut *tx)
        .await?;
        let mut seen = HashSet::new();
        for source_ref in &poll.refs {
            let kind = match source_ref.kind {
                SourceRefKind::PullRequest => CiRefKind::PullRequest,
                SourceRefKind::Branch => CiRefKind::Branch,
            };
            seen.insert((kind.as_str().to_owned(), source_ref.name.clone()));
            sqlx::query(
                "INSERT INTO ci_refs (repository_id, kind, name, number, commit_sha, source_repository, closed, deleted, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, COALESCE($9, now()))
                 ON CONFLICT (repository_id, kind, name) DO UPDATE SET number = EXCLUDED.number,
                   commit_sha = EXCLUDED.commit_sha, source_repository = EXCLUDED.source_repository,
                   closed = EXCLUDED.closed, deleted = EXCLUDED.deleted, updated_at = EXCLUDED.updated_at",
            )
            .bind(repository_id).bind(kind.as_str()).bind(&source_ref.name)
            .bind(source_ref.number.map(i64::try_from).transpose()?).bind(&source_ref.commit_sha).bind(&source_ref.source_repository)
            .bind(source_ref.closed).bind(source_ref.deleted).bind(source_ref.updated_at)
            .execute(&mut *tx).await?;
        }
        for kind in [CiRefKind::PullRequest, CiRefKind::Branch] {
            let unchanged = match kind {
                CiRefKind::PullRequest => SourceRefKind::PullRequest,
                CiRefKind::Branch => SourceRefKind::Branch,
                CiRefKind::Commit => unreachable!(),
            };
            if !poll.complete_kinds.contains(&unchanged) {
                // We hit the pagination safety cap. Never infer deletion from
                // an incomplete enumeration of GitHub refs.
                continue;
            }
            let names = seen
                .iter()
                .filter_map(|(seen_kind, name)| {
                    (seen_kind == kind.as_str()).then_some(name.clone())
                })
                .collect::<Vec<_>>();
            sqlx::query("UPDATE ci_refs SET deleted = TRUE, updated_at = now() WHERE repository_id = $1 AND kind = $2 AND deleted = FALSE AND NOT (name = ANY($3::text[]))")
                .bind(repository_id).bind(kind.as_str()).bind(names).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(poll)
    }

    pub async fn ensure_build(&self, key: &BuildKey) -> Result<(CiBuild, bool)> {
        key.validate().map_err(|error| anyhow::anyhow!(error))?;
        let mut tx = self.pool.begin().await?;
        let inserted = sqlx::query(
            "INSERT INTO ci_builds (repository, commit_sha, patch_fingerprint, toolchain_version,
                 target_architecture, stage, expires_at)
             VALUES ($1, $2, $3, $4, $5, 'queued', now() + interval '72 hours')
             ON CONFLICT (repository, commit_sha, patch_fingerprint, toolchain_version, target_architecture)
             DO NOTHING",
        )
        .bind(&key.repository)
        .bind(&key.commit_sha)
        .bind(&key.patch_fingerprint)
        .bind(&key.toolchain_version)
        .bind(&key.target_architecture)
        .execute(&mut *tx)
        .await?
        .rows_affected() == 1;
        let row: CiBuildValidityRow = sqlx::query_as(
            "SELECT b.id, b.repository, b.commit_sha, b.patch_fingerprint, b.toolchain_version,
                    b.target_architecture, b.stage, b.artifact_digest, b.failure_stage,
                    b.failure_message, b.build_log, b.created_at, b.expires_at,
                    (a.digest IS NOT NULL AND a.expires_at > now()) AS artifact_valid
             FROM ci_builds b LEFT JOIN ci_artifacts a ON a.digest = b.artifact_digest
             WHERE b.repository = $1 AND b.commit_sha = $2 AND b.patch_fingerprint = $3
               AND b.toolchain_version = $4 AND b.target_architecture = $5 FOR UPDATE OF b",
        )
        .bind(&key.repository)
        .bind(&key.commit_sha)
        .bind(&key.patch_fingerprint)
        .bind(&key.toolchain_version)
        .bind(&key.target_architecture)
        .fetch_one(&mut *tx)
        .await?;
        let stale_success = row.stage == "ready" && !row.artifact_valid;
        let elapsed = row.expires_at <= Utc::now();
        let row = if stale_success || elapsed && row.stage != "queued" {
            sqlx::query_as::<_, CiBuildRow>(
                "UPDATE ci_builds SET stage = 'queued', artifact_digest = NULL,
                    failure_stage = NULL, failure_message = NULL, expires_at = now() + interval '72 hours',
                    lease_token = NULL, lease_expires_at = NULL, updated_at = now()
                 WHERE id = $1 AND (lease_expires_at IS NULL OR lease_expires_at <= now())
                 RETURNING id, repository, commit_sha, patch_fingerprint, toolchain_version,
                           target_architecture, stage, artifact_digest, failure_stage, failure_message, build_log,
                           created_at, expires_at",
            )
            .bind(row.id)
            .fetch_optional(&mut *tx)
            .await?
             .unwrap_or_else(|| row.into_build_row())
        } else {
            row.into_build_row()
        };
        tx.commit().await?;
        Ok((row.try_into()?, inserted))
    }

    pub async fn claim_build(
        &self,
        build_id: Uuid,
        lease: Uuid,
        lease_duration: Duration,
    ) -> Result<bool> {
        if lease_duration.is_zero() {
            bail!("build lease duration must be non-zero");
        }
        let result = sqlx::query(
            "UPDATE ci_builds SET stage = 'queued', lease_token = $2, lease_expires_at = now() + $3::interval,
                 failure_stage = NULL, failure_message = NULL, artifact_digest = NULL, updated_at = now()
             WHERE id = $1 AND stage IN ('queued', 'fetching', 'patching', 'compiling', 'packaging')
               AND (lease_expires_at IS NULL OR lease_expires_at <= now())",
        )
        .bind(build_id)
        .bind(lease)
        .bind(format!("{} seconds", lease_duration.num_seconds()))
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn renew_build_claim(
        &self,
        build_id: Uuid,
        lease: Uuid,
        lease_duration: Duration,
    ) -> Result<bool> {
        if lease_duration <= Duration::zero() {
            bail!("build lease duration must be positive");
        }
        let rows = sqlx::query(
            "UPDATE ci_builds SET lease_expires_at = now() + $3::interval, updated_at = now()
             WHERE id = $1 AND lease_token = $2 AND lease_expires_at > now()
               AND stage IN ('queued', 'fetching', 'patching', 'compiling', 'packaging')",
        )
        .bind(build_id)
        .bind(lease)
        .bind(format!("{} seconds", lease_duration.num_seconds()))
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(rows == 1)
    }

    pub async fn release_build(&self, build_id: Uuid, lease: Uuid) -> Result<()> {
        sqlx::query("UPDATE ci_builds SET lease_token = NULL, lease_expires_at = NULL, updated_at = now() WHERE id = $1 AND lease_token = $2")
            .bind(build_id).bind(lease).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn transition_build(
        &self,
        build_id: Uuid,
        next: CiBuildStage,
        failure_message: Option<&str>,
    ) -> Result<CiBuild> {
        let mut tx = self.pool.begin().await?;
        let current: CiBuildRow = sqlx::query_as(
            "SELECT id, repository, commit_sha, patch_fingerprint, toolchain_version,
                    target_architecture, stage, artifact_digest, failure_stage, failure_message, build_log,
                    created_at, expires_at FROM ci_builds WHERE id = $1 FOR UPDATE",
        )
        .bind(build_id)
        .fetch_one(&mut *tx)
        .await?;
        let current_stage = current.stage.parse::<CiBuildStage>()?;
        if !current_stage.can_transition_to(next) {
            bail!("invalid CI build transition {current_stage} -> {next}");
        }
        let row: CiBuildRow = sqlx::query_as(
            "UPDATE ci_builds SET stage = $2, failure_stage = CASE WHEN $2 = 'failed' THEN $2 ELSE failure_stage END,
                    failure_message = CASE WHEN $2 = 'failed' THEN $3 ELSE failure_message END,
                    expires_at = CASE WHEN $2 = 'ready' THEN now() + interval '72 hours' ELSE expires_at END,
                    updated_at = now() WHERE id = $1
             RETURNING id, repository, commit_sha, patch_fingerprint, toolchain_version,
                       target_architecture, stage, artifact_digest, failure_stage, failure_message, build_log,
                       created_at, expires_at",
        )
        .bind(build_id)
        .bind(next.to_string())
        .bind(failure_message)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        row.try_into()
    }

    pub async fn transition_claimed_build(
        &self,
        build_id: Uuid,
        lease: Uuid,
        next: CiBuildStage,
    ) -> Result<CiBuild> {
        let mut tx = self.pool.begin().await?;
        let current: Option<(String, Option<Uuid>)> =
            sqlx::query_as("SELECT stage, lease_token FROM ci_builds WHERE id = $1 AND lease_expires_at > now() FOR UPDATE")
                .bind(build_id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some((stage, current_lease)) = current else {
            bail!("CI build not found");
        };
        if current_lease != Some(lease) {
            bail!("CI build lease is stale");
        }
        let current_stage = stage.parse::<CiBuildStage>()?;
        if !current_stage.can_transition_to(next) {
            bail!("invalid CI build transition {current_stage} -> {next}");
        }
        let row: CiBuildRow = sqlx::query_as(
            "UPDATE ci_builds SET stage = $3,
                 failure_stage = CASE WHEN $3 = 'failed' THEN $3 ELSE failure_stage END,
                 updated_at = now() WHERE id = $1 AND lease_token = $2 AND lease_expires_at > now()
             RETURNING id, repository, commit_sha, patch_fingerprint, toolchain_version,
                       target_architecture, stage, artifact_digest, failure_stage, failure_message, build_log,
                       created_at, expires_at",
        )
        .bind(build_id)
        .bind(lease)
        .bind(next.to_string())
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        row.try_into()
    }

    pub async fn fail_build(
        &self,
        build_id: Uuid,
        failure_stage: CiBuildStage,
        message: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE ci_builds SET stage = 'failed', failure_stage = $2, failure_message = $3,
                    lease_token = NULL, lease_expires_at = NULL, updated_at = now()
             WHERE id = $1 AND stage NOT IN ('ready', 'expired')",
        )
        .bind(build_id)
        .bind(failure_stage.to_string())
        .bind(message.chars().take(8192).collect::<String>())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn fail_claimed_build(
        &self,
        build_id: Uuid,
        lease: Uuid,
        failure_stage: CiBuildStage,
        message: &str,
    ) -> Result<bool> {
        let rows = sqlx::query(
            "UPDATE ci_builds SET stage = 'failed', failure_stage = $3, failure_message = $4,
                    updated_at = now()
             WHERE id = $1 AND lease_token = $2 AND lease_expires_at > now() AND stage NOT IN ('ready', 'expired')",
        )
        .bind(build_id)
        .bind(lease)
        .bind(failure_stage.to_string())
        .bind(message.chars().take(8192).collect::<String>())
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(rows == 1)
    }

    pub async fn append_build_log_claimed(
        &self,
        build_id: Uuid,
        lease: Uuid,
        stage: CiBuildStage,
        chunk: &str,
    ) -> Result<()> {
        const MAX_BUILD_LOG_BYTES: usize = 512 * 1024;
        let mut start = chunk.len().saturating_sub(MAX_BUILD_LOG_BYTES);
        while start < chunk.len() && !chunk.is_char_boundary(start) {
            start += 1;
        }
        let bounded = chunk[start..].to_owned();
        let updated = sqlx::query(
            "UPDATE ci_builds SET build_log = right(build_log || $3, $4), updated_at = now()
             WHERE id = $1 AND lease_token = $2 AND lease_expires_at > now() AND stage = $5",
        )
        .bind(build_id)
        .bind(lease)
        .bind(bounded)
        .bind(MAX_BUILD_LOG_BYTES as i32)
        .bind(stage.to_string())
        .execute(&self.pool)
        .await?
        .rows_affected();
        if updated != 1 {
            bail!("CI build lease is stale while appending stage log");
        }
        Ok(())
    }

    pub async fn build_diagnostics(
        &self,
        build_id: Uuid,
        root_domain: &str,
    ) -> Result<(String, DateTime<Utc>, Vec<String>)> {
        let expires_at: DateTime<Utc> =
            sqlx::query_scalar("SELECT expires_at FROM ci_builds WHERE id = $1")
                .bind(build_id)
                .fetch_one(&self.pool)
                .await?;
        let build_log = self.build_log(build_id).await?.unwrap_or_default();
        let mut hostnames: Vec<String> = sqlx::query_scalar(
            "SELECT hostname FROM ci_preview_aliases WHERE build_id = $1 ORDER BY kind, hostname",
        )
        .bind(build_id)
        .fetch_all(&self.pool)
        .await?;
        let key = self.build_key(build_id).await?;
        let commit_hostname =
            CiHostname::for_ref(CiRefKind::Commit, &key.commit_sha, root_domain)?.value;
        if !hostnames
            .iter()
            .any(|hostname| hostname == &commit_hostname)
        {
            hostnames.push(commit_hostname);
        }
        hostnames.sort();
        hostnames.dedup();
        hostnames.truncate(8);
        Ok((build_log, expires_at, hostnames))
    }

    pub async fn build_key(&self, build_id: Uuid) -> Result<BuildKey> {
        let row = sqlx::query_as::<_, (String, String, String, String, String)>(
            "SELECT repository, commit_sha, patch_fingerprint, toolchain_version, target_architecture
             FROM ci_builds WHERE id = $1",
        )
        .bind(build_id)
        .fetch_one(&self.pool)
        .await?;
        BuildKey::new(row.0, row.1, row.2, row.3, row.4).map_err(Into::into)
    }

    pub async fn preview_hostnames_for_build(&self, build_id: Uuid) -> Result<Vec<String>> {
        let mut hostnames: Vec<String> = sqlx::query_scalar(
            "SELECT hostname FROM ci_preview_aliases
             WHERE build_id = $1 AND expires_at > now()
             ORDER BY kind, hostname LIMIT 8",
        )
        .bind(build_id)
        .fetch_all(&self.pool)
        .await?;
        hostnames.sort();
        hostnames.dedup();
        Ok(hostnames)
    }

    pub async fn build_log(&self, build_id: Uuid) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT build_log FROM ci_builds WHERE id = $1")
                .bind(build_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn owns_build_lease(&self, build_id: Uuid, lease: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM ci_builds WHERE id = $1 AND lease_token = $2 AND lease_expires_at > now())",
        )
        .bind(build_id)
        .bind(lease)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn check_run_reconciliation_candidates(
        &self,
        limit: i64,
    ) -> Result<Vec<CheckRunReconciliation>> {
        let rows = sqlx::query_as::<_, CheckRunReconciliationRow>(
            "SELECT build.id AS build_id, check_run.external_id AS check_run_id,
                    build.repository, build.commit_sha, build.patch_fingerprint,
                    build.toolchain_version, build.target_architecture, build.stage,
                    build.artifact_digest, build.failure_stage, build.failure_message
             FROM ci_check_runs check_run
             JOIN ci_builds build ON build.id = check_run.build_id
             WHERE check_run.external_id IS NOT NULL
               AND check_run.last_stage IS DISTINCT FROM build.stage
             ORDER BY build.updated_at, build.id
             LIMIT $1",
        )
        .bind(limit.clamp(1, 256))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn mark_check_run_stage_synced(
        &self,
        build_id: Uuid,
        stage: CiBuildStage,
    ) -> Result<bool> {
        let rows = sqlx::query(
            "UPDATE ci_check_runs check_run SET last_stage = $2, updated_at = now()
             FROM ci_builds build
             WHERE check_run.build_id = $1 AND build.id = check_run.build_id
               AND build.stage = $2 AND check_run.external_id IS NOT NULL",
        )
        .bind(build_id)
        .bind(stage.to_string())
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(rows == 1)
    }

    pub async fn check_run_id(&self, build_id: Uuid) -> Result<Option<i64>> {
        Ok(
            sqlx::query_scalar("SELECT external_id FROM ci_check_runs WHERE build_id = $1")
                .bind(build_id)
                .fetch_optional(&self.pool)
                .await?
                .flatten(),
        )
    }

    /// Serialize external Check Run creation across poller replicas. If a worker
    /// dies during the network call, the lease expires and the next poller uses
    /// GitHub's lookup-before-create path to recover idempotently.
    pub async fn claim_check_run(
        &self,
        build_id: Uuid,
        claim: Uuid,
        lease: Duration,
    ) -> Result<bool> {
        if lease <= Duration::zero() {
            bail!("check-run claim lease must be positive");
        }
        let seconds = lease.num_seconds().max(1);
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO ci_check_runs(id, build_id, repository, commit_sha, last_stage)
             SELECT gen_random_uuid(), id, repository, commit_sha, stage FROM ci_builds WHERE id = $1
             ON CONFLICT(repository, commit_sha) DO UPDATE SET build_id = EXCLUDED.build_id, updated_at = now()",
        ).bind(build_id).execute(&mut *tx).await?;
        let rows = sqlx::query(
            "UPDATE ci_check_runs SET claim_token = $2, claim_expires_at = now() + $3 * interval '1 second', updated_at = now()
             WHERE build_id = $1 AND external_id IS NULL AND (claim_expires_at IS NULL OR claim_expires_at <= now())",
        ).bind(build_id).bind(claim).bind(seconds).execute(&mut *tx).await?.rows_affected();
        tx.commit().await?;
        Ok(rows == 1)
    }

    pub async fn complete_check_run_claim(
        &self,
        build_id: Uuid,
        claim: Uuid,
        external_id: i64,
        external_url: Option<&str>,
        stage: CiBuildStage,
    ) -> Result<bool> {
        let rows = sqlx::query(
            "UPDATE ci_check_runs SET external_id = $3, external_url = $4, last_stage = $5,
                    claim_token = NULL, claim_expires_at = NULL, updated_at = now()
             WHERE build_id = $1 AND claim_token = $2 AND external_id IS NULL",
        )
        .bind(build_id)
        .bind(claim)
        .bind(external_id)
        .bind(external_url)
        .bind(stage.to_string())
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(rows == 1)
    }

    pub async fn release_check_run_claim(&self, build_id: Uuid, claim: Uuid) -> Result<()> {
        sqlx::query("UPDATE ci_check_runs SET claim_token = NULL, claim_expires_at = NULL, updated_at = now() WHERE build_id = $1 AND claim_token = $2 AND external_id IS NULL")
            .bind(build_id).bind(claim).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn record_check_run(
        &self,
        build_id: Uuid,
        external_id: Option<i64>,
        external_url: Option<&str>,
        stage: CiBuildStage,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO ci_check_runs(id, build_id, repository, commit_sha, external_id, external_url, last_stage)
             SELECT gen_random_uuid(), id, repository, commit_sha, $2, $3, $4 FROM ci_builds WHERE id = $1
             ON CONFLICT(repository, commit_sha) DO UPDATE SET build_id = EXCLUDED.build_id,
               external_id = COALESCE(EXCLUDED.external_id, ci_check_runs.external_id),
               external_url = COALESCE(EXCLUDED.external_url, ci_check_runs.external_url),
               last_stage = EXCLUDED.last_stage, claim_token = NULL, claim_expires_at = NULL, updated_at = now()",
        ).bind(build_id).bind(external_id).bind(external_url).bind(stage.to_string()).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn attach_artifact(
        &self,
        build_id: Uuid,
        digest: &str,
        object_key: &str,
        size_bytes: i64,
    ) -> Result<()> {
        if digest.trim().is_empty() || object_key.trim().is_empty() || size_bytes < 0 {
            bail!("invalid CI artifact metadata");
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO ci_artifacts (digest, object_key, size_bytes, expires_at)
             VALUES ($1, $2, $3, now() + interval '72 hours')
             ON CONFLICT (digest) DO UPDATE SET expires_at = GREATEST(ci_artifacts.expires_at, EXCLUDED.expires_at)",
        )
        .bind(digest)
        .bind(object_key)
        .bind(size_bytes)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE ci_builds SET artifact_digest = $2, updated_at = now() WHERE id = $1")
            .bind(build_id)
            .bind(digest)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn attach_claimed_artifact(
        &self,
        build_id: Uuid,
        lease: Uuid,
        digest: &str,
        object_key: &str,
        size_bytes: i64,
    ) -> Result<()> {
        if digest.is_empty()
            || object_key.is_empty()
            || !object_key.starts_with("ci/")
            || size_bytes < 0
        {
            bail!("invalid CI artifact metadata");
        }
        let mut tx = self.pool.begin().await?;
        let owned: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ci_builds WHERE id = $1 AND lease_token = $2 AND lease_expires_at > now() AND stage = 'packaging' FOR UPDATE)")
            .bind(build_id).bind(lease).fetch_one(&mut *tx).await?;
        if !owned {
            bail!("CI build lease is stale while publishing artifact");
        }
        sqlx::query("INSERT INTO ci_artifacts(digest, object_key, size_bytes, expires_at) VALUES($1,$2,$3,now() + interval '72 hours') ON CONFLICT(digest) DO UPDATE SET object_key = EXCLUDED.object_key, size_bytes = EXCLUDED.size_bytes, expires_at = GREATEST(ci_artifacts.expires_at, EXCLUDED.expires_at)")
            .bind(digest).bind(object_key).bind(size_bytes).execute(&mut *tx).await?;
        let updated = sqlx::query("UPDATE ci_builds SET artifact_digest = $3, updated_at = now() WHERE id = $1 AND lease_token = $2 AND lease_expires_at > now() AND stage = 'packaging'")
            .bind(build_id).bind(lease).bind(digest).execute(&mut *tx).await?.rows_affected();
        if updated != 1 {
            bail!("CI build lease was lost while publishing artifact");
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn ensure_preview(
        &self,
        repository: &str,
        commit_sha: &str,
        kind: CiRefKind,
        ref_name: Option<&str>,
        alias: &CiHostname,
        build_id: Uuid,
    ) -> Result<CiPreview> {
        validate_commit_sha(commit_sha).map_err(|error| anyhow::anyhow!(error))?;
        let mut tx = self.pool.begin().await?;
        let existing = sqlx::query_as::<_, CiPreviewRow>(
            "SELECT id, repository, commit_sha, kind, hostname, build_id, state,
                    runtime_generation, NULL::text AS failure_message, ''::text AS build_log, expires_at FROM ci_preview_aliases
             WHERE hostname = $1 FOR UPDATE",
        )
        .bind(&alias.value)
        .fetch_optional(&mut *tx)
        .await?;
        let row = if let Some(existing) = existing {
            let existing_kind: CiRefKind = existing.kind.parse()?;
            if existing_kind == CiRefKind::Commit && existing.commit_sha != commit_sha {
                bail!("immutable commit alias cannot be rebound");
            }
            if existing.commit_sha != commit_sha {
                // Stable PR/branch aliases intentionally remain on the last
                // successful build until the candidate is ready. The caller
                // can expose the candidate through its immutable commit alias.
                tx.commit().await?;
                return existing.try_into();
            }
            sqlx::query_as::<_, CiPreviewRow>(
                "UPDATE ci_preview_aliases SET build_id = $2, ref_name = COALESCE($3, ref_name), updated_at = now()
                 WHERE id = $1 RETURNING id, repository, commit_sha, kind, hostname, build_id, state, runtime_generation, NULL::text AS failure_message, ''::text AS build_log, expires_at",
            ).bind(existing.id).bind(build_id).bind(ref_name).fetch_one(&mut *tx).await?
        } else {
            sqlx::query_as::<_, CiPreviewRow>(
                "INSERT INTO ci_preview_aliases (repository, commit_sha, kind, ref_name, hostname, build_id, state, expires_at)
                 VALUES ($1, $2, $3, $4, $5, $6, CASE WHEN $3 = 'branch' THEN 'lobby' ELSE 'queued' END, now() + interval '72 hours')
                 RETURNING id, repository, commit_sha, kind, hostname, build_id, state, runtime_generation, NULL::text AS failure_message, ''::text AS build_log, expires_at",
            ).bind(repository).bind(commit_sha).bind(kind.as_str()).bind(ref_name).bind(&alias.value).bind(build_id).fetch_one(&mut *tx).await?
        };
        tx.commit().await?;
        row.try_into()
    }

    /// Move a stable alias only if the commit is still the latest observed head
    /// for that exact ref. The conditional UPDATE closes the poll/build race.
    pub async fn advance_stable_alias(
        &self,
        hostname: &str,
        build_id: Uuid,
        ref_kind: CiRefKind,
        ref_name: &str,
    ) -> Result<Option<CiPreview>> {
        let row = sqlx::query_as::<_, CiPreviewRow>(
            "UPDATE ci_preview_aliases alias SET commit_sha = build.commit_sha, build_id = build.id,
                    state = 'ready', expires_at = GREATEST(alias.expires_at, build.expires_at), updated_at = now()
             FROM ci_builds build WHERE alias.hostname = $1 AND alias.kind = $5
               AND build.id = $2 AND build.stage = 'ready'
               AND alias.state NOT IN ('booting', 'running', 'stopping')
               AND NOT EXISTS (
                    SELECT 1 FROM ci_runtime_placements runtime
                    WHERE runtime.preview_id = alias.id AND runtime.active
               )
               AND EXISTS (
                    SELECT 1 FROM ci_refs ref
                    JOIN ci_watched_repositories watched ON watched.id = ref.repository_id
                    WHERE watched.repository = alias.repository AND ref.kind = $3
                      AND ref.name = $4 AND ref.commit_sha = build.commit_sha AND ref.deleted = FALSE)
               AND EXISTS (SELECT 1 FROM ci_artifacts artifact WHERE artifact.digest = build.artifact_digest AND artifact.expires_at > now())
             RETURNING alias.id, alias.repository, alias.commit_sha, alias.kind, alias.hostname,
                       alias.build_id, alias.state, alias.runtime_generation, build.failure_message, build.build_log, alias.expires_at",
        )
        .bind(hostname)
        .bind(build_id)
        .bind(ref_kind.as_str())
        .bind(ref_name)
        .bind(ref_kind.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    pub async fn advance_stable_aliases_for_build(
        &self,
        build_id: Uuid,
        root_domain: &str,
    ) -> Result<u64> {
        let refs: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT refs.kind, refs.name, refs.commit_sha FROM ci_refs refs
             JOIN ci_watched_repositories watched ON watched.id = refs.repository_id
             JOIN ci_builds builds ON builds.repository = watched.repository AND builds.commit_sha = refs.commit_sha
             WHERE builds.id = $1 AND refs.deleted = FALSE",
        ).bind(build_id).fetch_all(&self.pool).await?;
        let mut advanced = 0;
        for (kind, name, commit_sha) in refs {
            let kind = kind.parse::<CiRefKind>()?;
            let alias_kind = match kind {
                CiRefKind::PullRequest => CiRefKind::PullRequest,
                CiRefKind::Branch => CiRefKind::Branch,
                CiRefKind::Commit => continue,
            };
            let alias = CiHostname::for_ref(alias_kind, &name, root_domain)?;
            if let Some(preview) = self
                .advance_stable_alias(&alias.value, build_id, kind, &name)
                .await?
                && preview.commit_sha == commit_sha
            {
                advanced += 1;
            }
        }
        sqlx::query(
            "UPDATE ci_preview_aliases alias SET expires_at = GREATEST(alias.expires_at, build.expires_at), updated_at = now()
             FROM ci_builds build WHERE alias.build_id = build.id AND build.id = $1 AND alias.kind = 'commit' AND build.stage = 'ready'",
        ).bind(build_id).execute(&self.pool).await?;
        Ok(advanced)
    }

    pub async fn lookup_preview_by_id(&self, preview_id: Uuid) -> Result<Option<CiPreview>> {
        let row = sqlx::query_as::<_, CiPreviewRow>(
            "SELECT alias.id, alias.repository, alias.commit_sha, alias.kind, alias.hostname, alias.build_id,
                    CASE WHEN alias.state IN ('running', 'stopping', 'expired') THEN alias.state
                         WHEN alias.kind = 'branch' AND alias.state = 'lobby' THEN 'lobby'
                         WHEN build.stage = 'failed' THEN 'failed'
                         WHEN build.stage = 'expired' THEN 'expired'
                         ELSE build.stage END AS state,
                    alias.runtime_generation, build.failure_message, build.build_log, alias.expires_at
             FROM ci_preview_aliases alias JOIN ci_builds build ON build.id = alias.build_id
             WHERE alias.id = $1 AND alias.expires_at > now()",
        )
        .bind(preview_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    pub async fn lookup_preview(&self, hostname: &str) -> Result<Option<CiPreview>> {
        let row = sqlx::query_as::<_, CiPreviewRow>(
            "SELECT alias.id, alias.repository, alias.commit_sha, alias.kind, alias.hostname, alias.build_id,
                    CASE WHEN alias.state IN ('running', 'stopping', 'expired') THEN alias.state
                         WHEN alias.kind = 'branch' AND alias.state = 'lobby' THEN 'lobby'
                         WHEN build.stage = 'failed' THEN 'failed'
                         WHEN build.stage = 'expired' THEN 'expired'
                         ELSE build.stage END AS state,
                    alias.runtime_generation, build.failure_message, build.build_log, alias.expires_at
             FROM ci_preview_aliases alias JOIN ci_builds build ON build.id = alias.build_id
             WHERE alias.hostname = $1 AND alias.expires_at > now()",
        )
        .bind(hostname)
        .fetch_optional(&self.pool)
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    pub async fn branch_build_request(
        &self,
        preview_id: Uuid,
    ) -> Result<super::services::BuildRequest> {
        let row = sqlx::query_as::<_, (String, String, String, String, String, Option<String>)>(
            "SELECT candidate.repository, ref.commit_sha, candidate.patch_fingerprint,
                    candidate.toolchain_version, candidate.target_architecture, ref.source_repository
             FROM ci_preview_aliases alias
             JOIN ci_watched_repositories watched ON watched.repository = alias.repository
             JOIN ci_refs ref ON ref.repository_id = watched.id AND ref.kind = 'branch'
                 AND ref.name = alias.ref_name AND ref.deleted = FALSE
             JOIN ci_builds current_key ON current_key.id = alias.build_id
             JOIN ci_builds candidate ON candidate.repository = alias.repository
                 AND candidate.commit_sha = ref.commit_sha
                 AND candidate.patch_fingerprint = current_key.patch_fingerprint
                 AND candidate.toolchain_version = current_key.toolchain_version
                 AND candidate.target_architecture = current_key.target_architecture
             WHERE alias.id = $1 AND alias.kind = 'branch' AND alias.state = 'lobby'
               AND alias.expires_at > now()
             ORDER BY candidate.created_at DESC LIMIT 1",
        )
        .bind(preview_id)
        .fetch_one(&self.pool)
        .await?;
        let key = BuildKey::new(row.0, row.1, row.2, row.3, row.4)
            .map_err(|error| anyhow::anyhow!(error))?;
        Ok(super::services::BuildRequest {
            key,
            source_repository: row.5,
            source_archive: Vec::new(),
        })
    }

    /// Resolve the latest observed ref before changing a lazy branch lobby.
    /// The conditional update protects against a concurrent poll advancing the
    /// ref between build-key creation and alias admission.
    pub async fn prepare_lazy_branch_admission(
        &self,
        preview_id: Uuid,
        root_domain: &str,
    ) -> Result<(
        CiPreview,
        Option<super::services::BuildRequest>,
        Option<Uuid>,
    )> {
        for _ in 0..3 {
            let request = match self.branch_build_request(preview_id).await {
                Ok(request) => request,
                Err(error) => {
                    if let Some(current) = self.lookup_preview_by_id(preview_id).await?
                        && current.state != CiPreviewState::Lobby
                    {
                        return Ok((current, None, None));
                    }
                    return Err(error);
                }
            };
            let (build, _) = self.ensure_build(&request.key).await?;
            let commit_alias =
                CiHostname::for_ref(CiRefKind::Commit, &request.key.commit_sha, root_domain)?;
            self.ensure_preview(
                &request.key.repository,
                &request.key.commit_sha,
                CiRefKind::Commit,
                None,
                &commit_alias,
                build.id,
            )
            .await?;
            let row = sqlx::query_as::<_, CiPreviewRow>(
                "UPDATE ci_preview_aliases alias
                 SET commit_sha = $2, build_id = $3,
                     state = CASE WHEN build.stage = 'ready'
                                      AND EXISTS (SELECT 1 FROM ci_artifacts artifact
                                                  WHERE artifact.digest = build.artifact_digest
                                                    AND artifact.expires_at > now())
                                  THEN 'ready' ELSE 'queued' END,
                     expires_at = GREATEST(alias.expires_at, build.expires_at, now() + interval '72 hours'),
                     updated_at = now()
                 FROM ci_builds build
                 WHERE alias.id = $1 AND alias.kind = 'branch' AND alias.state = 'lobby'
                   AND alias.ref_name IS NOT NULL AND build.id = $3
                   AND EXISTS (
                       SELECT 1 FROM ci_watched_repositories watched
                       JOIN ci_refs ref ON ref.repository_id = watched.id
                       WHERE watched.repository = alias.repository AND ref.kind = 'branch'
                         AND ref.name = alias.ref_name AND ref.commit_sha = $2 AND ref.deleted = FALSE
                   )
                 RETURNING alias.id, alias.repository, alias.commit_sha, alias.kind, alias.hostname,
                           alias.build_id, alias.state, alias.runtime_generation, NULL::text AS failure_message,
                           ''::text AS build_log, alias.expires_at",
            )
            .bind(preview_id)
            .bind(&request.key.commit_sha)
            .bind(build.id)
            .fetch_optional(&self.pool)
            .await?;
            if let Some(row) = row {
                return Ok((row.try_into()?, Some(request), Some(build.id)));
            }
            if let Some(current) = self.lookup_preview_by_id(preview_id).await?
                && current.commit_sha == request.key.commit_sha
                && current.state != CiPreviewState::Lobby
            {
                return Ok((current, Some(request), Some(build.id)));
            }
        }
        bail!("branch head changed repeatedly while preparing its first join")
    }

    pub async fn ensure_lazy_branch_build(&self, preview_id: Uuid) -> Result<CiPreview> {
        let row = sqlx::query_as::<_, CiPreviewRow>(
            "UPDATE ci_preview_aliases alias
             SET state = CASE
                    WHEN build.stage = 'ready' AND EXISTS (
                        SELECT 1 FROM ci_artifacts artifact
                        WHERE artifact.digest = build.artifact_digest AND artifact.expires_at > now()
                    ) THEN 'ready'
                    ELSE 'queued'
                 END,
                 updated_at = now()
             FROM ci_builds build WHERE alias.id = $1 AND alias.kind = 'branch'
               AND alias.state = 'lobby' AND build.id = alias.build_id
             RETURNING alias.id, alias.repository, alias.commit_sha, alias.kind, alias.hostname,
                       alias.build_id, alias.state, alias.runtime_generation,
                       build.failure_message, build.build_log, alias.expires_at",
        )
        .bind(preview_id)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(row) = row {
            return row.try_into();
        }
        let row = sqlx::query_as::<_, CiPreviewRow>(
            "SELECT alias.id, alias.repository, alias.commit_sha, alias.kind, alias.hostname, alias.build_id,
                    CASE WHEN alias.state IN ('running', 'stopping', 'expired') THEN alias.state
                         WHEN alias.kind = 'branch' AND alias.state = 'lobby' THEN 'lobby'
                         WHEN build.stage = 'failed' THEN 'failed' WHEN build.stage = 'expired' THEN 'expired'
                         ELSE build.stage END AS state, alias.runtime_generation, build.failure_message, build.build_log, alias.expires_at
             FROM ci_preview_aliases alias JOIN ci_builds build ON build.id = alias.build_id WHERE alias.id = $1",
        )
        .bind(preview_id)
        .fetch_one(&self.pool)
        .await?;
        row.try_into()
    }

    pub async fn mark_preview_running(&self, preview_id: Uuid) -> Result<CiPreview> {
        let row = sqlx::query_as::<_, CiPreviewRow>(
            "UPDATE ci_preview_aliases SET state = 'running', runtime_generation = runtime_generation + 1,
                    updated_at = now() WHERE id = $1 AND state = 'ready'
             RETURNING id, repository, commit_sha, kind, hostname, build_id, state,
                       runtime_generation, NULL::text AS failure_message, ''::text AS build_log, expires_at",
        )
        .bind(preview_id)
        .fetch_one(&self.pool)
        .await?;
        row.try_into()
    }

    pub async fn touch_build(&self, build_id: Uuid) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let digest: Option<String> = sqlx::query_scalar(
            "UPDATE ci_builds SET expires_at = GREATEST(expires_at, now() + interval '72 hours'), updated_at = now()
             WHERE id = $1 RETURNING artifact_digest",
        ).bind(build_id).fetch_optional(&mut *tx).await?.flatten();
        if let Some(digest) = digest {
            sqlx::query("UPDATE ci_artifacts SET expires_at = GREATEST(expires_at, now() + interval '72 hours') WHERE digest = $1")
                .bind(digest).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn touch_preview(&self, preview_id: Uuid) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let build_id: Option<Uuid> = sqlx::query_scalar(
            "UPDATE ci_preview_aliases SET expires_at = GREATEST(expires_at, now() + interval '72 hours'), updated_at = now()
             WHERE id = $1 AND state <> 'expired' RETURNING build_id",
        ).bind(preview_id).fetch_optional(&mut *tx).await?;
        if let Some(build_id) = build_id {
            let digest: Option<String> = sqlx::query_scalar(
                "UPDATE ci_builds SET expires_at = GREATEST(expires_at, now() + interval '72 hours'), updated_at = now()
                 WHERE id = $1 RETURNING artifact_digest",
            ).bind(build_id).fetch_one(&mut *tx).await?;
            if let Some(digest) = digest {
                sqlx::query("UPDATE ci_artifacts SET expires_at = GREATEST(expires_at, now() + interval '72 hours') WHERE digest = $1")
                    .bind(digest).execute(&mut *tx).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn expire(&self, now: DateTime<Utc>) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "UPDATE ci_preview_aliases SET state = 'expired', updated_at = now()
             WHERE expires_at <= $1 AND state <> 'expired'",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE ci_builds SET stage = 'expired', updated_at = now()
             WHERE expires_at <= $1 AND stage <> 'expired'
               AND (lease_expires_at IS NULL OR lease_expires_at <= $1)",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        // Runtime-bearing aliases remain as tombstones until their worker has
        // confirmed world deletion. Build-only aliases and unreferenced builds
        // can be removed in this transaction without orphaning a runtime.
        // If GitHub has been unavailable for a full week after expiration,
        // stop retaining failed logs solely for an external status retry.
        // The regular poller still retries terminal Check Run updates throughout
        // that grace period; after it, the build/check metadata can be collected.
        sqlx::query(
            "DELETE FROM ci_check_runs check_run USING ci_builds build
             WHERE check_run.build_id = build.id AND build.stage = 'expired'
               AND check_run.last_stage IS DISTINCT FROM build.stage
               AND check_run.updated_at <= $1 - interval '7 days'",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM ci_preview_aliases alias
             WHERE alias.state = 'expired'
               AND NOT EXISTS (SELECT 1 FROM ci_runtime_placements runtime WHERE runtime.preview_id = alias.id)",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM ci_builds build
             WHERE build.stage = 'expired'
               AND NOT EXISTS (SELECT 1 FROM ci_preview_aliases alias WHERE alias.build_id = build.id)
               AND NOT EXISTS (
                   SELECT 1 FROM ci_check_runs check_run
                   WHERE check_run.build_id = build.id
                     AND check_run.external_id IS NOT NULL
                     AND check_run.last_stage IS DISTINCT FROM build.stage
               )",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(result.rows_affected())
    }
}

#[derive(Debug, sqlx::FromRow)]
struct CheckRunReconciliationRow {
    build_id: Uuid,
    check_run_id: Option<i64>,
    repository: String,
    commit_sha: String,
    patch_fingerprint: String,
    toolchain_version: String,
    target_architecture: String,
    stage: String,
    artifact_digest: Option<String>,
    failure_stage: Option<String>,
    failure_message: Option<String>,
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
struct WatchedRepositoryRow {
    id: Uuid,
    repository: String,
    default_branch: String,
    enabled: bool,
    etag: Option<String>,
    branch_etag: Option<String>,
    pull_request_page: i32,
    branch_cursor: Option<String>,
    last_successful_poll: Option<DateTime<Utc>>,
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
struct CiRefRecordRow {
    id: Uuid,
    repository_id: Uuid,
    kind: String,
    name: String,
    number: Option<i64>,
    commit_sha: String,
    closed: bool,
    deleted: bool,
    updated_at: DateTime<Utc>,
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
struct CiBuildValidityRow {
    id: Uuid,
    repository: String,
    commit_sha: String,
    patch_fingerprint: String,
    toolchain_version: String,
    target_architecture: String,
    stage: String,
    artifact_digest: Option<String>,
    failure_stage: Option<String>,
    failure_message: Option<String>,
    build_log: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    artifact_valid: bool,
}
impl CiBuildValidityRow {
    fn into_build_row(self) -> CiBuildRow {
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
struct CiBuildRow {
    id: Uuid,
    repository: String,
    commit_sha: String,
    patch_fingerprint: String,
    toolchain_version: String,
    target_architecture: String,
    stage: String,
    artifact_digest: Option<String>,
    failure_stage: Option<String>,
    failure_message: Option<String>,
    build_log: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
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
struct CiPreviewRow {
    id: Uuid,
    repository: String,
    commit_sha: String,
    kind: String,
    hostname: String,
    build_id: Uuid,
    state: String,
    runtime_generation: i64,
    failure_message: Option<String>,
    build_log: String,
    expires_at: DateTime<Utc>,
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

fn validate_repository_name(value: &str) -> Result<()> {
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        bail!("repository must use owner/name form");
    }
    Ok(())
}

#[allow(dead_code)]
fn _build_ttl() -> Duration {
    Duration::hours(BUILD_TTL_HOURS)
}
