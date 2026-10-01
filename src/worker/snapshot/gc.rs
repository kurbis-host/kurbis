use std::{collections::HashSet, sync::Arc};

use anyhow::{Context, Result, bail};
use futures::TryStreamExt;
use object_store::{ObjectStore, path::Path};
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::object_key::physical_object_path;
use super::object_key::{MAX_MANIFEST_BYTES, ObjectKind, WorkerObjectKey};
use super::{SnapshotManager, SnapshotManifest, manager::validate_manifest_structure};
use crate::model::SnapshotRecord;

#[derive(Debug, Default, Serialize)]
pub struct GarbageCollection {
    pub chunks_deleted: u64,
    pub manifests_deleted: u64,
    pub exports_deleted: u64,
    pub staging_deleted: u64,
}

#[derive(Debug, FromRow)]
struct ExportObject {
    id: Uuid,
    server_id: Uuid,
    attempt: i32,
    status: String,
    worker_id: Option<Uuid>,
    archive_key: Option<String>,
    unexpired: bool,
}

impl SnapshotManager {
    /// Global object listing is a control-plane operation; worker stores expose no LIST.
    pub async fn collect_garbage_with(
        pool: &PgPool,
        store: Arc<dyn ObjectStore>,
    ) -> Result<GarbageCollection> {
        let manager = Self {
            pool: pool.clone(),
            store: store.clone(),
            worker_client: None,
        };
        let mut guard = manager.storage_guard(true).await?;
        let snapshots = sqlx::query_as::<_, SnapshotRecord>(
            "SELECT id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete, created_at
             FROM snapshots WHERE complete",
        )
        .fetch_all(&mut *guard)
        .await?;
        let mut retained = HashSet::new();
        for snapshot in snapshots {
            let manifest_key = WorkerObjectKey::parse(&snapshot.manifest_key)
                .context("retained snapshot has an invalid object key")?;
            if !matches!(manifest_key.kind(), ObjectKind::Manifest { generation, checksum } if generation == snapshot.generation && checksum == snapshot.checksum)
            {
                bail!("retained snapshot object key does not match its generation");
            }
            let physical_manifest =
                physical_object_path(snapshot.server_id, manifest_key.path().as_ref())?;
            let object = store
                .get(&physical_manifest)
                .await
                .context("retained snapshot manifest is missing")?;
            if object.meta.size > MAX_MANIFEST_BYTES {
                bail!("retained snapshot manifest exceeds the maximum object size");
            }
            let bytes = object.bytes().await?;
            if blake3::hash(&bytes).to_hex().as_str() != snapshot.checksum {
                bail!("retained snapshot manifest checksum mismatch");
            }
            let manifest: SnapshotManifest = serde_json::from_slice(&bytes)?;
            if manifest.server_id != snapshot.server_id
                || manifest.generation != snapshot.generation
            {
                bail!("retained snapshot manifest identity mismatch");
            }
            validate_manifest_structure(&manifest)?;
            retained.insert(physical_manifest.to_string());
            for file in manifest.files {
                let chunk_key = format!("chunks/{}", file.blake3);
                let key = WorkerObjectKey::parse(&chunk_key)
                    .context("retained snapshot contains an invalid chunk hash")?;
                if !matches!(key.kind(), ObjectKind::Chunk) {
                    bail!("retained snapshot contains an invalid chunk key");
                }
                retained.insert(
                    physical_object_path(snapshot.server_id, key.path().as_ref())?.to_string(),
                );
            }
        }

        let exports = sqlx::query_as::<_, ExportObject>(
            "SELECT id, server_id, attempt, status, worker_id, archive_key,
                    COALESCE(expires_at > now(), false) AS unexpired
             FROM world_exports
             WHERE status IN ('pending', 'running', 'completed')",
        )
        .fetch_all(&mut *guard)
        .await?;
        for export in exports {
            // Pending rows have no uploaded attempt yet (their attempt starts
            // at zero), so retaining a synthetic `.../0.zip` would fail key
            // validation and could pin an object that was never published.
            // A running assigned row is the only in-flight archive authority.
            let active_attempt = export.status == "running" && export.worker_id.is_some();
            let retained_archive = export.status == "completed" && export.unexpired;
            if !active_attempt && !retained_archive {
                continue;
            }
            let relative_key = if active_attempt {
                let expected = format!("exports/{}/{}.zip", export.id, export.attempt);
                if export
                    .archive_key
                    .as_deref()
                    .is_some_and(|key| key != expected)
                {
                    bail!("active export object key does not match its current attempt");
                }
                expected
            } else {
                let Some(relative_key) = export.archive_key else {
                    continue;
                };
                relative_key
            };
            let key = WorkerObjectKey::parse(&relative_key)
                .context("retained export has an invalid object key")?;
            if !matches!(key.kind(), ObjectKind::Export { export_id, attempt } if export_id == export.id && attempt == export.attempt)
            {
                bail!("retained export object key does not match its active attempt");
            }
            retained
                .insert(physical_object_path(export.server_id, key.path().as_ref())?.to_string());
        }

        let objects: Vec<_> = store
            .list(Some(&Path::from("servers")))
            .try_collect()
            .await?;
        let staged_objects: Vec<_> = store
            .list(Some(&Path::from("control-plane-object-staging")))
            .try_collect()
            .await?;
        let mut report = GarbageCollection::default();
        for object in objects {
            let location = object.location.to_string();
            let Some(key) = parse_physical_key(&location) else {
                continue;
            };
            if retained.contains(&location) {
                continue;
            }
            match store.delete(&object.location).await {
                Ok(()) => match key.kind() {
                    ObjectKind::Chunk => report.chunks_deleted += 1,
                    ObjectKind::Manifest { .. } => report.manifests_deleted += 1,
                    ObjectKind::Export { .. } => report.exports_deleted += 1,
                },
                Err(object_store::Error::NotFound { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        for object in staged_objects {
            match store.delete(&object.location).await {
                Ok(()) => report.staging_deleted += 1,
                Err(object_store::Error::NotFound { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(report)
    }
}

fn parse_physical_key(value: &str) -> Option<WorkerObjectKey> {
    let relative = value.strip_prefix("servers/")?;
    let (server, key) = relative.split_once('/')?;
    let server_id = Uuid::parse_str(server).ok()?;
    if server_id.to_string() != server {
        return None;
    }
    let key = WorkerObjectKey::parse(key).ok()?;
    Some(key)
}
