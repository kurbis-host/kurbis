use std::path::PathBuf;

use anyhow::{Result, bail};
use sqlx::PgPool;
use uuid::Uuid;

use super::{snapshot::SnapshotManager, storage::StorageManager};

/// Prepare stopped-server data from the authoritative completed snapshot.
/// Callers must prove no runtime is using the directory before invoking this.
pub async fn prepare_server_data(
    pool: &PgPool,
    worker_id: Uuid,
    server_id: Uuid,
    expected_generation: Option<i64>,
    storage: &StorageManager,
    snapshots: &SnapshotManager,
) -> Result<PathBuf> {
    let directory = storage.ensure(server_id).await?;
    let cached: Option<i64> = sqlx::query_scalar(
        "SELECT generation FROM worker_snapshot_cache WHERE worker_id = $1 AND server_id = $2",
    )
    .bind(worker_id)
    .bind(server_id)
    .fetch_optional(pool)
    .await?;
    if expected_generation.is_some()
        && cached == expected_generation
        && snapshots
            .local_matches_current(server_id, &directory)
            .await?
    {
        return Ok(directory);
    }
    sqlx::query("DELETE FROM worker_snapshot_cache WHERE worker_id = $1 AND server_id = $2")
        .bind(worker_id)
        .bind(server_id)
        .execute(pool)
        .await?;
    storage.reset(server_id).await?;
    let restored = snapshots.restore_latest(server_id, &directory).await?;
    if restored != expected_generation {
        bail!("restored snapshot generation does not match server current snapshot");
    }
    if let Some(generation) = restored {
        sqlx::query(
            "INSERT INTO worker_snapshot_cache(worker_id, server_id, generation) VALUES($1,$2,$3)",
        )
        .bind(worker_id)
        .bind(server_id)
        .bind(generation)
        .execute(pool)
        .await?;
    }
    Ok(directory)
}
