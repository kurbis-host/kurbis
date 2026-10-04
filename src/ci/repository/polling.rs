use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use uuid::Uuid;

use super::super::{
    domain::CiRefKind,
    source_control::{PollCheckpoint, SourceRefKind},
};
use super::rows::WatchedRepositoryRow;
use super::{CiPreviewRepository, WatchedRepository};

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
