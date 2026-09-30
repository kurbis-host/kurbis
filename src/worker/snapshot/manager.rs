use std::{path::Path as FsPath, sync::Arc};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use object_store::{ObjectStore, aws::AmazonS3Builder, local::LocalFileSystem, path::Path};
use sqlx::{Connection, PgConnection, PgPool, Postgres, pool::PoolConnection};
use uuid::Uuid;

use crate::{
    config::ObjectStoreConfig,
    model::{SERVER_STORAGE_BYTES, SNAPSHOT_RETENTION_GENERATIONS, SnapshotReason, SnapshotRecord},
};

use super::files::{collect_files, safe_relative_path};
use super::{ManifestFile, SnapshotManager, SnapshotManifest};

impl SnapshotManager {
    pub(super) async fn storage_guard(&self, exclusive: bool) -> Result<PoolConnection<Postgres>> {
        let mut connection = self.pool.acquire().await?;
        connection.close_on_drop();
        let query = if exclusive {
            "SELECT pg_advisory_lock(724515, 0)"
        } else {
            "SELECT pg_advisory_lock_shared(724515, 0)"
        };
        sqlx::query(query).execute(&mut *connection).await?;
        Ok(connection)
    }
    pub async fn new(pool: PgPool, config: &ObjectStoreConfig) -> Result<Self> {
        let store: Arc<dyn ObjectStore> = match config {
            ObjectStoreConfig::Local { root } => {
                tokio::fs::create_dir_all(root).await?;
                Arc::new(LocalFileSystem::new_with_prefix(root)?)
            }
            ObjectStoreConfig::S3 {
                bucket,
                region,
                endpoint,
                access_key,
                secret_key,
                allow_http,
            } => {
                let mut builder = AmazonS3Builder::new()
                    .with_bucket_name(bucket)
                    .with_region(region)
                    .with_access_key_id(access_key)
                    .with_secret_access_key(secret_key)
                    .with_allow_http(*allow_http);
                if let Some(endpoint) = endpoint {
                    builder = builder.with_endpoint(endpoint);
                }
                Arc::new(builder.build()?)
            }
        };
        Ok(Self { pool, store })
    }

