use std::path::Path as FsPath;

use anyhow::{Context, Result, bail};
use async_zip::{Compression, ZipEntryBuilder, tokio::write::ZipFileWriter};
use bytes::Bytes;
use futures::StreamExt;
use futures_lite::io::AsyncWriteExt as ZipWriteExt;
use object_store::{MultipartUpload, ObjectStore, path::Path};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use super::{SnapshotManager, SnapshotManifest};
use crate::model::SnapshotRecord;

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
        let mut guard = self.storage_guard(false).await?;
        let snapshot = sqlx::query_as::<_, SnapshotRecord>(
            "SELECT id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete, created_at
             FROM snapshots WHERE server_id = $1 AND generation = $2 AND complete",
        )
        .bind(server_id)
        .bind(generation)
        .fetch_optional(&mut *guard)
        .await?
        .context("pinned world export snapshot is no longer retained")?;
        let pin_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM world_exports
             WHERE id = $1 AND server_id = $2 AND source_generation = $3
               AND status = 'running')",
        )
        .bind(export_id)
        .bind(server_id)
        .bind(generation)
        .fetch_one(&mut *guard)
        .await?;
        if !pin_exists {
            bail!("world export source snapshot is not pinned by an active export");
        }
        let manifest = self.read_manifest(&snapshot).await?;
        let output = staging_root.join(format!("{export_id}-{attempt}.zip.part"));
        if let Some(parent) = output.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let result = self.write_world_zip(export_id, &manifest, &output).await;
        if let Err(error) = result {
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
        if !files.iter().any(|(_, path)| path == "level.dat") {
            bail!("world snapshot has no level.dat and cannot be exported");
        }

        let output_file = tokio::fs::File::create(output).await?;
        let mut writer = ZipFileWriter::with_tokio(output_file).force_zip64();
        for (file, archive_path) in files {
            if self.export_cancelled(export_id).await? {
                bail!("world export was cancelled");
            }
            let object = self
                .store
                .get(&self.object_path(manifest.server_id, &format!("chunks/{}", file.blake3))?)
                .await?;
            let mut stream = object.into_stream();
            let entry = ZipEntryBuilder::new(archive_path.into(), Compression::Deflate);
            let mut entry_writer = writer.write_entry_stream(entry).await?;
            let mut hasher = blake3::Hasher::new();
            let mut size = 0_u64;
            while let Some(chunk) = stream.next().await {
                if self.export_cancelled(export_id).await? {
                    bail!("world export was cancelled");
                }
                let chunk: Bytes = chunk?;
                size = size
                    .checked_add(chunk.len() as u64)
                    .context("snapshot chunk size overflow")?;
                hasher.update(&chunk);
                entry_writer.write_all(&chunk).await?;
            }
            if size != file.size || hasher.finalize().to_hex().as_str() != file.blake3 {
                bail!(
                    "snapshot chunk failed integrity validation for {}",
                    file.path
                );
            }
            entry_writer.close().await?;
        }
        writer.close().await?;
        Ok(())
    }

    async fn export_cancelled(&self, export_id: Uuid) -> Result<bool> {
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM world_exports WHERE id = $1")
                .bind(export_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(status.as_deref() != Some("running"))
    }

    pub async fn upload_world_archive(&self, archive: &WorldArchive) -> Result<()> {
        let _guard = self.storage_guard(false).await?;
        let key = self.object_path(archive.server_id, &archive.archive_key)?;
        let mut upload = self.store.put_multipart(&key).await?;
        let result = stream_archive_parts(&mut upload, &archive.path).await;
        if let Err(error) = result {
            let _ = upload.abort().await;
            return Err(error);
        }
        upload.complete().await?;
        let (size, checksum) = hash_object(self, &key).await?;
        if size != archive.size_bytes as u64 || checksum != archive.checksum {
            let _ = self.store.delete(&key).await;
            bail!("uploaded world archive failed checksum verification");
        }
        Ok(())
    }

    pub async fn delete_world_archive(&self, server_id: Uuid, archive_key: &str) -> Result<()> {
        let _guard = self.storage_guard(false).await?;
        let key = self.object_path(server_id, archive_key)?;
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

async fn hash_object(manager: &SnapshotManager, key: &Path) -> Result<(u64, String)> {
    let result = manager.store.get(key).await?;
    let mut stream = result.into_stream();
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

fn archive_entry_path(source: &str) -> Result<Option<String>> {
    let source_path = crate::worker::snapshot::files::safe_relative_path(source)?;
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
    let normalized = if let Some(player_file) = suffix_text.strip_prefix("players/data/") {
        format!("playerdata/{player_file}")
    } else if let Some(advancement_file) = suffix_text.strip_prefix("players/advancements/") {
        format!("advancements/{advancement_file}")
    } else if let Some(stats_file) = suffix_text.strip_prefix("players/stats/") {
        format!("stats/{stats_file}")
    } else if suffix_text.starts_with("players/") {
        return Ok(None);
    } else {
        suffix_text
    };
    crate::worker::snapshot::files::safe_relative_path(&normalized)?;
    Ok(Some(normalized))
}

#[cfg(test)]
mod tests {
    use super::archive_entry_path;

    #[test]
    fn exports_only_standard_world_files_and_normalizes_pumpkin_player_data() {
        assert_eq!(
            archive_entry_path("world/players/data/abc.dat").unwrap(),
            Some("playerdata/abc.dat".into())
        );
        assert_eq!(
            archive_entry_path("world/players/advancements/abc.json").unwrap(),
            Some("advancements/abc.json".into())
        );
        assert_eq!(
            archive_entry_path("world/players/stats/abc.json").unwrap(),
            Some("stats/abc.json".into())
        );
        assert_eq!(
            archive_entry_path("world/DIM-1/region/r.0.0.mca").unwrap(),
            Some("DIM-1/region/r.0.0.mca".into())
        );
        assert_eq!(archive_entry_path("ops.json").unwrap(), None);
        assert_eq!(archive_entry_path("world/session.lock").unwrap(), None);
        assert_eq!(
            archive_entry_path("world/players/unrecognized.bin").unwrap(),
            None
        );
        assert!(archive_entry_path("world/../../secret").is_err());
    }
}
