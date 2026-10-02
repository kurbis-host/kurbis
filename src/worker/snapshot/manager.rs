use std::{collections::HashSet, path::Path as FsPath};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use futures::StreamExt;
use object_store::{ObjectStore, path::Path};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use crate::{
    contracts::worker::WorkerAssignment,
    model::{OperationRecord, SERVER_STORAGE_BYTES, SnapshotReason, SnapshotRecord},
};

use super::{SnapshotManager, WorkerObjectClient};
use crate::snapshot::{
    ManifestFile, ObjectKind, SnapshotManifest, WorkerObjectKey,
    files::{collect_files, safe_relative_path},
    object_key::MAX_MANIFEST_BYTES,
};

const SNAPSHOT_CHUNK_PART_BYTES: usize = 8 * 1024 * 1024;

impl SnapshotManager {
    pub fn new_worker(client: WorkerObjectClient) -> Self {
        Self {
            store: std::sync::Arc::new(client.unscoped()),
            worker_client: client,
            worker_assignment: None,
        }
    }

    pub fn for_operation(
        &self,
        server_id: Uuid,
        operation_id: Uuid,
        generation: i64,
        attempt: i32,
    ) -> Self {
        self.for_assignment(
            server_id,
            WorkerAssignment::Operation {
                operation_id,
                generation,
                attempt,
            },
        )
    }

    pub fn for_placement(&self, server_id: Uuid, generation: i64) -> Self {
        self.for_assignment(server_id, WorkerAssignment::Placement { generation })
    }

    pub fn for_export(
        &self,
        server_id: Uuid,
        export_id: Uuid,
        generation: i64,
        attempt: i32,
    ) -> Self {
        self.for_assignment(
            server_id,
            WorkerAssignment::Export {
                export_id,
                generation,
                attempt,
            },
        )
    }

    fn for_assignment(&self, server_id: Uuid, assignment: WorkerAssignment) -> Self {
        Self {
            store: std::sync::Arc::new(self.worker_client.scoped(server_id, assignment)),
            worker_client: self.worker_client.clone(),
            worker_assignment: Some((server_id, assignment)),
        }
    }

    pub(super) fn scope(&self, server_id: Uuid) -> Result<WorkerAssignment> {
        let (scoped_server, assignment) = self
            .worker_assignment
            .context("worker snapshot request has no assignment scope")?;
        anyhow::ensure!(
            server_id == scoped_server,
            "snapshot server differs from worker object scope"
        );
        Ok(assignment)
    }

    pub(super) fn object_path(&self, key: &str) -> Result<Path> {
        Ok(WorkerObjectKey::parse(key)?.path().clone())
    }

    pub async fn create(
        &self,
        server_id: Uuid,
        root: &FsPath,
        reason: SnapshotReason,
    ) -> Result<SnapshotRecord> {
        let assignment = self.scope(server_id)?;
        let reservation = self
            .worker_client
            .reserve_snapshot(server_id, assignment, reason)
            .await?;
        let root = root.to_path_buf();
        let files = tokio::task::spawn_blocking(move || collect_files(&root))
            .await
            .context("snapshot scan task failed")??;
        let mut manifest_files = Vec::with_capacity(files.len());
        let mut total_bytes = 0_u64;
        for file in files {
            let (size, hash) = hash_file(&file.absolute).await?;
            total_bytes = total_bytes
                .checked_add(size)
                .context("snapshot size overflow")?;
            anyhow::ensure!(
                total_bytes <= SERVER_STORAGE_BYTES as u64,
                "server storage quota exceeded"
            );
            let chunk_path = self.object_path(&format!("chunks/{hash}"))?;
            match self.store.head(&chunk_path).await {
                Ok(_) => {}
                Err(object_store::Error::NotFound { .. }) => {
                    self.upload_file(&chunk_path, &file.absolute, size).await?
                }
                Err(error) => return Err(error.into()),
            }
            let manifest_file = ManifestFile {
                path: file.relative,
                size,
                blake3: hash,
            };
            self.verify_stored_chunk(&manifest_file).await?;
            manifest_files.push(manifest_file);
        }
        manifest_files.sort_by(|left, right| left.path.cmp(&right.path));
        let manifest = SnapshotManifest {
            server_id,
            generation: reservation.generation,
            files: manifest_files,
        };
        let manifest_bytes = serde_json::to_vec(&manifest)?;
        anyhow::ensure!(
            manifest_bytes.len() as u64 <= MAX_MANIFEST_BYTES,
            "snapshot manifest exceeds the maximum object size"
        );
        let checksum = blake3::hash(&manifest_bytes).to_hex().to_string();
        let manifest_key = format!("snapshots/{}-{checksum}.json", reservation.generation);
        let manifest_path = self.object_path(&manifest_key)?;
        self.store
            .put(&manifest_path, Bytes::from(manifest_bytes).into())
            .await?;
        self.validate_manifest_object(&manifest_path, &checksum)
            .await?;
        let size_bytes = i64::try_from(total_bytes)?;
        self.worker_client
            .publish_snapshot_metadata(
                server_id,
                assignment,
                reservation.generation,
                &manifest_key,
                size_bytes,
                &checksum,
            )
            .await?;
        Ok(SnapshotRecord {
            id: reservation.id,
            server_id,
            generation: reservation.generation,
            manifest_key,
            size_bytes,
            checksum,
            reason,
            complete: true,
            created_at: reservation.created_at,
        })
    }

