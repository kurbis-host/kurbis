use super::source_control::PollCheckpoint;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::domain::{
    BUILD_TTL_HOURS, BuildKey, CiBuild, CiBuildFailureClass, CiBuildQueueState, CiBuildStage,
    CiHostname, CiPreview, CiPreviewState, CiRefKind, validate_commit_sha,
};

mod github_webhook;
mod polling;
mod rows;
pub use github_webhook::{CiGitHubPrDelivery, CiGitHubPrDeliveryResult, ClaimedCiGitHubPrDelivery};
use rows::*;

/// A failed immutable build key may be retried a small number of times. The
/// retry budget lives on the deduplicated build row, so every poller and API
/// replica observes the same backoff rather than creating a new build.
pub const CI_BUILD_MAX_RETRIES: i32 = 3;

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
    pub tombstone: bool,
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

/// A fenced claim on the mutable desired-build queue. The claim token and
/// expiry are required for every completion so a timed-out worker cannot commit
/// a result after another worker has reclaimed the request.
#[derive(Debug, Clone)]
pub struct CiBuildQueueClaim {
    pub id: Uuid,
    pub claim_token: Uuid,
    pub build_id: Uuid,
    pub pull_request_number: i64,
    pub attempt: i32,
    pub lease_expires_at: DateTime<Utc>,
    pub request: super::services::BuildRequest,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CiBuildQueueMetrics {
    pub pending: i64,
    pub running: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub superseded: i64,
    pub cancelled: i64,
    pub stale_running: i64,
    pub oldest_pending_age_seconds: Option<i64>,
}

#[derive(Clone)]
pub struct CiPreviewRepository {
    pool: PgPool,
}

impl CiPreviewRepository {
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub(crate) fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    pub async fn ensure_build(&self, key: &BuildKey) -> Result<(CiBuild, bool)> {
        self.ensure_build_with_retention(key, super::domain::CiBuildRetention::Preview)
            .await
    }

