use std::sync::Arc;

use object_store::ObjectStore;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

mod export;
mod files;
mod gc;
mod manager;
mod object_key;
mod remote;

#[cfg(test)]
mod tests;

pub use export::WorldArchive;
pub use gc::GarbageCollection;
pub use object_key::{InvalidObjectKey, physical_object_path};
pub(crate) use object_key::{ObjectKind, WorkerObjectKey};
pub use remote::{WorkerObjectAssignment, WorkerObjectClient};

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
    worker_client: Option<WorkerObjectClient>,
}
