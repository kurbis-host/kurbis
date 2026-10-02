use std::sync::Arc;

use anyhow::Result;
use object_store::ObjectStore;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::control_plane::snapshots::{GarbageCollection, SnapshotStore};

#[derive(Debug, Default)]
pub struct MaintenanceReport {
    pub operations_assigned: u64,
    pub exports_assigned: u64,
    pub exports_requeued: u64,
    pub exports_expired: u64,
    pub garbage_collection: GarbageCollection,
}

#[derive(Clone)]
pub struct WorkerMaintenance {
    pool: PgPool,
    object_store: Arc<dyn ObjectStore>,
}

impl WorkerMaintenance {
    pub fn new(pool: PgPool, object_store: Arc<dyn ObjectStore>) -> Self {
        Self { pool, object_store }
    }

    pub async fn run_once(&self) -> Result<MaintenanceReport> {
        let operations_assigned = self.assign_pending_cleanup_operations().await?;
        let exports_requeued = self.requeue_stale_exports().await?;
        let exports_assigned = self.assign_pending_exports().await?;
        let exports_expired = self.expire_exports().await?;
        let garbage_collection =
            SnapshotStore::collect_garbage_with(&self.pool, self.object_store.clone()).await?;
        Ok(MaintenanceReport {
            operations_assigned,
            exports_assigned,
            exports_requeued,
            exports_expired,
            garbage_collection,
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
