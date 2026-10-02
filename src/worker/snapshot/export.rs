use std::path::Path as FsPath;

use crate::snapshot::{SnapshotManifest, files::safe_relative_path};
use anyhow::{Context, Result, bail};
use async_zip::{Compression, ZipEntryBuilder, tokio::write::ZipFileWriter};
use bytes::Bytes;
use futures::StreamExt;
use futures_lite::io::AsyncWriteExt as ZipWriteExt;
use object_store::{MultipartUpload, ObjectStore, path::Path};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use crate::contracts::worker::WorkerAssignment;

use super::SnapshotManager;

const ARCHIVE_PART_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct WorldArchive {
    pub server_id: Uuid,
    pub archive_key: String,
    pub path: std::path::PathBuf,
    pub size_bytes: i64,
    pub checksum: String,
}

impl SnapshotManager {
    pub async fn create_world_archive(
        &self,
        export_id: Uuid,
        server_id: Uuid,
        generation: i64,
        attempt: i32,
        staging_root: &FsPath,
    ) -> Result<WorldArchive> {
        let assignment = self.scope(server_id)?;
        anyhow::ensure!(
            matches!(assignment, WorkerAssignment::Export { export_id: id, generation: source_generation, attempt: source_attempt } if id == export_id && source_generation == generation && source_attempt == attempt),
            "export task differs from worker object scope"
        );
        anyhow::ensure!(
            self.worker_client
                .export_is_running(export_id, generation, attempt)
                .await?,
            "world export is no longer active"
        );
        let snapshot = self
            .worker_client
            .snapshot(server_id, assignment, generation)
            .await?;
        let manifest = self.read_manifest(&snapshot).await?;
        self.create_archive_from_manifest(export_id, server_id, attempt, staging_root, manifest)
            .await
    }

    async fn create_archive_from_manifest(
        &self,
        export_id: Uuid,
        server_id: Uuid,
        attempt: i32,
        staging_root: &FsPath,
        manifest: SnapshotManifest,
    ) -> Result<WorldArchive> {
        let output = staging_root.join(format!("{export_id}-{attempt}.zip.part"));
        if let Some(parent) = output.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if let Err(error) = self
            .write_world_zip(export_id, attempt, &manifest, &output)
            .await
        {
            let _ = tokio::fs::remove_file(&output).await;
            return Err(error);
        }
        let size_bytes = tokio::fs::metadata(&output).await?.len();
        let checksum = hash_file(&output).await?;
        Ok(WorldArchive {
            server_id,
            archive_key: format!("exports/{export_id}/{attempt}.zip"),
            path: output,
            size_bytes: i64::try_from(size_bytes)
                .context("world archive exceeds supported size")?,
            checksum,
        })
    }

    async fn write_world_zip(
        &self,
        export_id: Uuid,
        attempt: i32,
        manifest: &SnapshotManifest,
        output: &FsPath,
    ) -> Result<()> {
        let mut files = Vec::new();
        for file in &manifest.files {
            if let Some(path) = archive_entry_path(&file.path)? {
                files.push((file, path));
            }
        }
        files.sort_by(|left, right| left.1.cmp(&right.1));
        anyhow::ensure!(
            files.iter().any(|(_, path)| path == "level.dat"),
            "world snapshot has no level.dat and cannot be exported"
        );

        let output_file = tokio::fs::File::create(output).await?;
        let mut writer = ZipFileWriter::with_tokio(output_file).force_zip64();
        for (file, archive_path) in files {
            if self.export_cancelled(export_id, attempt).await? {
                bail!("world export was cancelled");
            }
            let object = self
                .store
                .get(&self.object_path(&format!("chunks/{}", file.blake3))?)
                .await?;
            let mut stream = object.into_stream();
            let entry = ZipEntryBuilder::new(archive_path.into(), Compression::Deflate);
            let mut entry_writer = writer.write_entry_stream(entry).await?;
            let mut hasher = blake3::Hasher::new();
            let mut size = 0_u64;
            while let Some(chunk) = stream.next().await {
                if self.export_cancelled(export_id, attempt).await? {
                    bail!("world export was cancelled");
                }
                let chunk: Bytes = chunk?;
                size = size
                    .checked_add(chunk.len() as u64)
                    .context("snapshot chunk size overflow")?;
                hasher.update(&chunk);
                entry_writer.write_all(&chunk).await?;
            }
            anyhow::ensure!(
                size == file.size && hasher.finalize().to_hex().as_str() == file.blake3,
                "snapshot chunk failed integrity validation for {}",
                file.path
            );
            entry_writer.close().await?;
        }
        writer.close().await?;
        Ok(())
    }

