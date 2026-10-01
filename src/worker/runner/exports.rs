use std::{
    path::{Path as FsPath, PathBuf},
    time::Duration,
};

use anyhow::Result;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::WorkerRunner;
use crate::worker::snapshot::SnapshotManager;

#[derive(Debug, Clone, FromRow)]
struct WorldExportTask {
    id: Uuid,
    server_id: Uuid,
    source_generation: i64,
    attempt: i32,
}

#[derive(Debug, FromRow)]
struct ExpiredArchive {
    id: Uuid,
    archive_key: String,
}

pub async fn recover_running_exports(pool: &PgPool, worker_id: Uuid) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE world_exports SET status = 'pending', worker_id = NULL,
             stage = CASE WHEN source_generation IS NULL THEN 'waiting_snapshot' ELSE 'ready' END,
             updated_at = now()
         WHERE status = 'running' AND worker_id = $1",
    )
    .bind(worker_id)
    .execute(pool)
    .await?
    .rows_affected())
}

impl WorkerRunner {
    async fn claim_world_export(&self) -> Result<Option<WorldExportTask>> {
        let mut tx = self.pool.begin().await?;
        let task = sqlx::query_as::<_, WorldExportTask>(
            "SELECT id, server_id, source_generation, attempt
             FROM world_exports
             WHERE status = 'pending' AND stage = 'ready' AND source_generation IS NOT NULL
             ORDER BY created_at, id
             FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(mut task) = task else {
            tx.commit().await?;
            return Ok(None);
        };
        task.attempt += 1;
        sqlx::query(
            "UPDATE world_exports SET status = 'running', worker_id = $2,
                 attempt = $3, stage = 'archiving', updated_at = now()
             WHERE id = $1 AND status = 'pending'",
        )
        .bind(task.id)
        .bind(self.config.id)
        .bind(task.attempt)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(task))
    }

