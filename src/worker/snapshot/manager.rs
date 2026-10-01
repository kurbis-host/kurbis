use std::{collections::HashSet, path::Path as FsPath, sync::Arc};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use futures::StreamExt;
use object_store::{ObjectStore, path::Path};
use sqlx::{Connection, PgConnection, PgPool, Postgres, pool::PoolConnection};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use crate::model::{
    SERVER_STORAGE_BYTES, SNAPSHOT_RETENTION_GENERATIONS, SnapshotReason, SnapshotRecord,
};

use super::files::{collect_files, safe_relative_path};
use super::{
    ManifestFile, ObjectKind, SnapshotManager, SnapshotManifest, WorkerObjectAssignment,
    WorkerObjectClient, WorkerObjectKey, object_key::MAX_MANIFEST_BYTES, physical_object_path,
};

const SNAPSHOT_CHUNK_PART_BYTES: usize = 8 * 1024 * 1024;

impl SnapshotManager {
    pub(super) fn object_path(&self, server_id: Uuid, key: &str) -> Result<Path> {
        if self.worker_client.is_some() {
            Ok(WorkerObjectKey::parse(key)?.path().clone())
        } else {
            Ok(physical_object_path(server_id, key)?)
        }
    }

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
    pub fn new(pool: PgPool, store: Arc<dyn ObjectStore>) -> Self {
        Self {
            pool,
            store,
            worker_client: None,
        }
    }

    pub fn new_worker(pool: PgPool, client: WorkerObjectClient) -> Self {
        Self {
            pool,
            store: Arc::new(client.unscoped()),
            worker_client: Some(client),
        }
    }

    pub fn for_operation(&self, server_id: Uuid, operation_id: Uuid, generation: i64) -> Self {
        self.for_assignment(
            server_id,
            WorkerObjectAssignment::Operation {
                operation_id,
                generation,
            },
        )
    }

    pub fn for_placement(&self, server_id: Uuid, generation: i64) -> Self {
        self.for_assignment(server_id, WorkerObjectAssignment::Placement { generation })
    }

    pub fn for_export(&self, server_id: Uuid, export_id: Uuid, generation: i64) -> Self {
        self.for_assignment(
            server_id,
            WorkerObjectAssignment::Export {
                export_id,
                generation,
            },
        )
    }

    fn for_assignment(&self, server_id: Uuid, assignment: WorkerObjectAssignment) -> Self {
        let store = self
            .worker_client
            .as_ref()
            .map(|client| Arc::new(client.scoped(server_id, assignment)) as Arc<dyn ObjectStore>)
            .unwrap_or_else(|| self.store.clone());
        Self {
            pool: self.pool.clone(),
            store,
            worker_client: self.worker_client.clone(),
        }
    }

