use anyhow::{Context, Result};
use sqlx::PgPool;
use uuid::Uuid;

use super::CiPreviewRepository;

#[derive(Debug, Clone)]
pub struct CiGitHubPrDelivery {
    pub delivery_id: Uuid,
    pub repository: String,
    pub action: String,
    pub pull_request_number: u64,
    pub source_repository: String,
    pub base_sha: String,
    pub head_sha: String,
    pub payload_digest: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiGitHubPrDeliveryResult {
    Inserted,
    Duplicate,
    ConflictingReplay,
    AtCapacity,
}

#[derive(Debug, Clone)]
pub struct ClaimedCiGitHubPrDelivery {
    pub delivery_id: Uuid,
    pub claim_token: Uuid,
    pub repository: String,
    pub action: String,
    pub pull_request_number: u64,
    pub source_repository: String,
    pub base_sha: String,
    pub head_sha: String,
}

const MAX_PENDING_DELIVERIES_PER_REPOSITORY: i64 = 5_000;

impl CiPreviewRepository {
    pub async fn record_github_pr_delivery(
        &self,
        delivery: &CiGitHubPrDelivery,
    ) -> Result<CiGitHubPrDeliveryResult> {
        let mut tx = self.pool.begin().await?;
        let repository_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM ci_watched_repositories WHERE repository = $1 AND enabled FOR UPDATE",
        )
        .bind(&delivery.repository)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(repository_id) = repository_id else {
            anyhow::bail!("webhook repository is not enabled for CI builds");
        };
        let existing = sqlx::query_as::<_, (Uuid, Vec<u8>)>(
            "SELECT repository_id, payload_digest FROM ci_github_webhook_deliveries WHERE delivery_id = $1",
        )
        .bind(delivery.delivery_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((existing_repository_id, existing_digest)) = existing {
            tx.commit().await?;
            return Ok(
                if existing_repository_id == repository_id
                    && existing_digest == delivery.payload_digest
                {
                    CiGitHubPrDeliveryResult::Duplicate
                } else {
                    CiGitHubPrDeliveryResult::ConflictingReplay
                },
            );
        }
        let pending: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM ci_github_webhook_deliveries WHERE repository_id = $1 AND processed_at IS NULL",
        )
        .bind(repository_id)
        .fetch_one(&mut *tx)
        .await?;
        if pending >= MAX_PENDING_DELIVERIES_PER_REPOSITORY {
            tx.rollback().await?;
            return Ok(CiGitHubPrDeliveryResult::AtCapacity);
        }
        let inserted: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO ci_github_webhook_deliveries
                (delivery_id, repository_id, action, pull_request_number, source_repository,
                 base_sha, head_sha, payload_digest)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
             ON CONFLICT (delivery_id) DO NOTHING
             RETURNING delivery_id",
        )
        .bind(delivery.delivery_id)
        .bind(repository_id)
        .bind(&delivery.action)
        .bind(i64::try_from(delivery.pull_request_number).context("PR number overflow")?)
        .bind(&delivery.source_repository)
        .bind(&delivery.base_sha)
        .bind(&delivery.head_sha)
        .bind(&delivery.payload_digest)
        .fetch_optional(&mut *tx)
        .await?;
        let result = if inserted.is_some() {
            CiGitHubPrDeliveryResult::Inserted
        } else {
            let stored_digest: Vec<u8> = sqlx::query_scalar(
                "SELECT payload_digest FROM ci_github_webhook_deliveries WHERE delivery_id = $1",
            )
            .bind(delivery.delivery_id)
            .fetch_optional(&mut *tx)
            .await?
            .context("delivery disappeared while checking replay")?;
            if stored_digest == delivery.payload_digest {
                CiGitHubPrDeliveryResult::Duplicate
            } else {
                CiGitHubPrDeliveryResult::ConflictingReplay
            }
        };
        tx.commit().await?;
        Ok(result)
    }

    pub async fn claim_github_pr_deliveries(
        &self,
        limit: i64,
    ) -> Result<Vec<ClaimedCiGitHubPrDelivery>> {
        let claim_token = Uuid::new_v4();
        let rows = sqlx::query_as::<_, ClaimedDeliveryRow>(
            "WITH candidates AS (
                SELECT delivery_id FROM ci_github_webhook_deliveries
                WHERE processed_at IS NULL AND next_attempt_at <= now()
                  AND (claim_expires_at IS NULL OR claim_expires_at <= now())
                ORDER BY received_at, delivery_id
                FOR UPDATE SKIP LOCKED LIMIT $1
             )
             UPDATE ci_github_webhook_deliveries delivery
             SET claim_token = $2, claim_expires_at = now() + interval '3 minutes',
                 attempts = attempts + 1
             FROM candidates, ci_watched_repositories watched
             WHERE delivery.delivery_id = candidates.delivery_id
               AND watched.id = delivery.repository_id
             RETURNING delivery.delivery_id, delivery.action, delivery.pull_request_number,
                       delivery.source_repository, delivery.base_sha, delivery.head_sha,
                       watched.repository",
        )
        .bind(limit.clamp(1, 128))
        .bind(claim_token)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| ClaimedCiGitHubPrDelivery {
                delivery_id: row.delivery_id,
                claim_token,
                repository: row.repository,
                action: row.action,
                pull_request_number: row.pull_request_number as u64,
                source_repository: row.source_repository,
                base_sha: row.base_sha,
                head_sha: row.head_sha,
            })
            .collect())
    }

    pub async fn renew_github_pr_delivery_claim(
        &self,
        delivery_id: Uuid,
        claim_token: Uuid,
    ) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE ci_github_webhook_deliveries
             SET claim_expires_at = now() + interval '3 minutes'
             WHERE delivery_id = $1 AND claim_token = $2 AND processed_at IS NULL",
        )
        .bind(delivery_id)
        .bind(claim_token)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn complete_github_pr_delivery(
        &self,
        delivery_id: Uuid,
        claim_token: Uuid,
    ) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE ci_github_webhook_deliveries
             SET processed_at = now(), claim_token = NULL, claim_expires_at = NULL, last_error = NULL
             WHERE delivery_id = $1 AND claim_token = $2 AND processed_at IS NULL",
        )
        .bind(delivery_id)
        .bind(claim_token)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn retry_github_pr_delivery(
        &self,
        delivery_id: Uuid,
        claim_token: Uuid,
        error: &str,
    ) -> Result<bool> {
        let error = error.chars().take(2_000).collect::<String>();
        let result = sqlx::query(
            "UPDATE ci_github_webhook_deliveries
             SET claim_token = NULL, claim_expires_at = NULL, last_error = $3,
                 next_attempt_at = now() + make_interval(secs => LEAST(300, 5 * power(2, LEAST(attempts, 6))::integer))
             WHERE delivery_id = $1 AND claim_token = $2 AND processed_at IS NULL",
        )
        .bind(delivery_id)
        .bind(claim_token)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn delete_processed_github_pr_deliveries(&self) -> Result<u64> {
        let result = sqlx::query(
            "DELETE FROM ci_github_webhook_deliveries WHERE processed_at < now() - interval '7 days'",
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

#[derive(sqlx::FromRow)]
struct ClaimedDeliveryRow {
    delivery_id: Uuid,
    action: String,
    pull_request_number: i64,
    source_repository: String,
    base_sha: String,
    head_sha: String,
    repository: String,
}

#[allow(dead_code)]
fn _pool_is_send(_: PgPool) {}