    pub(super) async fn expire_world_exports(&self) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let expired = sqlx::query_as::<_, ExpiredArchive>(
            "SELECT id, archive_key FROM world_exports
             WHERE status IN ('completed', 'expired') AND expires_at <= now()
               AND archive_key IS NOT NULL
             ORDER BY expires_at FOR UPDATE SKIP LOCKED LIMIT 25",
        )
        .fetch_all(&mut *tx)
        .await?;
        for archive in &expired {
            sqlx::query(
                "UPDATE world_exports SET status = 'expired', stage = 'expired', updated_at = now()
                 WHERE id = $1 AND status IN ('completed', 'expired')",
            )
            .bind(archive.id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;

        for archive in expired {
            match self
                .snapshots
                .delete_world_archive(&archive.archive_key)
                .await
            {
                Ok(()) => {
                    sqlx::query(
                        "UPDATE world_exports SET archive_key = NULL, updated_at = now()
                         WHERE id = $1 AND status = 'expired' AND archive_key = $2",
                    )
                    .bind(archive.id)
                    .bind(archive.archive_key)
                    .execute(&self.pool)
                    .await?;
                }
                Err(error) => {
                    tracing::warn!(export_id = %archive.id, ?error, "expired archive cleanup deferred")
                }
            }
        }
        Ok(())
    }

    pub(super) async fn recover_stale_world_exports(&self) -> Result<()> {
        sqlx::query(
            "WITH stale AS (
                SELECT e.id FROM world_exports e
                LEFT JOIN workers w ON w.id = e.worker_id
                WHERE e.status = 'running'
                  AND e.worker_id IS DISTINCT FROM $1
                  AND (w.id IS NULL OR w.heartbeat_at <= now() - interval '30 seconds')
                FOR UPDATE OF e SKIP LOCKED
             )
             UPDATE world_exports e SET status = 'pending', worker_id = NULL,
                 stage = CASE WHEN e.source_generation IS NULL THEN 'waiting_snapshot' ELSE 'ready' END,
                 updated_at = now()
             FROM stale WHERE e.id = stale.id",
        )
        .bind(self.config.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub(super) async fn expire_stale_exports_if_due(&mut self) -> Result<()> {
        if self.last_export_cleanup.elapsed() >= Duration::from_secs(60) {
            self.expire_world_exports().await?;
            self.recover_stale_world_exports().await?;
            self.last_export_cleanup = tokio::time::Instant::now();
        }
        Ok(())
    }

    pub(super) async fn spawn_world_export_if_idle(&mut self) -> Result<()> {
        if let Some(task) = self.world_export_task.take() {
            if task.is_finished() {
                if let Err(error) = task.await {
                    tracing::error!(?error, "world export task panicked");
                }
            } else {
                self.world_export_task = Some(task);
                return Ok(());
            }
        }
        let Some(task) = self.claim_world_export().await? else {
            return Ok(());
        };
        let pool = self.pool.clone();
        let snapshots = self.snapshots.clone();
        let worker_id = self.config.id;
        let staging_root = self.config.root.join("exports");
        self.world_export_task = Some(tokio::spawn(async move {
            if let Err(error) =
                run_world_export(pool, snapshots, task, worker_id, staging_root).await
            {
                tracing::error!(?error, "world export worker failed unexpectedly");
            }
        }));
        Ok(())
    }
}

async fn run_world_export(
    pool: PgPool,
    snapshots: SnapshotManager,
    task: WorldExportTask,
    worker_id: Uuid,
    staging_root: PathBuf,
) -> Result<()> {
    tokio::fs::create_dir_all(&staging_root).await?;
    snapshots
        .remove_stale_world_archives(task.server_id, task.id)
        .await?;
    let stale_file = staging_root.join(format!("{}-{}.zip.part", task.id, task.attempt));
    let _ = tokio::fs::remove_file(&stale_file).await;

    let result = build_upload_complete(&pool, &snapshots, &task, worker_id, &staging_root).await;
    if let Err(error) = result {
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM world_exports WHERE id = $1")
                .bind(task.id)
                .fetch_optional(&pool)
                .await?;
        if status.as_deref() == Some("running") {
            sqlx::query(
                "UPDATE world_exports SET status = 'failed', stage = 'failed',
                     last_error = $3, updated_at = now()
                 WHERE id = $1 AND status = 'running' AND worker_id = $2 AND attempt = $4",
            )
            .bind(task.id)
            .bind(worker_id)
            .bind(format!("{error:#}").chars().take(1000).collect::<String>())
            .bind(task.attempt)
            .execute(&pool)
            .await?;
        }
        let _ = tokio::fs::remove_file(stale_file).await;
        return Ok(());
    }
    Ok(())
}

async fn build_upload_complete(
    pool: &PgPool,
    snapshots: &SnapshotManager,
    task: &WorldExportTask,
    worker_id: Uuid,
    staging_root: &FsPath,
) -> Result<()> {
    let archive = snapshots
        .create_world_archive(
            task.id,
            task.server_id,
            task.source_generation,
            task.attempt,
            staging_root,
        )
        .await?;
    let result = snapshots.upload_world_archive(&archive).await;
    if let Err(error) = result {
        let _ = snapshots.delete_world_archive(&archive.archive_key).await;
        snapshots.remove_world_archive_staging(&archive).await;
        return Err(error);
    }
    let update = sqlx::query(
        "UPDATE world_exports SET status = 'completed', stage = 'completed',
             archive_key = $5, archive_size = $6, archive_checksum = $7,
             expires_at = now() + interval '24 hours', last_error = NULL, updated_at = now()
         WHERE id = $1 AND server_id = $2 AND status = 'running' AND worker_id = $3
           AND attempt = $4 AND source_generation = $8",
    )
    .bind(task.id)
    .bind(task.server_id)
    .bind(worker_id)
    .bind(task.attempt)
    .bind(&archive.archive_key)
    .bind(archive.size_bytes)
    .bind(&archive.checksum)
    .bind(task.source_generation)
    .execute(pool)
    .await;
    let update = match update {
        Ok(update) => update,
        Err(error) => {
            let _ = snapshots.delete_world_archive(&archive.archive_key).await;
            snapshots.remove_world_archive_staging(&archive).await;
            return Err(error.into());
        }
    };
    if update.rows_affected() == 0 {
        snapshots.delete_world_archive(&archive.archive_key).await?;
    }
    snapshots.remove_world_archive_staging(&archive).await;
    Ok(())
}