    pub async fn create(
        &self,
        server_id: Uuid,
        root: &FsPath,
        reason: SnapshotReason,
    ) -> Result<SnapshotRecord> {
        // Serialize allocation, upload, and retention across workers.
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
            let (size, hash) = hash_file(&file.absolute).await?;
            total_bytes = total_bytes
                .checked_add(size)
                .context("snapshot size overflow")?;
            if total_bytes > SERVER_STORAGE_BYTES as u64 {
                bail!("server storage quota exceeded");
            }
            let chunk_path = self.object_path(server_id, &format!("chunks/{hash}"))?;
            match self.store.head(&chunk_path).await {
                Ok(_) => {}
                Err(object_store::Error::NotFound { .. }) => {
                    self.upload_file(&chunk_path, &file.absolute, size).await?;
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
            self.verify_stored_chunk(server_id, &manifest_file).await?;
            manifest_files.push(manifest_file);
        }
        manifest_files.sort_by(|a, b| a.path.cmp(&b.path));
        let manifest = SnapshotManifest {
            server_id,
            generation,
            files: manifest_files,
        };
        let manifest_bytes = serde_json::to_vec(&manifest)?;
        anyhow::ensure!(
            manifest_bytes.len() as u64 <= MAX_MANIFEST_BYTES,
            "snapshot manifest exceeds the maximum object size"
        );
        let checksum = blake3::hash(&manifest_bytes).to_hex().to_string();
        let manifest_key = format!("snapshots/{generation}-{checksum}.json");
        let manifest_path = self.object_path(server_id, &manifest_key)?;
        self.store
            .put(&manifest_path, Bytes::from(manifest_bytes).into())
            .await?;
        self.validate_manifest_object(&manifest_path, &checksum)
            .await?;

        let total_bytes = i64::try_from(total_bytes)?;
        let mut tx = publication_guard.begin().await?;
        sqlx::query("SELECT id FROM servers WHERE id = $1")
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

    async fn verify_stored_chunk(&self, server_id: Uuid, file: &ManifestFile) -> Result<()> {
        let object = self
            .store
            .get(&self.object_path(server_id, &format!("chunks/{}", file.blake3))?)
            .await?;
        let mut stream = object.into_stream();
        let mut hasher = blake3::Hasher::new();
        let mut size = 0_u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            size = size
                .checked_add(chunk.len() as u64)
                .context("snapshot chunk size overflow")?;
            hasher.update(&chunk);
        }
        if size != file.size || hasher.finalize().to_hex().as_str() != file.blake3 {
            bail!(
                "snapshot chunk failed integrity validation for {}",
                file.path
            );
        }
        Ok(())
    }

    async fn upload_file(&self, key: &Path, source: &FsPath, size: u64) -> Result<()> {
        if size <= SNAPSHOT_CHUNK_PART_BYTES as u64 {
            let bytes = tokio::fs::read(source).await?;
            if bytes.len() as u64 != size {
                bail!("snapshot source changed while it was being read");
            }
            self.store.put(key, Bytes::from(bytes).into()).await?;
            return Ok(());
        }

        let mut file = tokio::fs::File::open(source).await?;
        let mut upload = self.store.put_multipart(key).await?;
        let result = async {
            let mut buffer = vec![0; SNAPSHOT_CHUNK_PART_BYTES];
            let mut uploaded = 0_u64;
            loop {
                let read = file.read(&mut buffer).await?;
                if read == 0 {
                    break;
                }
                uploaded = uploaded
                    .checked_add(read as u64)
                    .context("snapshot source size overflow")?;
                upload
                    .put_part(Bytes::copy_from_slice(&buffer[..read]).into())
                    .await?;
            }
            if uploaded != size {
                bail!("snapshot source changed while it was being read");
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            let _ = upload.abort().await;
            return Err(error);
        }
        upload.complete().await?;
        Ok(())
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
        ensure_safe_directory_chain(root, root).await?;
        let mut entries = Vec::with_capacity(manifest.files.len());
        let mut paths = HashSet::with_capacity(manifest.files.len());
        let mut total_size = 0_u64;
        for file in &manifest.files {
            let relative = safe_relative_path(&file.path)?;
            anyhow::ensure!(
                paths.insert(relative.clone()),
                "snapshot manifest contains a duplicate path"
            );
            anyhow::ensure!(
                WorkerObjectKey::parse(&format!("chunks/{}", file.blake3))
                    .is_ok_and(|key| matches!(key.kind(), ObjectKind::Chunk)),
                "snapshot manifest contains an invalid chunk hash"
            );
            total_size = total_size
                .checked_add(file.size)
                .context("snapshot manifest size overflow")?;
            anyhow::ensure!(
                total_size <= SERVER_STORAGE_BYTES as u64,
                "snapshot manifest exceeds the server storage quota"
            );
            let target = root.join(relative);
            if let Some(parent) = target.parent() {
                ensure_existing_safe_directory_chain(root, parent).await?;
            }
            entries.push((file.clone(), target));
        }

        // Every manifest path and existing parent has been checked before the
        // first destination is touched. Directory creation is therefore only
        // a completion step for already-validated paths.
        for (file, target) in entries {
            if let Some(parent) = target.parent() {
                ensure_safe_directory_chain(root, parent).await?;
            }
            let temp = target.with_extension(format!("kurbis-restore-{}", Uuid::new_v4()));
            let result = self
                .restore_chunk_to_file(manifest.server_id, &file, &temp)
                .await;
            if let Err(error) = result {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(error);
            }
            tokio::fs::rename(temp, target).await?;
        }
        Ok(())
    }

    async fn restore_chunk_to_file(
        &self,
        server_id: Uuid,
        file: &ManifestFile,
        destination: &FsPath,
    ) -> Result<()> {
        let object = self
            .store
            .get(&self.object_path(server_id, &format!("chunks/{}", file.blake3))?)
            .await?;
        let mut stream = object.into_stream();
        let mut output = tokio::fs::File::create(destination).await?;
        let mut hasher = blake3::Hasher::new();
        let mut size = 0_u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            size = size
                .checked_add(chunk.len() as u64)
                .context("snapshot chunk size overflow")?;
            hasher.update(&chunk);
            output.write_all(&chunk).await?;
        }
        output.flush().await?;
        if size != file.size || hasher.finalize().to_hex().as_str() != file.blake3 {
            bail!(
                "snapshot chunk failed integrity validation for {}",
                file.path
            );
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
        let manifest = self
            .store
            .get(&self.object_path(snapshot.server_id, &snapshot.manifest_key)?)
            .await?;
        anyhow::ensure!(
            manifest.meta.size <= MAX_MANIFEST_BYTES,
            "snapshot manifest exceeds the maximum object size"
        );
        let manifest_bytes = manifest.bytes().await?;
        let actual_checksum = blake3::hash(&manifest_bytes).to_hex().to_string();
        if actual_checksum != snapshot.checksum {
            bail!("snapshot manifest checksum mismatch");
        }
        let manifest: SnapshotManifest = serde_json::from_slice(&manifest_bytes)?;
        if manifest.server_id != snapshot.server_id || manifest.generation != snapshot.generation {
            bail!("snapshot manifest identity mismatch");
        }
        validate_manifest_structure(&manifest)?;

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
        let _guard = self.storage_guard(false).await?;
        let manifest_keys: Vec<String> = sqlx::query_scalar(
            "SELECT manifest_key FROM snapshots WHERE server_id = $1 AND manifest_key <> ''",
        )
        .bind(server_id)
        .fetch_all(&self.pool)
        .await?;
        for manifest_key in manifest_keys {
            match self
                .store
                .delete(&self.object_path(server_id, &manifest_key)?)
                .await
            {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    async fn validate_manifest_object(&self, manifest_path: &Path, checksum: &str) -> Result<()> {
        let object = self.store.get(manifest_path).await?;
        anyhow::ensure!(
            object.meta.size <= MAX_MANIFEST_BYTES,
            "uploaded snapshot manifest exceeds the maximum object size"
        );
        let bytes = object.bytes().await?;
        let actual = blake3::hash(&bytes).to_hex().to_string();
        if actual != checksum {
            bail!("uploaded snapshot manifest checksum mismatch");
        }
        let manifest: SnapshotManifest = serde_json::from_slice(&bytes)?;
        validate_manifest_structure(&manifest)?;
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
               AND NOT EXISTS (
                 SELECT 1 FROM world_exports e
                 WHERE e.server_id = snapshots.server_id
                   AND e.source_generation = snapshots.generation
                   AND e.status IN ('pending', 'running')
               )
             ORDER BY generation ASC",
        )
        .bind(server_id)
        .bind(SNAPSHOT_RETENTION_GENERATIONS)
        .fetch_all(&mut *connection)
        .await?;
        for (snapshot_id, manifest_key) in stale {
            match self
                .store
                .delete(&self.object_path(server_id, &manifest_key)?)
                .await
            {
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

async fn hash_file(path: &FsPath) -> Result<(u64, String)> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut buffer = vec![0; 64 * 1024];
    let mut hasher = blake3::Hasher::new();
    let mut size = 0_u64;
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(read as u64)
            .context("snapshot file size overflow")?;
        hasher.update(&buffer[..read]);
    }
    Ok((size, hasher.finalize().to_hex().to_string()))
}

async fn ensure_safe_directory_chain(root: &FsPath, target: &FsPath) -> Result<()> {
    let relative = target
        .strip_prefix(root)
        .context("restore target is outside its server directory")?;
    ensure_safe_directory(root).await?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            bail!("restore target contains an unsafe directory component");
        };
        current.push(component);
        ensure_safe_directory(&current).await?;
    }
    Ok(())
}

async fn ensure_existing_safe_directory_chain(root: &FsPath, target: &FsPath) -> Result<()> {
    let relative = target
        .strip_prefix(root)
        .context("restore target is outside its server directory")?;
    ensure_safe_directory(root).await?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            bail!("restore target contains an unsafe directory component");
        };
        current.push(component);
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!("restore target directory is not a real directory")
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(super) fn validate_manifest_structure(manifest: &SnapshotManifest) -> Result<()> {
    anyhow::ensure!(
        manifest.generation > 0,
        "snapshot manifest generation is invalid"
    );
    let mut paths = HashSet::with_capacity(manifest.files.len());
    let mut total_size = 0_u64;
    for file in &manifest.files {
        let relative = safe_relative_path(&file.path)?;
        anyhow::ensure!(
            paths.insert(relative),
            "snapshot manifest contains a duplicate path"
        );
        anyhow::ensure!(
            WorkerObjectKey::parse(&format!("chunks/{}", file.blake3))
                .is_ok_and(|key| matches!(key.kind(), ObjectKind::Chunk)),
            "snapshot manifest contains an invalid chunk hash"
        );
        total_size = total_size
            .checked_add(file.size)
            .context("snapshot manifest size overflow")?;
        anyhow::ensure!(
            total_size <= SERVER_STORAGE_BYTES as u64,
            "snapshot manifest exceeds the server storage quota"
        );
    }
    Ok(())
}

async fn ensure_safe_directory(path: &FsPath) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            bail!("restore target directory is not a real directory")
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir(path).await?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}