    pub async fn create(
        &self,
        server_id: Uuid,
        root: &FsPath,
        reason: SnapshotReason,
    ) -> Result<SnapshotRecord> {
        // Serialize allocation, upload, publication and retention across workers.
        // Releasing the allocator lock before upload allowed older uploads to
        // overwrite the current pointer and race retention of their manifests.
        let mut publication_guard = self.storage_guard(false).await?;
        // A session lock survives the short transactions that persist partial
        // metadata. Always close this connection on drop, including cancellation
        // and errors, so no advisory lock can leak into the connection pool.
        sqlx::query("SELECT pg_advisory_lock(724514, hashtext($1))")
            .bind(server_id.to_string())
            .execute(&mut *publication_guard)
            .await?;
        let generation = self
            .allocate_generation(&mut publication_guard, server_id, reason)
            .await?;
        let root = root.to_path_buf();
        let files = tokio::task::spawn_blocking(move || collect_files(&root))
            .await
            .context("snapshot scan task failed")??;

        let scanned_bytes = files.iter().try_fold(0_u64, |total, file| {
            total
                .checked_add(file.size)
                .context("snapshot size overflow")
        })?;
        if scanned_bytes > SERVER_STORAGE_BYTES as u64 {
            bail!("server storage quota exceeded");
        }

        let mut manifest_files = Vec::with_capacity(files.len());
        let mut total_bytes = 0_u64;
        for file in files {
            let bytes = tokio::fs::read(&file.absolute).await?;
            let size = bytes.len() as u64;
            total_bytes = total_bytes
                .checked_add(size)
                .context("snapshot size overflow")?;
            if total_bytes > SERVER_STORAGE_BYTES as u64 {
                bail!("server storage quota exceeded");
            }
            let hash = blake3::hash(&bytes).to_hex().to_string();
            let chunk_path = Path::from(format!("chunks/{hash}"));
            match self.store.head(&chunk_path).await {
                Ok(_) => {}
                Err(object_store::Error::NotFound { .. }) => {
                    self.store
                        .put(&chunk_path, Bytes::from(bytes).into())
                        .await?;
                }
                Err(error) => return Err(error.into()),
            }
            let manifest_file = ManifestFile {
                path: file.relative,
                size,
                blake3: hash,
            };
            // Existence alone cannot prove a reused or newly uploaded chunk is
            // intact. Validate stored bytes before making this generation usable.
            self.read_verified_chunk(&manifest_file).await?;
            manifest_files.push(manifest_file);
        }
        manifest_files.sort_by(|a, b| a.path.cmp(&b.path));
        let manifest = SnapshotManifest {
            server_id,
            generation,
            files: manifest_files,
        };
        let manifest_bytes = serde_json::to_vec(&manifest)?;
        let checksum = blake3::hash(&manifest_bytes).to_hex().to_string();
        let manifest_key = format!("snapshots/{server_id}/{generation}-{checksum}.json");
        self.store
            .put(
                &Path::from(manifest_key.clone()),
                Bytes::from(manifest_bytes).into(),
            )
            .await?;
        self.validate_manifest_object(&manifest_key, &checksum)
            .await?;

        let total_bytes = i64::try_from(total_bytes)?;
        let mut tx = publication_guard.begin().await?;
        sqlx::query("SELECT id FROM servers WHERE id = $1 FOR UPDATE")
            .bind(server_id)
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE snapshots
             SET manifest_key = $3, size_bytes = $4, checksum = $5, complete = true
             WHERE server_id = $1 AND generation = $2 AND NOT complete",
        )
        .bind(server_id)
        .bind(generation)
        .bind(&manifest_key)
        .bind(total_bytes)
        .bind(&checksum)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE servers SET current_snapshot_generation = $2, storage_bytes = $3, updated_at = now()
             WHERE id = $1 AND (current_snapshot_generation IS NULL OR current_snapshot_generation < $2)",
        )
        .bind(server_id)
        .bind(generation)
        .bind(total_bytes)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        if let Err(error) = self
            .prune_old_generations(&mut publication_guard, server_id)
            .await
        {
            tracing::warn!(%server_id, ?error, "failed to prune old snapshot generations");
        }

        let snapshot = sqlx::query_as::<_, SnapshotRecord>(
            "SELECT id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete, created_at
             FROM snapshots WHERE server_id = $1 AND generation = $2",
        )
        .bind(server_id)
        .bind(generation)
        .fetch_one(&mut *publication_guard)
        .await?;
        Ok(snapshot)
    }

    async fn read_verified_chunk(&self, file: &ManifestFile) -> Result<Bytes> {
        let bytes = self
            .store
            .get(&Path::from(format!("chunks/{}", file.blake3)))
            .await?
            .bytes()
            .await?;
        if bytes.len() as u64 != file.size || blake3::hash(&bytes).to_hex().as_str() != file.blake3
        {
            bail!(
                "snapshot chunk failed integrity validation for {}",
                file.path
            );
        }
        Ok(bytes)
    }

    pub async fn restore_latest(&self, server_id: Uuid, root: &FsPath) -> Result<Option<i64>> {
        let mut guard = self.storage_guard(false).await?;
        let Some((snapshot, manifest)) = self.current_manifest(&mut guard, server_id).await? else {
            return Ok(None);
        };
        self.restore_manifest(root, manifest).await?;
        Ok(Some(snapshot.generation))
    }

    pub async fn restore_generation(
        &self,
        server_id: Uuid,
        generation: i64,
        root: &FsPath,
    ) -> Result<i64> {
        let mut guard = self.storage_guard(false).await?;
        let snapshot = sqlx::query_as::<_, SnapshotRecord>(
            "SELECT id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete, created_at
             FROM snapshots
             WHERE server_id = $1 AND generation = $2 AND complete",
        )
        .bind(server_id)
        .bind(generation)
        .fetch_optional(&mut *guard)
        .await?
        .context("requested snapshot is not retained or complete")?;
        let manifest = self.read_manifest(&snapshot).await?;
        self.restore_manifest(root, manifest).await?;
        Ok(snapshot.generation)
    }

    async fn restore_manifest(&self, root: &FsPath, manifest: SnapshotManifest) -> Result<()> {
        tokio::fs::create_dir_all(root).await?;
        for file in manifest.files {
            let relative = safe_relative_path(&file.path)?;
            let bytes = self.read_verified_chunk(&file).await?;
            let target = root.join(relative);
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let temp = target.with_extension(format!("kurbis-restore-{}", Uuid::new_v4()));
            tokio::fs::write(&temp, &bytes).await?;
            tokio::fs::rename(temp, target).await?;
        }
        Ok(())
    }

    async fn current_manifest(
        &self,
        connection: &mut PgConnection,
        server_id: Uuid,
    ) -> Result<Option<(SnapshotRecord, SnapshotManifest)>> {
        let current_generation: Option<i64> =
            sqlx::query_scalar("SELECT current_snapshot_generation FROM servers WHERE id = $1")
                .bind(server_id)
                .fetch_one(&mut *connection)
                .await?;
        let Some(current_generation) = current_generation else {
            return Ok(None);
        };
        let snapshot = sqlx::query_as::<_, SnapshotRecord>(
            "SELECT id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete, created_at
             FROM snapshots
             WHERE server_id = $1 AND generation = $2 AND complete",
        )
        .bind(server_id)
        .bind(current_generation)
        .fetch_optional(&mut *connection)
        .await?
        .context("published snapshot metadata is missing or incomplete")?;
        let manifest = self.read_manifest(&snapshot).await?;
        Ok(Some((snapshot, manifest)))
    }

    pub(super) async fn read_manifest(
        &self,
        snapshot: &SnapshotRecord,
    ) -> Result<SnapshotManifest> {
        let manifest_bytes = self
            .store
            .get(&Path::from(snapshot.manifest_key.clone()))
            .await?
            .bytes()
            .await?;
        let actual_checksum = blake3::hash(&manifest_bytes).to_hex().to_string();
        if actual_checksum != snapshot.checksum {
            bail!("snapshot manifest checksum mismatch");
        }
        let manifest: SnapshotManifest = serde_json::from_slice(&manifest_bytes)?;
        if manifest.server_id != snapshot.server_id || manifest.generation != snapshot.generation {
            bail!("snapshot manifest identity mismatch");
        }

        Ok(manifest)
    }

    /// A database cache hint alone cannot prove disposable local disk contents.
    pub async fn local_matches_current(&self, server_id: Uuid, root: &FsPath) -> Result<bool> {
        let mut guard = self.storage_guard(false).await?;
        let Some((_, manifest)) = self.current_manifest(&mut guard, server_id).await? else {
            return Ok(false);
        };
        let root = root.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let local = collect_files(&root)?;
            if local.len() != manifest.files.len() {
                return Ok(false);
            }
            let local_paths: std::collections::HashSet<_> =
                local.iter().map(|file| file.relative.as_str()).collect();
            for file in &manifest.files {
                if !local_paths.contains(file.path.as_str()) {
                    return Ok(false);
                }
                let relative = safe_relative_path(&file.path)?;
                let bytes = match std::fs::read(root.join(relative)) {
                    Ok(bytes) => bytes,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                    Err(error) => return Err(error.into()),
                };
                if bytes.len() as u64 != file.size
                    || blake3::hash(&bytes).to_hex().as_str() != file.blake3
                {
                    return Ok(false);
                }
            }
            Ok(true)
        })
        .await
        .context("local snapshot validation task failed")?
    }

    pub async fn delete_server_manifests(&self, server_id: Uuid) -> Result<()> {
        let manifest_keys: Vec<String> = sqlx::query_scalar(
            "SELECT manifest_key FROM snapshots WHERE server_id = $1 AND manifest_key <> ''",
        )
        .bind(server_id)
        .fetch_all(&self.pool)
        .await?;
        for manifest_key in manifest_keys {
            match self.store.delete(&Path::from(manifest_key)).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    async fn validate_manifest_object(&self, manifest_key: &str, checksum: &str) -> Result<()> {
        let bytes = self
            .store
            .get(&Path::from(manifest_key.to_owned()))
            .await?
            .bytes()
            .await?;
        let actual = blake3::hash(&bytes).to_hex().to_string();
        if actual != checksum {
            bail!("uploaded snapshot manifest checksum mismatch");
        }
        let _: SnapshotManifest = serde_json::from_slice(&bytes)?;
        Ok(())
    }

    async fn prune_old_generations(
        &self,
        connection: &mut PgConnection,
        server_id: Uuid,
    ) -> Result<()> {
        let stale: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, manifest_key
             FROM snapshots
             WHERE server_id = $1 AND complete
               AND generation NOT IN (
                 SELECT generation FROM snapshots
                 WHERE server_id = $1 AND complete
                 ORDER BY generation DESC
                 LIMIT $2
               )
               AND NOT EXISTS (
                 SELECT 1 FROM operations o
                 WHERE o.server_id = snapshots.server_id AND o.kind = 'restore'
                   AND o.status IN ('pending', 'running')
                   AND o.payload->>'target_generation' = snapshots.generation::text
               )
             ORDER BY generation ASC",
        )
        .bind(server_id)
        .bind(SNAPSHOT_RETENTION_GENERATIONS)
        .fetch_all(&mut *connection)
        .await?;
        for (snapshot_id, manifest_key) in stale {
            match self.store.delete(&Path::from(manifest_key)).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(error) => return Err(error.into()),
            }
            sqlx::query("DELETE FROM snapshots WHERE id = $1")
                .bind(snapshot_id)
                .execute(&mut *connection)
                .await?;
        }
        Ok(())
    }

    async fn allocate_generation(
        &self,
        connection: &mut PgConnection,
        server_id: Uuid,
        reason: SnapshotReason,
    ) -> Result<i64> {
        let mut tx = connection.begin().await?;
        let generation: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(generation), 0) + 1 FROM snapshots WHERE server_id = $1",
        )
        .bind(server_id)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete)
             VALUES($1,$2,$3,'',0,'',$4,false)",
        )
        .bind(Uuid::new_v4())
        .bind(server_id)
        .bind(generation)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(generation)
    }
}