    async fn export_cancelled(&self, export_id: Uuid, attempt: i32) -> Result<bool> {
        let (_, assignment) = self
            .worker_assignment
            .context("worker export cancellation check has no export scope")?;
        let WorkerAssignment::Export {
            export_id: assigned_id,
            generation,
            attempt: assigned_attempt,
        } = assignment
        else {
            bail!("worker export cancellation check has no export scope");
        };
        anyhow::ensure!(
            assigned_id == export_id && assigned_attempt == attempt,
            "export id differs from worker object scope"
        );
        Ok(!self
            .worker_client
            .export_is_running(export_id, generation, attempt)
            .await?)
    }

    pub async fn upload_world_archive(&self, archive: &WorldArchive) -> Result<()> {
        let key = self.object_path(&archive.archive_key)?;
        let mut upload = self.store.put_multipart(&key).await?;
        if let Err(error) = stream_archive_parts(&mut upload, &archive.path).await {
            let _ = upload.abort().await;
            return Err(error);
        }
        upload.complete().await?;
        let (size, checksum) = hash_object(self.store.as_ref(), &key).await?;
        if size != archive.size_bytes as u64 || checksum != archive.checksum {
            let _ = self.store.delete(&key).await;
            bail!("uploaded world archive failed checksum verification");
        }
        Ok(())
    }

    pub async fn delete_world_archive(&self, _server_id: Uuid, archive_key: &str) -> Result<()> {
        let key = self.object_path(archive_key)?;
        match self.store.delete(&key).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn remove_world_archive_staging(&self, archive: &WorldArchive) {
        let _ = tokio::fs::remove_file(&archive.path).await;
    }
}

async fn stream_archive_parts(
    upload: &mut Box<dyn MultipartUpload>,
    archive: &FsPath,
) -> Result<()> {
    let mut file = tokio::fs::File::open(archive).await?;
    loop {
        let mut part = vec![0; ARCHIVE_PART_BYTES];
        let read = file.read(&mut part).await?;
        if read == 0 {
            break;
        }
        part.truncate(read);
        upload.put_part(Bytes::from(part).into()).await?;
    }
    Ok(())
}

async fn hash_file(path: &FsPath) -> Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

async fn hash_object(store: &dyn ObjectStore, key: &Path) -> Result<(u64, String)> {
    let mut stream = store.get(key).await?.into_stream();
    let mut size = 0_u64;
    let mut hasher = blake3::Hasher::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        size = size
            .checked_add(chunk.len() as u64)
            .context("world archive size overflow")?;
        hasher.update(&chunk);
    }
    Ok((size, hasher.finalize().to_hex().to_string()))
}

fn archive_entry_path(path: &str) -> Result<Option<String>> {
    let source_path = safe_relative_path(path)?;
    let mut parts = source_path.components();
    if parts.next().and_then(|part| part.as_os_str().to_str()) != Some("world") {
        return Ok(None);
    }
    let suffix = source_path
        .strip_prefix("world")
        .context("world archive path does not have a relative suffix")?;
    if suffix.as_os_str().is_empty() {
        return Ok(None);
    }
    let suffix_text = suffix
        .components()
        .map(|part| {
            part.as_os_str()
                .to_str()
                .context("world path is not valid UTF-8")
        })
        .collect::<Result<Vec<_>>>()?
        .join("/");
    if matches!(
        suffix_text.as_str(),
        "session.lock"
            | "ops.json"
            | "whitelist.json"
            | "pumpkin.toml"
            | "advanced_config.toml"
            | "nethernet-key.der"
    ) || suffix_text.ends_with(".sock")
        || suffix_text.starts_with("logs/")
        || suffix_text.starts_with("run/")
    {
        return Ok(None);
    }
    let normalized = if let Some(path) = suffix_text.strip_prefix("players/data/") {
        format!("playerdata/{path}")
    } else if let Some(path) = suffix_text.strip_prefix("players/advancements/") {
        format!("advancements/{path}")
    } else if let Some(path) = suffix_text.strip_prefix("players/stats/") {
        format!("stats/{path}")
    } else if suffix_text.starts_with("players/") {
        return Ok(None);
    } else {
        suffix_text
    };
    safe_relative_path(&normalized)?;
    Ok(Some(normalized))
}