    /// Ensure the deduplicated immutable build row exists. A permanent request
    /// promotes an existing preview build for the same source/toolchain key so
    /// the main Pumpkin artifact can reuse a completed PR build without a
    /// second fetch, patch, compile, or artifact upload.
    pub async fn ensure_build_with_retention(
        &self,
        key: &BuildKey,
        retention: super::domain::CiBuildRetention,
    ) -> Result<(CiBuild, bool)> {
        key.validate().map_err(|error| anyhow::anyhow!(error))?;
        let mut tx = self.pool.begin().await?;
        let inserted = sqlx::query(
            "INSERT INTO ci_builds (repository, commit_sha, patch_fingerprint, toolchain_version,
                 target_architecture, stage, retention_policy, expires_at)
             VALUES ($1, $2, $3, $4, $5, 'queued', $6,
                     CASE WHEN $6 = 'permanent' THEN NULL ELSE now() + interval '336 hours' END)
             ON CONFLICT (repository, commit_sha, patch_fingerprint, toolchain_version, target_architecture)
             DO NOTHING",
        )
        .bind(&key.repository)
        .bind(&key.commit_sha)
        .bind(&key.patch_fingerprint)
        .bind(&key.toolchain_version)
        .bind(&key.target_architecture)
        .bind(retention.to_string())
        .execute(&mut *tx)
        .await?
        .rows_affected() == 1;
        let mut row: CiBuildValidityRow = sqlx::query_as(
            "SELECT b.id, b.repository, b.commit_sha, b.patch_fingerprint, b.toolchain_version,
                    b.target_architecture, b.stage, b.artifact_digest, b.failure_stage,
                    b.failure_message, b.build_log, b.created_at, b.expires_at,
                    (a.digest IS NOT NULL AND (a.expires_at IS NULL OR a.expires_at > now())) AS artifact_valid
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
        if retention == super::domain::CiBuildRetention::Permanent {
            sqlx::query("UPDATE ci_builds SET retention_policy = 'permanent', expires_at = NULL, updated_at = now() WHERE id = $1")
                .bind(row.id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE ci_artifacts SET expires_at = NULL WHERE digest = (SELECT artifact_digest FROM ci_builds WHERE id = $1)",
            )
            .bind(row.id)
            .execute(&mut *tx)
            .await?;
            row.expires_at = None;
        }
        // A failed immutable key carries its retry budget and permanence class.
        // Preview TTL expiry must not silently turn it into fresh queued work.
        let elapsed = row
            .expires_at
            .is_some_and(|expires_at| expires_at <= Utc::now());
        let stale_success = row.stage == "ready" && !row.artifact_valid;
        let refresh_expired_build = (elapsed && row.stage != "queued" && row.stage != "failed")
            || (retention == super::domain::CiBuildRetention::Permanent && row.stage == "expired");
        let row = if stale_success || refresh_expired_build {
            sqlx::query_as::<_, CiBuildRow>(
                "UPDATE ci_builds SET stage = 'queued', artifact_digest = NULL,
                    failure_stage = NULL, failure_message = NULL, failure_class = NULL, retry_at = NULL,
                    expires_at = CASE WHEN retention_policy = 'permanent' THEN NULL ELSE now() + interval '336 hours' END,
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

    /// Enqueue the current desired build for one pull request. The queue is
    /// keyed by repository and PR number, not by commit, so a new head fences
    /// every older pending/running/retry request as superseded before inserting
    /// the new desired request. Historical terminal rows remain available for
    /// diagnostics and bounded garbage collection.
    pub async fn enqueue_pull_request_build(
        &self,
        repository: &str,
        pull_request_number: i64,
        key: &BuildKey,
        source_repository: Option<&str>,
    ) -> Result<Uuid> {
        if pull_request_number <= 0 {
            bail!("pull request number must be positive");
        }
        if key.repository != repository {
            bail!("queue repository does not match build key repository");
        }
        key.validate().map_err(|error| anyhow::anyhow!(error))?;
        let (build, _) = self.ensure_build(key).await?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout = '2s'")
            .execute(&mut *tx)
            .await?;
        let repository_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM ci_watched_repositories WHERE repository = $1 AND enabled FOR UPDATE",
        )
        .bind(repository)
        .fetch_optional(&mut *tx)
        .await?
        .context("watched CI repository is not enabled")?;
        let lock_key = format!("{repository}:{pull_request_number}");
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(lock_key)
            .execute(&mut *tx)
            .await?;
        let existing: Vec<(Uuid, String, String)> = sqlx::query_as(
            "SELECT id, commit_sha, state FROM ci_build_queue
             WHERE repository_id = $1 AND pull_request_number = $2 FOR UPDATE",
        )
        .bind(repository_id)
        .bind(pull_request_number)
        .fetch_all(&mut *tx)
        .await?;
        if let Some((id, _commit_sha, _state)) = existing
            .iter()
            .find(|(_, commit_sha, state)| commit_sha == &key.commit_sha && state != "superseded")
        {
            sqlx::query(
                "UPDATE ci_build_queue SET source_repository = COALESCE($2, source_repository),
                        requested_at = CASE WHEN state = 'pending' THEN now() ELSE requested_at END,
                        updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(source_repository)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(*id);
        }
        sqlx::query(
            "UPDATE ci_build_queue
             SET state = 'superseded', claim_token = NULL, claim_expires_at = NULL,
                 retry_at = NULL, finished_at = COALESCE(finished_at, now()), updated_at = now()
             WHERE repository_id = $1 AND pull_request_number = $2
               AND state IN ('pending', 'running', 'failed')",
        )
        .bind(repository_id)
        .bind(pull_request_number)
        .execute(&mut *tx)
        .await?;
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO ci_build_queue(repository_id, pull_request_number, commit_sha,
                    build_id, source_repository, state)
             VALUES ($1, $2, $3, $4, $5, 'pending')
             RETURNING id",
        )
        .bind(repository_id)
        .bind(pull_request_number)
        .bind(&key.commit_sha)
        .bind(build.id)
        .bind(source_repository)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Give a queue entry back to a pull-request build that was explicitly
    /// requested (see `mark_build_requested`) but has none, for example after
    /// a coordinator stopped between admission and enqueueing. Build rows that
    /// polling created only so a preview address exists are not requests:
    /// queueing those compiled every open pull request. Idempotent.
    pub async fn recover_unqueued_pull_request_builds(&self, repository: &str) -> Result<u64> {
        let result = sqlx::query(
            "INSERT INTO public.ci_build_queue(
                repository_id, pull_request_number, commit_sha, build_id,
                source_repository, state, retry_at, failure_class, last_error,
                requested_at, started_at, finished_at
             )
             SELECT DISTINCT ON (watched.id, ref.number)
                    watched.id, ref.number, build.commit_sha, build.id,
                    ref.source_repository,
                    CASE WHEN build.stage = 'failed' THEN 'failed' ELSE 'pending' END,
                    build.retry_at, build.failure_class, build.failure_message,
                    build.created_at, NULL, NULL
             FROM public.ci_refs ref
             JOIN public.ci_watched_repositories watched ON watched.id = ref.repository_id
             JOIN public.ci_builds build
               ON build.repository = watched.repository AND build.commit_sha = ref.commit_sha
             WHERE watched.repository = $1 AND watched.enabled
               AND ref.kind = 'pull_request' AND ref.number IS NOT NULL
               AND NOT ref.closed AND NOT ref.deleted
               AND build.requested_at IS NOT NULL
               -- A live lease means a coordinator is building it right now;
               -- queueing it again only records an empty success every poll.
               AND (build.lease_expires_at IS NULL OR build.lease_expires_at <= now())
               AND build.stage IN ('queued', 'fetching', 'patching', 'compiling', 'packaging', 'failed')
               AND (build.stage <> 'failed' OR build.failure_class = 'transient')
               AND NOT EXISTS (
                   SELECT 1 FROM public.ci_build_queue existing
                   WHERE existing.repository_id = watched.id
                     AND existing.pull_request_number = ref.number
                     AND existing.state IN ('pending', 'running', 'failed')
               )
             ORDER BY watched.id, ref.number, build.updated_at DESC, build.id
             ON CONFLICT DO NOTHING",
        )
        .bind(repository)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Reclaim expired queue leases, fence requests whose PR head is no longer
    /// current, and atomically claim a bounded batch. Every transaction is
    /// short; no provider or builder work occurs while rows are locked.
    pub async fn claim_pull_request_builds(
        &self,
        repository: &str,
        limit: i64,
        lease_duration: Duration,
    ) -> Result<Vec<CiBuildQueueClaim>> {
        if !(1..=128).contains(&limit) {
            bail!("invalid CI build queue claim limit");
        }
        if lease_duration <= Duration::zero() {
            bail!("CI build queue lease duration must be positive");
        }
        let seconds = lease_duration.num_seconds().max(1);
        let repository_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM ci_watched_repositories WHERE repository = $1 AND enabled",
        )
        .bind(repository)
        .fetch_optional(&self.pool)
        .await?
        .context("watched CI repository is not enabled")?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout = '2s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE ci_build_queue SET state = 'pending', claim_token = NULL,
                    claim_expires_at = NULL, retry_at = now(),
                    last_error = COALESCE(last_error, 'queue lease expired'), updated_at = now()
             WHERE repository_id = $1 AND state = 'running'
               AND claim_expires_at IS NOT NULL AND claim_expires_at <= now()",
        )
        .bind(repository_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE ci_build_queue queue SET state = 'pending', retry_at = NULL, updated_at = now()
             WHERE queue.repository_id = $1 AND queue.state = 'failed'
               AND queue.failure_class = 'transient' AND queue.retry_at IS NOT NULL
               AND queue.retry_at <= now()",
        )
        .bind(repository_id)
        .execute(&mut *tx)
        .await?;
        // A provider poll persists the authoritative ref row before scheduling
        // its build. If that row says the PR is closed, deleted, or on another
        // head, never waste a worker slot on the stale request.
        sqlx::query(
            "UPDATE ci_build_queue queue SET state = 'superseded', claim_token = NULL,
                    claim_expires_at = NULL, finished_at = COALESCE(finished_at, now()), updated_at = now()
             WHERE queue.repository_id = $1 AND queue.state = 'pending'
               AND EXISTS (
                    SELECT 1 FROM ci_refs ref
                    WHERE ref.repository_id = queue.repository_id AND ref.kind = 'pull_request'
                      AND ref.number = queue.pull_request_number
                      AND (ref.closed OR ref.deleted OR ref.commit_sha <> queue.commit_sha)
               )",
        )
        .bind(repository_id)
        .execute(&mut *tx)
        .await?;
        let claim_token = Uuid::new_v4();
        let rows = sqlx::query_as::<_, CiBuildQueueClaimRow>(
            "WITH candidates AS (
                 SELECT queue.id
                 FROM ci_build_queue queue
                 WHERE queue.repository_id = $1 AND queue.state = 'pending'
                   AND (queue.retry_at IS NULL OR queue.retry_at <= now())
                 ORDER BY queue.requested_at, queue.id
                 FOR UPDATE SKIP LOCKED
                 LIMIT $2
             )
             UPDATE ci_build_queue queue
             SET state = 'running', claim_token = $3,
                 claim_expires_at = now() + $4 * interval '1 second',
                 attempt = queue.attempt + 1, started_at = COALESCE(queue.started_at, now()),
                 updated_at = now()
             FROM candidates, ci_builds build
             WHERE queue.id = candidates.id AND build.id = queue.build_id
             RETURNING queue.id, queue.repository_id::text AS repository, queue.pull_request_number,
                       queue.commit_sha, build.patch_fingerprint, build.toolchain_version,
                       build.target_architecture, queue.build_id, queue.source_repository,
                       queue.state, queue.attempt, queue.claim_token, queue.claim_expires_at",
        )
        .bind(repository_id)
        .bind(limit)
        .bind(claim_token)
        .bind(seconds)
        .fetch_all(&mut *tx)
        .await?;
        // The repository text is selected separately because queue rows store the
        // watched-repository FK as the concurrency boundary.
        let rows = if rows.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as::<_, CiBuildQueueClaimRow>(
                "SELECT queue.id, watched.repository, queue.pull_request_number, queue.commit_sha,
                        build.patch_fingerprint, build.toolchain_version, build.target_architecture,
                        queue.build_id, queue.source_repository, queue.state, queue.attempt,
                        queue.claim_token, queue.claim_expires_at
                 FROM ci_build_queue queue
                 JOIN ci_watched_repositories watched ON watched.id = queue.repository_id
                 JOIN ci_builds build ON build.id = queue.build_id
                 WHERE queue.claim_token = $1 AND queue.state = 'running'
                 ORDER BY queue.requested_at, queue.id",
            )
            .bind(claim_token)
            .fetch_all(&mut *tx)
            .await?
        };
        tx.commit().await?;
        rows.into_iter()
            .map(|row| {
                let claim_token = row.claim_token;
                let lease_expires_at = row.claim_expires_at;
                let key = BuildKey::new(
                    row.repository,
                    row.commit_sha,
                    row.patch_fingerprint,
                    row.toolchain_version,
                    row.target_architecture,
                )?;
                Ok(CiBuildQueueClaim {
                    id: row.id,
                    claim_token,
                    build_id: row.build_id,
                    pull_request_number: row.pull_request_number,
                    attempt: row.attempt,
                    lease_expires_at,
                    request: super::services::BuildRequest {
                        key,
                        source_repository: row.source_repository,
                        source_archive: Vec::new(),
                        retention: super::domain::CiBuildRetention::Preview,
                    },
                })
            })
            .collect()
    }

