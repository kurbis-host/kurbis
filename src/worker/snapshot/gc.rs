use std::collections::HashSet;

use anyhow::Result;
use futures::TryStreamExt;
use object_store::path::Path;
use serde::Serialize;

use super::SnapshotManager;
use crate::model::SnapshotRecord;

#[derive(Debug, Default, Serialize)]
pub struct GarbageCollection {
    pub chunks_deleted: u64,
    pub manifests_deleted: u64,
}

impl SnapshotManager {
    /// Collect unreachable objects only after validating every retained manifest.
    /// The Postgres-exclusive guard excludes publishers and restores throughout
    /// marking and sweeping, including cross-worker uploads not yet published.
    pub async fn collect_garbage(&self) -> Result<GarbageCollection> {
        let mut guard = self.storage_guard(true).await?;
        let snapshots = sqlx::query_as::<_, SnapshotRecord>(
            "SELECT id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete, created_at
             FROM snapshots WHERE complete",
        ).fetch_all(&mut *guard).await?;
        let mut chunks = HashSet::new();
        let mut manifests = HashSet::new();
        for snapshot in snapshots {
            let manifest = self.read_manifest(&snapshot).await?;
            manifests.insert(snapshot.manifest_key);
            for file in manifest.files {
                chunks.insert(format!("chunks/{}", file.blake3));
            }
        }
        // Finish all fallible marking/listing before deleting any objects.
        let chunk_objects: Vec<_> = self
            .store
            .list(Some(&Path::from("chunks")))
            .try_collect()
            .await?;
        let manifest_objects: Vec<_> = self
            .store
            .list(Some(&Path::from("snapshots")))
            .try_collect()
            .await?;
        let mut report = GarbageCollection::default();
        for object in chunk_objects {
            if !chunks.contains(object.location.as_ref()) {
                self.store.delete(&object.location).await?;
                report.chunks_deleted += 1;
            }
        }
        for object in manifest_objects {
            if !manifests.contains(object.location.as_ref()) {
                self.store.delete(&object.location).await?;
                report.manifests_deleted += 1;
            }
        }
        Ok(report)
    }
}
