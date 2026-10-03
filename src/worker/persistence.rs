use std::path::PathBuf;

use crate::snapshot::SnapshotSource;
use anyhow::{Result, bail};
use uuid::Uuid;

use super::storage::StorageManager;

/// Prepare stopped-server data from the authoritative completed snapshot.
/// Callers must prove no runtime is using the directory before invoking this.
pub async fn prepare_server_data<S: SnapshotSource>(
    server_id: Uuid,
    expected_generation: Option<i64>,
    storage: &StorageManager,
    snapshots: &S,
) -> Result<PathBuf> {
    let directory = storage.ensure(server_id).await?;
    if expected_generation.is_some()
        && snapshots
            .local_matches_current(server_id, &directory)
            .await?
    {
        return Ok(directory);
    }
    storage.reset(server_id).await?;
    let restored = snapshots.restore_latest(server_id, &directory).await?;
    if restored != expected_generation {
        bail!("restored snapshot generation does not match server current snapshot");
    }
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::snapshot::is_runtime_file;

    #[test]
    fn runtime_configuration_and_logs_are_not_persisted_with_world_data() {
        for path in [
            "pumpkin.toml",
            "advanced_config.toml",
            "nethernet-key.der",
            "whitelist.json",
            "ops.json",
            "data/whitelist.json",
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
        assert!(!is_runtime_file(Path::new("data/ops.json")));
        assert!(!is_runtime_file(Path::new("world/level.dat")));
    }
}