    pub async fn renew_pull_request_build_claim(
        &self,
        queue_id: Uuid,
        claim_token: Uuid,
        lease_duration: Duration,
    ) -> Result<bool> {
        if lease_duration <= Duration::zero() {
            bail!("CI build queue lease duration must be positive");
        }
        let rows = sqlx::query(
            "UPDATE ci_build_queue SET claim_expires_at = now() + $3 * interval '1 second', updated_at = now()
             WHERE id = $1 AND claim_token = $2 AND state = 'running' AND claim_expires_at > now()",
        )
        .bind(queue_id)
        .bind(claim_token)
        .bind(lease_duration.num_seconds().max(1))
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(rows == 1)
    }

    pub async fn complete_pull_request_build(
        &self,
        queue_id: Uuid,
        claim_token: Uuid,
        succeeded: bool,
        failure_class: Option<CiBuildFailureClass>,
        error: Option<&str>,
    ) -> Result<bool> {
        let (state, retry_sql) = if succeeded {
            (CiBuildQueueState::Succeeded.to_string(), "NULL")
        } else {
            (
                CiBuildQueueState::Failed.to_string(),
                "CASE WHEN $4 = 'transient' THEN now() + CASE WHEN attempt <= 1 THEN interval '1 minute' WHEN attempt = 2 THEN interval '2 minutes' ELSE interval '4 minutes' END ELSE NULL END",
            )
        };
        let error = error.map(|value| value.chars().take(8192).collect::<String>());
        let failure_class = failure_class.map(|value| value.to_string());
        let query = format!(
            "UPDATE ci_build_queue SET state = $3, failure_class = $4, last_error = $5,
                    retry_at = {retry_sql}, claim_token = NULL, claim_expires_at = NULL,
                    finished_at = now(), updated_at = now()
             WHERE id = $1 AND claim_token = $2 AND state = 'running' AND claim_expires_at > now()"
        );
        let rows = sqlx::query(&query)
            .bind(queue_id)
            .bind(claim_token)
            .bind(state)
            .bind(failure_class)
            .bind(error)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(rows == 1)
    }