    async fn verify_stored_chunk(&self, file: &ManifestFile) -> Result<()> {
        let object = self
            .store
            .get(&self.object_path(&format!("chunks/{}", file.blake3))?)
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
            anyhow::ensure!(
                bytes.len() as u64 == size,
                "snapshot source changed while it was being read"
            );
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
            anyhow::ensure!(
                uploaded == size,
                "snapshot source changed while it was being read"
            );
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
        let assignment = self.scope(server_id)?;
        let Some(snapshot) = self
            .worker_client
            .current_snapshot(server_id, assignment)
            .await?
        else {
            return Ok(None);
        };
        let manifest = self.read_manifest(&snapshot).await?;
        self.restore_manifest(root, manifest).await?;
        Ok(Some(snapshot.generation))
    }

    pub async fn restore_generation(
        &self,
        server_id: Uuid,
        generation: i64,
        root: &FsPath,
    ) -> Result<i64> {
        let assignment = self.scope(server_id)?;
        let snapshot = self
            .worker_client
            .snapshot(server_id, assignment, generation)
            .await?;
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
        for (file, target) in entries {
            if let Some(parent) = target.parent() {
                ensure_safe_directory_chain(root, parent).await?;
            }
            let temp = target.with_extension(format!("kurbis-restore-{}", Uuid::new_v4()));
            if let Err(error) = self.restore_chunk_to_file(&file, &temp).await {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(error);
            }
            tokio::fs::rename(temp, target).await?;
        }
        Ok(())
    }

    async fn restore_chunk_to_file(&self, file: &ManifestFile, destination: &FsPath) -> Result<()> {
        let object = self
            .store
            .get(&self.object_path(&format!("chunks/{}", file.blake3))?)
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
        anyhow::ensure!(
            size == file.size && hasher.finalize().to_hex().as_str() == file.blake3,
            "snapshot chunk failed integrity validation for {}",
            file.path
        );
        Ok(())
    }

    pub(super) async fn read_manifest(
        &self,
        snapshot: &SnapshotRecord,
    ) -> Result<SnapshotManifest> {
        let object = self
            .store
            .get(&self.object_path(&snapshot.manifest_key)?)
            .await?;
        anyhow::ensure!(
            object.meta.size <= MAX_MANIFEST_BYTES,
            "snapshot manifest exceeds the maximum object size"
        );
        let bytes = object.bytes().await?;
        anyhow::ensure!(
            blake3::hash(&bytes).to_hex().as_str() == snapshot.checksum,
            "snapshot manifest checksum mismatch"
        );
        let manifest: SnapshotManifest = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            manifest.server_id == snapshot.server_id && manifest.generation == snapshot.generation,
            "snapshot manifest identity mismatch"
        );
        validate_manifest_structure(&manifest)?;
        Ok(manifest)
    }

    pub async fn local_matches_current(&self, server_id: Uuid, root: &FsPath) -> Result<bool> {
        let assignment = self.scope(server_id)?;
        let Some(snapshot) = self
            .worker_client
            .current_snapshot(server_id, assignment)
            .await?
        else {
            return Ok(false);
        };
        let manifest = self.read_manifest(&snapshot).await?;
        let root = root.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let local = collect_files(&root)?;
            if local.len() != manifest.files.len() {
                return Ok(false);
            }
            let local_paths = local
                .iter()
                .map(|file| file.relative.as_str())
                .collect::<HashSet<_>>();
            for file in &manifest.files {
                if !local_paths.contains(file.path.as_str()) {
                    return Ok(false);
                }
                let bytes = match std::fs::read(root.join(safe_relative_path(&file.path)?)) {
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

    pub async fn delete_server_manifests(&self, operation: &OperationRecord) -> Result<()> {
        let assignment = self.scope(operation.server_id)?;
        anyhow::ensure!(
            matches!(assignment, WorkerAssignment::Operation { operation_id, generation, attempt } if operation_id == operation.id && generation == operation.generation && attempt == operation.attempt),
            "snapshot cleanup operation differs from worker object scope"
        );
        self.worker_client
            .delete_snapshot_manifests(operation.server_id, assignment)
            .await
    }

    async fn validate_manifest_object(&self, path: &Path, checksum: &str) -> Result<()> {
        let object = self.store.get(path).await?;
        anyhow::ensure!(
            object.meta.size <= MAX_MANIFEST_BYTES,
            "uploaded snapshot manifest exceeds the maximum object size"
        );
        let bytes = object.bytes().await?;
        anyhow::ensure!(
            blake3::hash(&bytes).to_hex().as_str() == checksum,
            "uploaded snapshot manifest checksum mismatch"
        );
        let manifest: SnapshotManifest = serde_json::from_slice(&bytes)?;
        validate_manifest_structure(&manifest)?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::snapshot::SnapshotSource for SnapshotManager {
    async fn local_matches_current(&self, server_id: Uuid, root: &FsPath) -> Result<bool> {
        SnapshotManager::local_matches_current(self, server_id, root).await
    }

    async fn restore_latest(&self, server_id: Uuid, root: &FsPath) -> Result<Option<i64>> {
        SnapshotManager::restore_latest(self, server_id, root).await
    }
}

pub(crate) fn validate_manifest_structure(manifest: &SnapshotManifest) -> Result<()> {
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
