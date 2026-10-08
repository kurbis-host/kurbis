use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio::time::sleep;
use uuid::Uuid;

use super::super::{
    domain::CiRefKind,
    source_control::{PollCheckpoint, SourceRefKind},
};
use super::rows::WatchedRepositoryRow;
use super::{CiPreviewRepository, WatchedRepository};

const POLL_LEASE_DURATION: Duration = Duration::from_secs(120);
const POLL_LEASE_RENEWAL_INTERVAL: Duration = Duration::from_secs(30);

/// Fences one provider poll without retaining a PostgreSQL transaction during
/// network I/O. Cancellation starts a best-effort asynchronous lease release;
/// the short lease expiry remains the recovery path if the runtime is stopping.
struct PollLease {
    pool: PgPool,
    repository_id: Uuid,
    token: Uuid,
    released: bool,
}

impl PollLease {
    fn new(pool: PgPool, repository_id: Uuid, token: Uuid) -> Self {
        Self {
            pool,
            repository_id,
            token,
            released: false,
        }
    }

    async fn renew(&self) -> Result<()> {
        let result = sqlx::query(
            "UPDATE ci_watched_repositories
             SET poll_lease_expires_at = now() + $3::interval, updated_at = now()
             WHERE id = $1 AND enabled AND poll_lease_token = $2
               AND poll_lease_expires_at > now()",
        )
        .bind(self.repository_id)
        .bind(self.token)
        .bind(format!("{} seconds", POLL_LEASE_DURATION.as_secs()))
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            bail!("CI repository poll lease was lost");
        }
        Ok(())
    }

    async fn release(&mut self) -> Result<()> {
        sqlx::query(
            "UPDATE ci_watched_repositories
             SET poll_lease_token = NULL, poll_lease_expires_at = NULL, updated_at = now()
             WHERE id = $1 AND poll_lease_token = $2",
        )
        .bind(self.repository_id)
        .bind(self.token)
        .execute(&self.pool)
        .await?;
        self.released = true;
        Ok(())
    }

    fn mark_committed(&mut self) {
        self.released = true;
    }

    fn schedule_release(&self) {
        let pool = self.pool.clone();
        let repository_id = self.repository_id;
        let token = self.token;
        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "UPDATE ci_watched_repositories
                 SET poll_lease_token = NULL, poll_lease_expires_at = NULL, updated_at = now()
                 WHERE id = $1 AND poll_lease_token = $2",
            )
            .bind(repository_id)
            .bind(token)
            .execute(&pool)
            .await
            {
                tracing::warn!(%repository_id, ?error, "failed to release cancelled CI repository poll lease");
            }
        });
    }
}

impl Drop for PollLease {
    fn drop(&mut self) {
        if !self.released && tokio::runtime::Handle::try_current().is_ok() {
            self.schedule_release();
        }
    }
}

async fn poll_with_lease<C: crate::ci::source_control::SourceControlClient + ?Sized>(
    lease: &PollLease,
    repository: &str,
    checkpoint: &PollCheckpoint,
    source: &C,
) -> Result<crate::ci::source_control::PollResult> {
    let poll = source.poll(repository, checkpoint);
    tokio::pin!(poll);
    loop {
        tokio::select! {
            result = &mut poll => return result.map_err(anyhow::Error::new),
            _ = sleep(POLL_LEASE_RENEWAL_INTERVAL) => lease.renew().await?,
        }
    }
}

impl CiPreviewRepository {
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
        super::validate_repository_name(repository)?;
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

