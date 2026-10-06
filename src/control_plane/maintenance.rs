use std::sync::Arc;

use anyhow::{Context, Result};
use object_store::{ObjectStore, path::Path as ObjectPath};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    ci::{CiPreviewRepository, CiPreviewRuntimeRepository},
    control_plane::object_cleanup::delete_if_present,
    control_plane::snapshots::{GarbageCollection, SnapshotStore},
};

/// Finish TTL/branch-deletion cleanup when the assigned worker is offline.
/// Shared checkpoint objects and database ownership are removed here; the
/// returning worker's next reconciliation removes its now-orphaned local disk.
pub async fn cleanup_stale_ci_preview_deletions(
    runtimes: &CiPreviewRuntimeRepository,
    object_store: Arc<dyn ObjectStore>,
    limit: i64,
) -> Result<u64> {
    let claims = runtimes.claim_stale_expired(limit).await?;
    let mut cleaned = 0;
    for claim in claims {
        finish_claimed_stale_ci_preview_deletion(runtimes, object_store.clone(), &claim).await?;
        cleaned += 1;
    }
    Ok(cleaned)
}

/// Complete a previously claimed stale-worker deletion. The caller must pass
/// the exact repository claim; worker routes are fenced while its claim marker
/// exists, and complete_stale_expiry revalidates it in the same transaction
/// that releases ports, disk and capacity.
pub async fn finish_claimed_stale_ci_preview_deletion(
    runtimes: &CiPreviewRuntimeRepository,
    object_store: Arc<dyn ObjectStore>,
    claim: &crate::ci::ExpiredCiRuntime,
) -> Result<()> {
    let placement = &claim.placement;
    let result = async {
        if let Some(snapshot) = runtimes
            .world_snapshot_for_stale_deletion(
                placement.preview_id,
                placement.worker_id,
                placement.generation,
                placement.lease_token,
                claim
                    .stale_cleanup_claim_token
                    .context("stale CI deletion claim omitted its token")?,
            )
            .await?
        {
            let path = ObjectPath::parse(&snapshot.object_key)?;
            delete_if_present(object_store.as_ref(), &path).await?;
        }
        let claim_token = claim
            .stale_cleanup_claim_token
            .context("stale CI deletion claim omitted its token")?;
        if !runtimes
            .complete_stale_expiry(placement, claim_token)
            .await?
        {
            anyhow::bail!("stale CI expiry cleanup lost its generation fence");
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            if let Some(claim_token) = claim.stale_cleanup_claim_token {
                let _ = runtimes
                    .release_stale_expiry_claim(placement, claim_token)
                    .await;
            }
            Err(error).context("finish stale CI preview deletion")
        }
    }
}

/// Drain the durable CI world-snapshot outbox. Database rows are created in
/// the same transaction that supersedes a checkpoint, so transient object-store
/// failures and process crashes cannot orphan superseded snapshots silently.
pub async fn cleanup_ci_world_snapshot_garbage(
    runtimes: &CiPreviewRuntimeRepository,
    object_store: Arc<dyn ObjectStore>,
    limit: i64,
) -> Result<u64> {
    let claims = runtimes.claim_world_snapshot_garbage(limit).await?;
    let mut completed = 0;
    let mut first_error = None;
    for claim in claims {
        let result = async {
            // Protect against a reused key or a delayed stale claim that points
            // at the currently published checkpoint.
            if !runtimes
                .world_snapshot_object_is_current(&claim.object_key)
                .await?
            {
                let path = ObjectPath::parse(&claim.object_key)?;
                delete_if_present(object_store.as_ref(), &path).await?;
            }
            if !runtimes
                .complete_world_snapshot_garbage(&claim.object_key, claim.claim_token)
                .await?
            {
                anyhow::bail!("CI snapshot garbage-collection claim was superseded");
            }
            completed += 1;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            if let Err(release_error) = runtimes
                .release_world_snapshot_garbage(&claim.object_key, claim.claim_token)
                .await
            {
                tracing::error!(?release_error, object_key=%claim.object_key,
                    "failed to release CI snapshot garbage claim after deletion error");
            }
            tracing::warn!(?error, object_key=%claim.object_key,
                "CI world snapshot garbage deletion will be retried");
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error).context("drain CI world snapshot garbage queue");
    }
    Ok(completed)
}

