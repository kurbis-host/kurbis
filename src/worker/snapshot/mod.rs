use std::sync::Arc;

use object_store::ObjectStore;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

mod export;
mod files;
mod gc;
mod manager;

#[cfg(test)]
mod tests;

pub use export::WorldArchive;
pub use gc::GarbageCollection;

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

#[derive(Clone)]
pub struct SnapshotManager {
    pool: PgPool,
    store: Arc<dyn ObjectStore>,
}
