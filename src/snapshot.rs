use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(crate) mod files;
pub(crate) mod object_key;

pub use object_key::{InvalidObjectKey, physical_object_path};
pub(crate) use object_key::{ObjectKind, WorkerObjectKey};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub server_id: Uuid,
    pub generation: i64,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestFile {
    pub path: String,
    pub size: u64,
    pub blake3: String,
}

pub const SNAPSHOT_RETENTION_GENERATIONS: i64 = crate::model::SNAPSHOT_RETENTION_GENERATIONS;

/// Files managed by orchestration rather than persistent Minecraft world data.
pub(crate) fn is_runtime_file(relative: &Path) -> bool {
    matches!(
        relative.to_str(),
        Some(
            "pumpkin.toml"
                | "advanced_config.toml"
                | "nethernet-key.der"
                | "whitelist.json"
                | "ops.json"
                | "data/whitelist.json"
                | "plugins/kurbis-control.wasm"
                | "plugins/data/kurbis-control/config.json"
        )
    ) || relative
        .components()
        .next()
        .is_some_and(|v| matches!(v.as_os_str().to_str(), Some("logs" | "run")))
        // The worker writes the installed plugins from the server's plugin
        // list, and Pumpkin's compiled-plugin cache is specific to the host.
        || relative.starts_with("plugins/installed")
        || relative.starts_with("plugins/cache")
}

/// Persistence providers that can validate and restore the authoritative server snapshot.
#[async_trait]
pub trait SnapshotSource: Sync {
    async fn local_matches_current(&self, server_id: Uuid, root: &Path) -> Result<bool>;
    async fn restore_latest(&self, server_id: Uuid, root: &Path) -> Result<Option<i64>>;
}