#[derive(Debug, Default)]
pub struct MaintenanceReport {
    pub operations_assigned: u64,
    pub exports_assigned: u64,
    pub exports_requeued: u64,
    pub exports_expired: u64,
    pub garbage_collection: GarbageCollection,
    pub ci_world_snapshot_gc_completed: u64,
    pub github_webhook_processed_gc_deleted: u64,
}

const GITHUB_WEBHOOK_RETENTION_BATCH_SIZE: i64 = 128;

fn webhook_mode_enabled_from_environment() -> bool {
    std::env::var("CI_GITHUB_WEBHOOK_ENABLED")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

#[derive(Clone)]
pub struct WorkerMaintenance {
    pool: PgPool,
    object_store: Arc<dyn ObjectStore>,
    github_webhook_gc_enabled: bool,
}

impl WorkerMaintenance {
    pub fn new(pool: PgPool, object_store: Arc<dyn ObjectStore>) -> Self {
        Self {
            pool,
            object_store,
            github_webhook_gc_enabled: webhook_mode_enabled_from_environment(),
        }
    }

    /// Override the process-level webhook opt-in for an embedding or test.
    /// Retention GC is never active unless this is true.
    pub fn with_github_webhook_enabled(mut self, enabled: bool) -> Self {
        self.github_webhook_gc_enabled = enabled;
        self
    }

    pub async fn run_once(&self) -> Result<MaintenanceReport> {
        let operations_assigned = self.assign_pending_cleanup_operations().await?;
        let exports_requeued = self.requeue_stale_exports().await?;
        let exports_assigned = self.assign_pending_exports().await?;
        let exports_expired = self.expire_exports().await?;
        let garbage_collection =
            SnapshotStore::collect_garbage_with(&self.pool, self.object_store.clone()).await?;
        let ci_world_snapshot_gc_completed = cleanup_ci_world_snapshot_garbage(
            &CiPreviewRuntimeRepository::new(self.pool.clone()),
            self.object_store.clone(),
            64,
        )
        .await?;
        let github_webhook_processed_gc_deleted = if self.github_webhook_gc_enabled {
            CiPreviewRepository::new(self.pool.clone())
                .delete_processed_github_pr_deliveries(GITHUB_WEBHOOK_RETENTION_BATCH_SIZE)
                .await?
        } else {
            0
        };
        Ok(MaintenanceReport {
            operations_assigned,
            exports_assigned,
            exports_requeued,
            exports_expired,
            garbage_collection,
            ci_world_snapshot_gc_completed,
            github_webhook_processed_gc_deleted,
        })
    }

    async fn assign_pending_cleanup_operations(&self) -> Result<u64> {
        let candidates: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, server_id FROM operations
             WHERE worker_id IS NULL AND status = 'pending' AND kind IN ('delete', 'reset')
             ORDER BY created_at, id LIMIT 64",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut assigned = 0;
        for (operation_id, server_id) in candidates {
            let mut tx = self.pool.begin().await?;
            let server_generation: Option<i64> =
                sqlx::query_scalar("SELECT generation FROM servers WHERE id = $1 FOR UPDATE")
                    .bind(server_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            let Some(server_generation) = server_generation else {
                tx.commit().await?;
                continue;
            };
            let operation: Option<(i64, String, String)> = sqlx::query_as(
                "SELECT generation, kind::text, status::text FROM operations
                 WHERE id = $1 AND server_id = $2 AND worker_id IS NULL
                 FOR UPDATE",
            )
            .bind(operation_id)
            .bind(server_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some((generation, kind, status)) = operation else {
                tx.commit().await?;
                continue;
            };
            if generation != server_generation
                || status != "pending"
                || !matches!(kind.as_str(), "delete" | "reset")
            {
                tx.commit().await?;
                continue;
            }
            let Some(worker_id) = eligible_worker(&mut tx).await? else {
                tx.commit().await?;
                continue;
            };
            let result = sqlx::query(
                "UPDATE operations SET worker_id = $2, updated_at = now()
                 WHERE id = $1 AND status = 'pending' AND worker_id IS NULL",
            )
            .bind(operation_id)
            .bind(worker_id)
            .execute(&mut *tx)
            .await?;
            assigned += result.rows_affected();
            tx.commit().await?;
        }
        Ok(assigned)
    }

    async fn requeue_stale_exports(&self) -> Result<u64> {
        let candidates: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT e.id, e.server_id FROM world_exports e
             WHERE e.status = 'running' AND e.source_generation IS NOT NULL
               AND (e.worker_id IS NULL OR EXISTS (
                   SELECT 1 FROM workers w WHERE w.id = e.worker_id
                     AND w.heartbeat_at <= now() - interval '30 seconds'
               ))
             ORDER BY e.updated_at, e.id LIMIT 64",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut requeued = 0;
        for (export_id, server_id) in candidates {
            let mut tx = self.pool.begin().await?;
            sqlx::query("SELECT id FROM servers WHERE id = $1 FOR UPDATE")
                .bind(server_id)
                .fetch_optional(&mut *tx)
                .await?;
            let stale: Option<(Option<Uuid>,)> = sqlx::query_as(
                "SELECT worker_id FROM world_exports
                 WHERE id = $1 AND server_id = $2 AND status = 'running'
                   AND source_generation IS NOT NULL FOR UPDATE",
            )
            .bind(export_id)
            .bind(server_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some((old_worker,)) = stale else {
                tx.commit().await?;
                continue;
            };
            let worker_is_stale = match old_worker {
                Some(worker_id) => {
                    let heartbeat: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
                        "SELECT heartbeat_at FROM workers WHERE id = $1 FOR UPDATE",
                    )
                    .bind(worker_id)
                    .fetch_optional(&mut *tx)
                    .await?;
                    heartbeat
                        .is_none_or(|at| at <= chrono::Utc::now() - chrono::Duration::seconds(30))
                }
                None => true,
            };
            if !worker_is_stale {
                tx.commit().await?;
                continue;
            }
            let result = sqlx::query(
                "UPDATE world_exports
                 SET status = 'pending', stage = 'ready', worker_id = NULL,
                     archive_key = NULL, archive_size = NULL, archive_checksum = NULL,
                     last_error = 'assigned worker heartbeat expired', updated_at = now()
                 WHERE id = $1 AND server_id = $2 AND worker_id IS NOT DISTINCT FROM $3
                   AND status = 'running' AND source_generation IS NOT NULL",
            )
            .bind(export_id)
            .bind(server_id)
            .bind(old_worker)
            .execute(&mut *tx)
            .await?;
            requeued += result.rows_affected();
            tx.commit().await?;
        }
        Ok(requeued)
    }

    async fn assign_pending_exports(&self) -> Result<u64> {
        let candidates: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, server_id FROM world_exports
             WHERE status = 'pending' AND worker_id IS NULL
               AND stage IN ('ready', 'waiting_snapshot')
             ORDER BY created_at, id LIMIT 64",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut assigned = 0;
        for (export_id, server_id) in candidates {
            let mut tx = self.pool.begin().await?;
            let server_generation: Option<i64> =
                sqlx::query_scalar("SELECT generation FROM servers WHERE id = $1 FOR UPDATE")
                    .bind(server_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            let Some(server_generation) = server_generation else {
                tx.commit().await?;
                continue;
            };
            let export: Option<(String, Option<i64>)> = sqlx::query_as(
                "SELECT stage, source_generation FROM world_exports
                 WHERE id = $1 AND server_id = $2 AND status = 'pending' AND worker_id IS NULL
                 FOR UPDATE",
            )
            .bind(export_id)
            .bind(server_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some((stage, source_generation)) = export else {
                tx.commit().await?;
                continue;
            };
            let worker_id = if stage == "waiting_snapshot" && source_generation.is_none() {
                let placement_worker: Option<Uuid> = sqlx::query_scalar(
                    "SELECT worker_id FROM placements
                     WHERE server_id = $1 AND generation = $2 FOR SHARE",
                )
                .bind(server_id)
                .bind(server_generation)
                .fetch_optional(&mut *tx)
                .await?;
                match placement_worker {
                    Some(worker_id) => lock_healthy_worker(&mut tx, worker_id)
                        .await?
                        .map(|_| worker_id),
                    None => None,
                }
            } else if stage == "ready" && source_generation.is_some() {
                eligible_worker(&mut tx).await?
            } else {
                None
            };
            let Some(worker_id) = worker_id else {
                tx.commit().await?;
                continue;
            };
            let result = sqlx::query(
                "UPDATE world_exports SET worker_id = $2, updated_at = now()
                 WHERE id = $1 AND status = 'pending' AND worker_id IS NULL",
            )
            .bind(export_id)
            .bind(worker_id)
            .execute(&mut *tx)
            .await?;
            assigned += result.rows_affected();
            tx.commit().await?;
        }
        Ok(assigned)
    }

    async fn expire_exports(&self) -> Result<u64> {
        let candidates: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, server_id FROM world_exports
             WHERE status = 'completed' AND expires_at <= now()
             ORDER BY expires_at, id LIMIT 128",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut expired = 0;
        for (export_id, server_id) in candidates {
            let mut tx = self.pool.begin().await?;
            sqlx::query("SELECT id FROM servers WHERE id = $1 FOR UPDATE")
                .bind(server_id)
                .fetch_optional(&mut *tx)
                .await?;
            let result = sqlx::query(
                "UPDATE world_exports SET status = 'expired', stage = 'expired',
                     worker_id = NULL, updated_at = now()
                 WHERE id = $1 AND server_id = $2 AND status = 'completed'
                   AND expires_at <= now()",
            )
            .bind(export_id)
            .bind(server_id)
            .execute(&mut *tx)
            .await?;
            expired += result.rows_affected();
            tx.commit().await?;
        }
        Ok(expired)
    }
}

async fn eligible_worker(tx: &mut Transaction<'_, Postgres>) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar(
        "SELECT w.id FROM workers w
         JOIN worker_credentials credential ON credential.worker_id = w.id
         WHERE credential.revoked_at IS NULL AND NOT w.draining
           AND w.heartbeat_at > now() - interval '30 seconds'
           AND w.signal_address <> ''
         ORDER BY w.id
         FOR UPDATE OF w SKIP LOCKED LIMIT 1",
    )
    .fetch_optional(&mut **tx)
    .await?)
}

async fn lock_healthy_worker(
    tx: &mut Transaction<'_, Postgres>,
    worker_id: Uuid,
) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar(
        "SELECT w.id FROM workers w
         JOIN worker_credentials credential ON credential.worker_id = w.id
         WHERE w.id = $1 AND credential.revoked_at IS NULL AND NOT w.draining
           AND w.heartbeat_at > now() - interval '30 seconds'
           AND w.signal_address <> ''
         FOR UPDATE OF w",
    )
    .bind(worker_id)
    .fetch_optional(&mut **tx)
    .await?)
}

pub fn spawn_worker_maintenance(maintenance: WorkerMaintenance) {
    tokio::spawn(async move {
        loop {
            match maintenance.run_once().await {
                Ok(report) => {
                    if report.operations_assigned > 0
                        || report.exports_assigned > 0
                        || report.exports_requeued > 0
                        || report.exports_expired > 0
                        || report.garbage_collection.chunks_deleted > 0
                        || report.garbage_collection.manifests_deleted > 0
                        || report.garbage_collection.exports_deleted > 0
                        || report.garbage_collection.staging_deleted > 0
                        || report.ci_world_snapshot_gc_completed > 0
                        || report.github_webhook_processed_gc_deleted > 0
                    {
                        tracing::info!(?report, "worker maintenance completed");
                    }
                }
                Err(error) => tracing::error!(?error, "worker maintenance failed"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
}
