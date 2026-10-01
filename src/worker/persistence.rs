use std::path::{Path, PathBuf};

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

/// Files managed by orchestration rather than persistent Minecraft world data.
pub(crate) fn is_runtime_file(relative: &Path) -> bool {
    matches!(
        relative.to_str(),
        Some(
            "pumpkin.toml"
                | "advanced_config.toml"
                | "nethernet-key.der"
                | "whitelist.json"
                | "plugins/kurbis-control.wasm"
                | "plugins/data/kurbis-control/config.json"
        )
    ) || relative
        .components()
        .next()
        .is_some_and(|v| matches!(v.as_os_str().to_str(), Some("logs" | "run")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_configuration_and_logs_are_not_persisted_with_world_data() {
        for path in [
            "pumpkin.toml",
            "advanced_config.toml",
            "nethernet-key.der",
            "whitelist.json",
            "logs/latest.log",
            "run/host-control.sock",
            "plugins/kurbis-control.wasm",
            "plugins/data/kurbis-control/config.json",
        ] {
            assert!(is_runtime_file(Path::new(path)));
        }
        for path in [
            "plugins/other-plugin/plugin.wasm",
            "plugins/data/other-plugin/config.json",
            "plugins/data/other-plugin/whitelist.json",
            "plugins/data/kurbis-control/operator-config.json",
        ] {
            assert!(!is_runtime_file(Path::new(path)));
        }
        assert!(!is_runtime_file(Path::new("world/level.dat")));
    }
}