    pub async fn complete_pull_request_build_from_build_error(
        &self,
        claim: &CiBuildQueueClaim,
        error: &str,
    ) -> Result<bool> {
        let class = sqlx::query_scalar::<_, Option<String>>(
            "SELECT failure_class FROM ci_builds WHERE id = $1",
        )
        .bind(claim.build_id)
        .fetch_optional(&self.pool)
        .await?
        .flatten()
        .and_then(|value| value.parse().ok())
        .unwrap_or(CiBuildFailureClass::Transient);
        self.complete_pull_request_build(
            claim.id,
            claim.claim_token,
            false,
            Some(class),
            Some(error),
        )
        .await
    }

    pub async fn cancel_pull_request_build(&self, queue_id: Uuid) -> Result<bool> {
        let rows = sqlx::query(
            "UPDATE ci_build_queue SET state = 'cancelled', claim_token = NULL,
                    claim_expires_at = NULL, retry_at = NULL, finished_at = now(), updated_at = now()
             WHERE id = $1 AND state IN ('pending', 'running', 'failed')",
        )
        .bind(queue_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(rows == 1)
    }

    pub async fn gc_pull_request_build_queue(&self, repository: &str, limit: i64) -> Result<u64> {
        if !(1..=512).contains(&limit) {
            bail!("invalid CI build queue GC limit");
        }
        let rows = sqlx::query(
            "WITH candidates AS (
                 SELECT queue.id
                 FROM ci_build_queue queue
                 JOIN ci_watched_repositories watched ON watched.id = queue.repository_id
                 WHERE watched.repository = $1
                   AND queue.state IN ('succeeded', 'failed', 'superseded', 'cancelled')
                   AND queue.finished_at IS NOT NULL
                   AND queue.finished_at <= now() - interval '7 days'
                 ORDER BY queue.finished_at, queue.id
                 FOR UPDATE OF queue SKIP LOCKED
                 LIMIT $2
             )
             DELETE FROM ci_build_queue queue USING candidates
             WHERE queue.id = candidates.id",
        )
        .bind(repository)
        .bind(limit)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(rows)
    }