    /// Poll one repository with a short durable lease. The lease serializes
    /// provider calls across replicas while all database transactions remain
    /// bounded to individual claim, renewal, and persistence statements.
    pub async fn poll_repository<C: crate::ci::source_control::SourceControlClient + ?Sized>(
        &self,
        repository_id: Uuid,
        source: &C,
    ) -> Result<Option<crate::ci::source_control::PollResult>> {
        let token = Uuid::new_v4();
        let row = sqlx::query_as::<_, WatchedRepositoryRow>(
            "UPDATE ci_watched_repositories
             SET poll_lease_token = $2,
                 poll_lease_expires_at = now() + $3::interval, updated_at = now()
             WHERE id = $1 AND enabled
               AND (poll_lease_token IS NULL OR poll_lease_expires_at <= now())
             RETURNING id, repository, default_branch, enabled, etag, branch_etag, pull_request_page,
                       branch_cursor, last_successful_poll",
        )
        .bind(repository_id)
        .bind(token)
        .bind(format!("{} seconds", POLL_LEASE_DURATION.as_secs()))
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut lease = PollLease::new(self.pool.clone(), repository_id, token);
        let checkpoint = PollCheckpoint {
            etag: row.etag.clone(),
            branch_etag: row.branch_etag.clone(),
            pull_request_page: row.pull_request_page.max(1) as u32,
            branch_cursor: row.branch_cursor.clone(),
            last_successful_poll: row.last_successful_poll,
        };
        let mut poll = match poll_with_lease(&lease, &row.repository, &checkpoint, source).await {
            Ok(poll) => poll,
            Err(error) => {
                if let Err(release_error) = lease.release().await {
                    tracing::warn!(%repository_id, ?release_error, "failed to release failed CI repository poll lease");
                }
                return Err(error);
            }
        };
        let mut tx = self.pool.begin().await?;
        sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM ci_watched_repositories
             WHERE id = $1 AND enabled AND poll_lease_token = $2
               AND poll_lease_expires_at > now()
             FOR UPDATE",
        )
        .bind(repository_id)
        .bind(token)
        .fetch_optional(&mut *tx)
        .await?
        .context("CI repository poll lease expired or was superseded")?;
        // A persisted ETag can survive a restart while the provider returns
        // 304 before this process has rebuilt its in-memory response cache.
        // Rehydrate the durable ref cache instead of interpreting the empty
        // response as deletion or starving queued PR builds.
        if poll.not_modified {
            let cached = sqlx::query_as::<_, (String, String, Option<i64>, String, Option<String>, bool, bool, DateTime<Utc>)>(
                "SELECT kind, name, number, commit_sha, source_repository, closed, deleted, updated_at
                 FROM ci_refs WHERE repository_id = $1",
            )
            .bind(repository_id)
            .fetch_all(&mut *tx)
            .await?;
            let mut refs = poll.refs;
            for (kind, name, number, commit_sha, source_repository, closed, deleted, updated_at) in
                cached
            {
                let source_kind = match kind.as_str() {
                    "pull_request" => SourceRefKind::PullRequest,
                    "branch" => SourceRefKind::Branch,
                    _ => continue,
                };
                if refs
                    .iter()
                    .any(|source| source.kind == source_kind && source.name == name)
                {
                    continue;
                }
                refs.push(crate::ci::SourceRef {
                    kind: source_kind,
                    name,
                    number: number.and_then(|value| u64::try_from(value).ok()),
                    commit_sha,
                    source_repository,
                    closed,
                    deleted,
                    updated_at: Some(updated_at),
                });
            }
            poll.refs = crate::ci::source_control::deduplicate_refs(refs);
        }
        // GitHub's updated_at is advisory and may be absent or have only
        // second-level precision. The durable ref row is authoritative: an
        // open PR is eager whenever its observed head differs from the prior
        // persisted head, or when it was closed/deleted and became live again.
        // This also keeps polling correct when a provider repeats an ETag or
        // emits a head replacement in the same timestamp bucket.
        let previous_pr_refs: HashMap<String, (String, bool, bool)> = sqlx::query_as(
            "SELECT name, commit_sha, closed, deleted FROM ci_refs
             WHERE repository_id = $1 AND kind = 'pull_request'",
        )
        .bind(repository_id)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|(name, sha, closed, deleted)| (name, (sha, closed, deleted)))
        .collect();
        poll.eager_refs = poll
            .refs
            .iter()
            .filter(|source_ref| {
                source_ref.kind == SourceRefKind::PullRequest
                    && !source_ref.closed
                    && !source_ref.deleted
                    && previous_pr_refs.get(&source_ref.name).is_none_or(
                        |(previous_sha, was_closed, was_deleted)| {
                            previous_sha != &source_ref.commit_sha || *was_closed || *was_deleted
                        },
                    )
            })
            .cloned()
            .collect();
        let updated = sqlx::query(
            "UPDATE ci_watched_repositories SET etag = $3, branch_etag = $4,
                    pull_request_page = $5, branch_cursor = $6, last_successful_poll = $7,
                    poll_lease_token = NULL, poll_lease_expires_at = NULL, updated_at = now()
             WHERE id = $1 AND poll_lease_token = $2 AND poll_lease_expires_at > now()",
        )
        .bind(repository_id)
        .bind(token)
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
        if updated.rows_affected() != 1 {
            bail!("CI repository poll lease was lost before persistence");
        }
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
        lease.mark_committed();
        Ok(Some(poll))
    }

    /// Expire stable branch aliases after the complete poll has marked their
    /// source ref deleted. PR-close events intentionally do not use this path.
    /// Runtime-bearing aliases remain as tombstones until the assigned worker
    /// confirms generation-fenced world teardown.
    pub async fn expire_deleted_branch_aliases(&self, repository_id: Uuid) -> Result<u64> {
        let result = sqlx::query(
            "UPDATE ci_preview_aliases alias
             SET state = 'expired', expires_at = LEAST(alias.expires_at, now()), updated_at = now()
             FROM ci_watched_repositories watched
             JOIN ci_refs ref ON ref.repository_id = watched.id
             WHERE watched.id = $1 AND watched.repository = alias.repository
               AND ref.kind = 'branch' AND ref.name = alias.ref_name AND ref.deleted
               AND alias.kind = 'branch' AND alias.state <> 'expired'",
        )
        .bind(repository_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}