    pub async fn pull_request_build_queue_metrics(
        &self,
        repository: Option<&str>,
    ) -> Result<CiBuildQueueMetrics> {
        let row: (i64, i64, i64, i64, i64, i64, i64, Option<f64>) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE queue.state = 'pending'),
                    count(*) FILTER (WHERE queue.state = 'running'),
                    count(*) FILTER (WHERE queue.state = 'succeeded'),
                    count(*) FILTER (WHERE queue.state = 'failed'),
                    count(*) FILTER (WHERE queue.state = 'superseded'),
                    count(*) FILTER (WHERE queue.state = 'cancelled'),
                    count(*) FILTER (WHERE queue.state = 'running' AND queue.claim_expires_at <= now()),
                    min(EXTRACT(EPOCH FROM (now() - queue.requested_at))::float8) FILTER (WHERE queue.state = 'pending')
             FROM ci_build_queue queue
             JOIN ci_watched_repositories watched ON watched.id = queue.repository_id
             WHERE ($1::text IS NULL OR watched.repository = $1)",
        )
        .bind(repository)
        .fetch_one(&self.pool)
        .await?;
        Ok(CiBuildQueueMetrics {
            pending: row.0,
            running: row.1,
            succeeded: row.2,
            failed: row.3,
            superseded: row.4,
            cancelled: row.5,
            stale_running: row.6,
            oldest_pending_age_seconds: row.7.map(|age| age.max(0.0) as i64),
        })
    }

    /// Return only durable, due retries for current open PR heads. This is
    /// intentionally separate from eagerly observed GitHub changes: unchanged
    /// refs can become due after their original Check Run failure without
    /// making every poll rebuild all queued PRs.
    pub async fn due_pull_request_build_retries(
        &self,
        repository: &str,
    ) -> Result<Vec<super::services::BuildRequest>> {
        let rows = sqlx::query_as::<_, (String, String, String, String, String, Option<String>)>(
            "SELECT build.repository, build.commit_sha, build.patch_fingerprint,
                    build.toolchain_version, build.target_architecture,
                    COALESCE(max(ref_row.source_repository), max(build.repository)) AS source_repository
             FROM ci_builds build
             JOIN ci_refs ref_row ON ref_row.commit_sha = build.commit_sha
             JOIN ci_watched_repositories watched ON watched.id = ref_row.repository_id
             WHERE build.repository = $1 AND watched.repository = build.repository
               AND ref_row.kind = 'pull_request' AND NOT ref_row.closed AND NOT ref_row.deleted
               AND build.stage = 'failed' AND build.failure_class = 'transient'
               AND build.requested_at IS NOT NULL
               AND build.retry_count <= $2 AND build.retry_at <= now()
               AND (build.expires_at IS NULL OR build.expires_at > now())
             GROUP BY build.id
             ORDER BY min(build.retry_at), build.id
             LIMIT 128",
        )
        .bind(repository)
        .bind(CI_BUILD_MAX_RETRIES)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(
                |(
                    repository,
                    commit_sha,
                    patch_fingerprint,
                    toolchain_version,
                    target_architecture,
                    source_repository,
                )| {
                    Ok(super::services::BuildRequest {
                        key: BuildKey::new(
                            repository,
                            commit_sha,
                            patch_fingerprint,
                            toolchain_version,
                            target_architecture,
                        )?,
                        source_repository,
                        source_archive: Vec::new(),
                        retention: super::domain::CiBuildRetention::Preview,
                    })
                },
            )
            .collect()
    }

    /// Return PR-head builds that were durably admitted but have no live build
    /// lease. This is the recovery queue for a poller restart or a control-plane
    /// crash: the source poll may return an unchanged ETag, so rebuilding only
    /// eager refs would strand these rows forever. Branch and arbitrary commit
    /// previews remain lazy and are intentionally excluded.
    pub async fn queued_pull_request_builds(
        &self,
        repository: &str,
    ) -> Result<Vec<super::services::BuildRequest>> {
        let rows = sqlx::query_as::<_, (String, String, String, String, String, Option<String>)>(
            "WITH requested AS (
                 SELECT build.repository, build.commit_sha, build.patch_fingerprint,
                        build.toolchain_version, build.target_architecture,
                        COALESCE(max(ref_row.source_repository), max(build.repository)) AS source_repository,
                        build.requested_at, max(ref_row.updated_at) AS ref_updated_at
                 FROM ci_builds build
                 JOIN ci_refs ref_row ON ref_row.commit_sha = build.commit_sha
                 JOIN ci_watched_repositories watched ON watched.id = ref_row.repository_id
                 WHERE watched.repository = $1 AND watched.enabled
                   AND ref_row.kind = 'pull_request' AND NOT ref_row.closed AND NOT ref_row.deleted
                   AND build.repository = watched.repository
                   AND build.requested_at IS NOT NULL
                   AND build.stage IN ('queued', 'fetching', 'patching', 'compiling', 'packaging')
                   AND (build.expires_at IS NULL OR build.expires_at > now())
                   AND (build.lease_expires_at IS NULL OR build.lease_expires_at <= now())
                 GROUP BY build.id
             ), recent AS (
                 SELECT repository, commit_sha, patch_fingerprint, toolchain_version,
                        target_architecture, source_repository
                 FROM requested
                 WHERE ref_updated_at >= now() - interval '15 minutes'
                 ORDER BY ref_updated_at DESC, requested_at, commit_sha
                 LIMIT 96
             ), aged AS (
                 SELECT repository, commit_sha, patch_fingerprint, toolchain_version,
                        target_architecture, source_repository
                 FROM requested
                 WHERE ref_updated_at < now() - interval '15 minutes' OR ref_updated_at IS NULL
                 ORDER BY requested_at, commit_sha
                 LIMIT 32
             )
             SELECT repository, commit_sha, patch_fingerprint, toolchain_version,
                    target_architecture, source_repository FROM recent
             UNION ALL
             SELECT repository, commit_sha, patch_fingerprint, toolchain_version,
                    target_architecture, source_repository FROM aged"
        )
        .bind(repository)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(
                |(
                    repository,
                    commit_sha,
                    patch_fingerprint,
                    toolchain_version,
                    target_architecture,
                    source_repository,
                )| {
                    Ok(super::services::BuildRequest {
                        key: BuildKey::new(
                            repository,
                            commit_sha,
                            patch_fingerprint,
                            toolchain_version,
                            target_architecture,
                        )?,
                        source_repository,
                        source_archive: Vec::new(),
                        retention: super::domain::CiBuildRetention::Preview,
                    })
                },
            )
            .collect()
    }

    /// A demand-triggered request may start failed immutable work only after
    /// the persisted transient retry deadline. Permanent failures never retry.
    pub async fn is_build_retry_due(&self, build_id: Uuid) -> Result<bool> {
        let due: Option<bool> = sqlx::query_scalar(
            "SELECT stage = 'failed'
                 AND failure_class = 'transient'
                 AND retry_count <= $2
                 AND retry_at IS NOT NULL
                 AND retry_at <= now()
                 AND expires_at > now()
                 AND (lease_expires_at IS NULL OR lease_expires_at <= now())
             FROM ci_builds WHERE id = $1",
        )
        .bind(build_id)
        .bind(CI_BUILD_MAX_RETRIES)
        .fetch_optional(&self.pool)
        .await?;
        Ok(due.unwrap_or(false))
    }

    /// Mark a build as explicitly requested so a poller restart can recover it.
    /// Source polling creates build identities for stable PR aliases, but those
    /// identities are not work until a current-head event, webhook, or preview
    /// join asks the coordinator to run them.
    pub async fn mark_build_requested(&self, build_id: Uuid) -> Result<bool> {
        let updated = sqlx::query(
            "UPDATE ci_builds SET requested_at = COALESCE(requested_at, now()), updated_at = now()
             WHERE id = $1 AND stage NOT IN ('ready', 'expired')",
        )
        .bind(build_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(updated == 1)
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
                 requested_at = COALESCE(requested_at, now()),
                 failure_stage = NULL, failure_message = NULL, failure_class = NULL, retry_at = NULL,
                 updated_at = now()
             WHERE id = $1
               AND (lease_expires_at IS NULL OR lease_expires_at <= now())
               AND (
                    stage IN ('queued', 'fetching', 'patching', 'compiling', 'packaging')
                    OR (stage = 'failed' AND failure_class = 'transient'
                        AND retry_count <= $4 AND retry_at IS NOT NULL AND retry_at <= now())
               )",
        )
        .bind(build_id)
        .bind(lease)
        .bind(format!("{} seconds", lease_duration.num_seconds()))
        .bind(CI_BUILD_MAX_RETRIES)
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
                    failure_class = CASE WHEN $2 = 'failed' THEN 'permanent' ELSE failure_class END,
                    retry_at = CASE WHEN $2 = 'failed' THEN NULL ELSE retry_at END,
                    expires_at = CASE WHEN $2 = 'ready' AND retention_policy = 'preview' THEN now() + interval '336 hours' ELSE expires_at END,
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
        self.fail_build_with_class(
            build_id,
            failure_stage,
            message,
            CiBuildFailureClass::Permanent,
        )
        .await
    }

    pub async fn fail_build_with_class(
        &self,
        build_id: Uuid,
        failure_stage: CiBuildStage,
        message: &str,
        failure_class: CiBuildFailureClass,
    ) -> Result<()> {
        let failure_class = failure_class.to_string();
        sqlx::query(
            "UPDATE ci_builds SET stage = 'failed', failure_stage = $2, failure_message = $3,
                    failure_class = $4,
                    retry_count = CASE WHEN $4 = 'transient' THEN retry_count + 1 ELSE retry_count END,
                    retry_at = CASE
                        WHEN $4 = 'transient' AND retry_count < $5 THEN
                            now() + CASE retry_count
                                WHEN 0 THEN interval '1 minute'
                                WHEN 1 THEN interval '2 minutes'
                                ELSE interval '4 minutes'
                            END
                        ELSE NULL
                    END,
                    lease_token = NULL, lease_expires_at = NULL, updated_at = now()
             WHERE id = $1 AND stage NOT IN ('ready', 'expired')",
        )
        .bind(build_id)
        .bind(failure_stage.to_string())
        .bind(message.chars().take(8192).collect::<String>())
        .bind(failure_class)
        .bind(CI_BUILD_MAX_RETRIES)
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
        self.fail_claimed_build_with_class(
            build_id,
            lease,
            failure_stage,
            message,
            CiBuildFailureClass::Permanent,
        )
        .await
    }

    pub async fn fail_claimed_build_with_class(
        &self,
        build_id: Uuid,
        lease: Uuid,
        failure_stage: CiBuildStage,
        message: &str,
        failure_class: CiBuildFailureClass,
    ) -> Result<bool> {
        let failure_class = failure_class.to_string();
        let rows = sqlx::query(
            "UPDATE ci_builds SET stage = 'failed', failure_stage = $3, failure_message = $4,
                    failure_class = $5,
                    retry_count = CASE WHEN $5 = 'transient' THEN retry_count + 1 ELSE retry_count END,
                    retry_at = CASE
                        WHEN $5 = 'transient' AND retry_count < $6 THEN
                            now() + CASE retry_count
                                WHEN 0 THEN interval '1 minute'
                                WHEN 1 THEN interval '2 minutes'
                                ELSE interval '4 minutes'
                            END
                        ELSE NULL
                    END,
                    updated_at = now()
             WHERE id = $1 AND lease_token = $2 AND lease_expires_at > now()
               AND stage NOT IN ('ready', 'expired')",
        )
        .bind(build_id)
        .bind(lease)
        .bind(failure_stage.to_string())
        .bind(message.chars().take(8192).collect::<String>())
        .bind(failure_class)
        .bind(CI_BUILD_MAX_RETRIES)
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
    ) -> Result<(String, Option<DateTime<Utc>>, Vec<String>)> {
        let expires_at: Option<DateTime<Utc>> =
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
        self.check_run_reconciliation_candidates_filtered(limit, None)
            .await
    }

    /// Reconcile only the watched repository currently being polled. Without
    /// this fence, one poller replica can attempt to update stale Check Runs or
    /// tombstones belonging to another repository/test fixture.
    pub async fn check_run_reconciliation_candidates_for_repository(
        &self,
        limit: i64,
        repository: &str,
    ) -> Result<Vec<CheckRunReconciliation>> {
        self.check_run_reconciliation_candidates_filtered(limit, Some(repository))
            .await
    }

    async fn check_run_reconciliation_candidates_filtered(
        &self,
        limit: i64,
        repository: Option<&str>,
    ) -> Result<Vec<CheckRunReconciliation>> {
        let rows = sqlx::query_as::<_, CheckRunReconciliationRow>(
            "WITH candidates AS (
                SELECT build.id AS build_id, check_run.external_id AS check_run_id,
                       build.repository, build.commit_sha, build.patch_fingerprint,
                       build.toolchain_version, build.target_architecture, build.stage,
                       build.artifact_digest, build.failure_stage, build.failure_message,
                       FALSE AS tombstone, build.updated_at
                FROM ci_check_runs check_run
                JOIN ci_builds build ON build.id = check_run.build_id
                WHERE check_run.external_id IS NOT NULL
                  AND check_run.last_stage IS DISTINCT FROM build.stage
                  AND ($2::text IS NULL OR build.repository = $2)
                UNION ALL
                SELECT tombstone.build_id, tombstone.external_id,
                       tombstone.repository, tombstone.commit_sha, tombstone.patch_fingerprint,
                       tombstone.toolchain_version, tombstone.target_architecture, tombstone.stage,
                       NULL::text AS artifact_digest, NULL::text AS failure_stage,
                       NULL::text AS failure_message, TRUE AS tombstone, tombstone.updated_at
                FROM ci_check_run_tombstones tombstone
                WHERE tombstone.last_stage IS DISTINCT FROM tombstone.stage
                  AND ($2::text IS NULL OR tombstone.repository = $2)
            )
            SELECT build_id, check_run_id, repository, commit_sha, patch_fingerprint,
                   toolchain_version, target_architecture, stage, artifact_digest,
                   failure_stage, failure_message, tombstone
            FROM candidates ORDER BY updated_at, build_id LIMIT $1",
        )
        .bind(limit.clamp(1, 256))
        .bind(repository)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn mark_check_run_stage_synced(
        &self,
        build_id: Uuid,
        stage: CiBuildStage,
        tombstone: bool,
    ) -> Result<bool> {
        let rows = if tombstone {
            sqlx::query(
                "UPDATE ci_check_run_tombstones SET last_stage=$2, updated_at=now()
                 WHERE build_id=$1 AND stage=$2",
            )
            .bind(build_id)
            .bind(stage.to_string())
            .execute(&self.pool)
            .await?
            .rows_affected()
        } else {
            sqlx::query(
                "UPDATE ci_check_runs check_run SET last_stage = $2, updated_at = now()
                 FROM ci_builds build
                 WHERE check_run.build_id = $1 AND build.id = check_run.build_id
                   AND build.stage = $2 AND check_run.external_id IS NOT NULL",
            )
            .bind(build_id)
            .bind(stage.to_string())
            .execute(&self.pool)
            .await?
            .rows_affected()
        };
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
             SELECT $1, $2, $3, CASE WHEN retention_policy = 'permanent' THEN NULL ELSE now() + interval '336 hours' END
             FROM ci_builds WHERE id = $4
             ON CONFLICT (digest) DO UPDATE SET expires_at = CASE
                 WHEN ci_artifacts.expires_at IS NULL OR EXCLUDED.expires_at IS NULL THEN NULL
                 ELSE GREATEST(ci_artifacts.expires_at, EXCLUDED.expires_at)
             END",
        )
        .bind(digest)
        .bind(object_key)
        .bind(size_bytes)
        .bind(build_id)
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
        sqlx::query("INSERT INTO ci_artifacts(digest, object_key, size_bytes, expires_at) SELECT $1, $2, $3, CASE WHEN retention_policy = 'permanent' THEN NULL ELSE now() + interval '336 hours' END FROM ci_builds WHERE id = $4 ON CONFLICT(digest) DO UPDATE SET object_key = EXCLUDED.object_key, size_bytes = EXCLUDED.size_bytes, expires_at = CASE WHEN ci_artifacts.expires_at IS NULL OR EXCLUDED.expires_at IS NULL THEN NULL ELSE GREATEST(ci_artifacts.expires_at, EXCLUDED.expires_at) END")
            .bind(digest).bind(object_key).bind(size_bytes).bind(build_id).execute(&mut *tx).await?;
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
        // PostgreSQL row locks do not lock a missing hostname. Serialize the
        // first insert for each canonical alias so overlapping repository
        // pollers do not race into the unique hostname constraint.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(&alias.value)
            .execute(&mut *tx)
            .await?;
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
                // The same commit gets a new build when the integration
                // patches change. An alias that was ready then has no artifact
                // again, so it goes back to queued and the next join builds it.
                "UPDATE ci_preview_aliases SET build_id = $2, ref_name = COALESCE($3, ref_name), updated_at = now(),
                        state = CASE WHEN state = 'ready' AND NOT EXISTS (
                            SELECT 1 FROM ci_builds build WHERE build.id = $2 AND build.stage = 'ready'
                        ) THEN 'queued' ELSE state END
                 WHERE id = $1 RETURNING id, repository, commit_sha, kind, hostname, build_id, state, runtime_generation, NULL::text AS failure_message, ''::text AS build_log, expires_at",
            ).bind(existing.id).bind(build_id).bind(ref_name).fetch_one(&mut *tx).await?
        } else {
            sqlx::query_as::<_, CiPreviewRow>(
                "INSERT INTO ci_preview_aliases (repository, commit_sha, kind, ref_name, hostname, build_id, state, expires_at)
                 VALUES ($1, $2, $3, $4, $5, $6, CASE WHEN $3 = 'branch' THEN 'lobby' ELSE 'queued' END, now() + interval '336 hours')
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
                    state = 'ready', expires_at = CASE WHEN build.expires_at IS NULL THEN alias.expires_at ELSE GREATEST(alias.expires_at, build.expires_at) END, updated_at = now()
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
               AND EXISTS (SELECT 1 FROM ci_artifacts artifact WHERE artifact.digest = build.artifact_digest AND (artifact.expires_at IS NULL OR artifact.expires_at > now()))
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
            "UPDATE ci_preview_aliases alias SET expires_at = CASE WHEN build.expires_at IS NULL THEN alias.expires_at ELSE GREATEST(alias.expires_at, build.expires_at) END, updated_at = now()
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
            retention: super::domain::CiBuildRetention::Preview,
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
                                                    AND (artifact.expires_at IS NULL OR artifact.expires_at > now()))
                                  THEN 'ready' ELSE 'queued' END,
                     expires_at = CASE WHEN build.expires_at IS NULL THEN GREATEST(alias.expires_at, now() + interval '336 hours') ELSE GREATEST(alias.expires_at, build.expires_at, now() + interval '336 hours') END,
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
                        WHERE artifact.digest = build.artifact_digest AND (artifact.expires_at IS NULL OR artifact.expires_at > now())
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
            "UPDATE ci_builds SET expires_at = CASE WHEN retention_policy = 'permanent' THEN NULL ELSE GREATEST(COALESCE(expires_at, now()), now() + interval '336 hours') END, updated_at = now()
             WHERE id = $1 RETURNING artifact_digest",
        ).bind(build_id).fetch_optional(&mut *tx).await?.flatten();
        if let Some(digest) = digest {
            sqlx::query("UPDATE ci_artifacts SET expires_at = CASE WHEN expires_at IS NULL THEN NULL ELSE GREATEST(expires_at, now() + interval '336 hours') END WHERE digest = $1")
                .bind(digest).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn touch_preview(&self, preview_id: Uuid) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let build_id: Option<Uuid> = sqlx::query_scalar(
            "UPDATE ci_preview_aliases SET expires_at = GREATEST(expires_at, now() + interval '336 hours'), updated_at = now()
             WHERE id = $1 AND state <> 'expired' RETURNING build_id",
        ).bind(preview_id).fetch_optional(&mut *tx).await?;
        if let Some(build_id) = build_id {
            let digest: Option<String> = sqlx::query_scalar(
                "UPDATE ci_builds SET expires_at = CASE WHEN retention_policy = 'permanent' THEN NULL ELSE GREATEST(COALESCE(expires_at, now()), now() + interval '336 hours') END, updated_at = now()
                 WHERE id = $1 RETURNING artifact_digest",
            ).bind(build_id).fetch_one(&mut *tx).await?;
            if let Some(digest) = digest {
                sqlx::query("UPDATE ci_artifacts SET expires_at = CASE WHEN expires_at IS NULL THEN NULL ELSE GREATEST(expires_at, now() + interval '336 hours') END WHERE digest = $1")
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
             WHERE expires_at IS NOT NULL AND expires_at <= $1 AND stage <> 'expired'
               AND (lease_expires_at IS NULL OR lease_expires_at <= $1)",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        // Expired build diagnostics have the same 14-day lifetime as the
        // artifact. Preserve only the small provider reconciliation tombstone
        // if GitHub has not yet accepted the terminal Check Run update.
        sqlx::query(
            "UPDATE ci_builds SET build_log='', failure_message=NULL, updated_at=now()
             WHERE stage='expired' AND expires_at IS NOT NULL AND expires_at <= $1 AND (build_log <> '' OR failure_message IS NOT NULL)",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO ci_check_run_tombstones(
                build_id,repository,commit_sha,patch_fingerprint,toolchain_version,
                target_architecture,external_id,stage,last_stage,updated_at)
             SELECT build.id,build.repository,build.commit_sha,build.patch_fingerprint,
                    build.toolchain_version,build.target_architecture,check_run.external_id,
                    build.stage,check_run.last_stage,now()
             FROM ci_check_runs check_run JOIN ci_builds build ON build.id=check_run.build_id
             WHERE build.stage='expired' AND build.expires_at IS NOT NULL AND build.expires_at <= $1
               AND check_run.external_id IS NOT NULL
               AND check_run.last_stage IS DISTINCT FROM build.stage
             ON CONFLICT(build_id) DO UPDATE SET
                stage=EXCLUDED.stage,last_stage=EXCLUDED.last_stage,updated_at=now()",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM ci_check_runs check_run USING ci_builds build
             WHERE check_run.build_id=build.id AND build.stage='expired' AND build.expires_at IS NOT NULL AND build.expires_at <= $1",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM ci_check_run_tombstones WHERE last_stage=stage")
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
",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(result.rows_affected())
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
