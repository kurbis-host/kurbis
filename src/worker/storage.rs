use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use tokio::{fs, process::Command};
use uuid::Uuid;

use crate::config::StorageMode;
#[cfg(target_os = "linux")]
use crate::model::SERVER_STORAGE_BYTES;

#[derive(Clone)]
pub struct StorageManager {
    root: PathBuf,
    mode: StorageMode,
}

impl StorageManager {
    pub fn new(root: PathBuf, mode: StorageMode) -> Self {
        Self { root, mode }
    }

    pub fn path(&self, server_id: Uuid) -> PathBuf {
        self.root.join("servers").join(server_id.to_string())
    }

    pub async fn ensure(&self, server_id: Uuid) -> Result<PathBuf> {
        let path = self.path(server_id);
        fs::create_dir_all(&path).await?;
        match self.mode {
            StorageMode::Directory => {}
            StorageMode::LoopbackExt4 => self.ensure_loopback(server_id, &path).await?,
        }
        Ok(path)
    }

    pub async fn reset(&self, server_id: Uuid) -> Result<()> {
        let path = self.ensure(server_id).await?;
        self.assert_managed_path(server_id, &path)?;
        let mut entries = fs::read_dir(&path).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            if file_type.is_dir() && !file_type.is_symlink() {
                fs::remove_dir_all(entry.path()).await?;
            } else {
                fs::remove_file(entry.path()).await?;
            }
        }
        Ok(())
    }

    pub async fn remove(&self, server_id: Uuid) -> Result<()> {
        let path = self.path(server_id);
        self.assert_managed_path(server_id, &path)?;
        if self.mode == StorageMode::LoopbackExt4 && is_mountpoint(&path).await? {
            run(
                Command::new("umount").arg(&path),
                "unmount server filesystem",
            )
            .await?;
        }
        match fs::remove_dir_all(&path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if self.mode == StorageMode::LoopbackExt4 {
            let image = self.image_path(server_id);
            match fs::remove_file(&image).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            match fs::remove_file(image.with_extension("ext4.initializing")).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub async fn managed_server_ids(&self) -> Result<HashSet<Uuid>> {
        let mut ids = HashSet::new();
        collect_uuid_directories(&self.root.join("servers"), &mut ids).await?;
        if self.mode == StorageMode::LoopbackExt4 {
            collect_volume_ids(&self.root.join("volumes"), &mut ids).await?;
        }
        Ok(ids)
    }

    async fn ensure_loopback(&self, server_id: Uuid, mountpoint: &Path) -> Result<()> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (server_id, mountpoint);
            bail!("loopback_ext4 storage requires a Linux worker");
        }
        #[cfg(target_os = "linux")]
        {
            if is_mountpoint(mountpoint).await? {
                return Ok(());
            }
            let volumes = self.root.join("volumes");
            fs::create_dir_all(&volumes).await?;
            let image = self.image_path(server_id);
            if !fs::try_exists(&image).await? {
                // Publish only a formatted filesystem. An interrupted first
                // attempt must not leave an unformatted image that all retries
                // mistake for a usable volume.
                let initializing = image.with_extension("ext4.initializing");
                run(
                    Command::new("truncate")
                        .arg("-s")
                        .arg(SERVER_STORAGE_BYTES.to_string())
                        .arg(&initializing),
                    "create quota image",
                )
                .await?;
                run(
                    Command::new("mkfs.ext4")
                        .args(["-F", "-m", "0"])
                        .arg(&initializing),
                    "format quota image",
                )
                .await?;
                fs::rename(&initializing, &image).await?;
            }
            run(
                Command::new("mount")
                    .args(["-o", "loop,noatime,nosuid,nodev"])
                    .arg(&image)
                    .arg(mountpoint),
                "mount quota image",
            )
            .await?;
            if !is_mountpoint(mountpoint).await? {
                bail!("server quota filesystem did not become a mountpoint");
            }
            Ok(())
        }
    }

    fn image_path(&self, server_id: Uuid) -> PathBuf {
        self.root.join("volumes").join(format!("{server_id}.ext4"))
    }

    fn assert_managed_path(&self, server_id: Uuid, path: &Path) -> Result<()> {
        let expected = self.path(server_id);
        if path != expected
            || path.parent() != Some(self.root.join("servers").as_path())
            || path.file_name().and_then(|part| part.to_str())
                != Some(server_id.to_string().as_str())
        {
            bail!("refusing destructive storage operation outside managed server path");
        }
        Ok(())
    }
}

async fn collect_uuid_directories(root: &Path, ids: &mut HashSet<Uuid>) -> Result<()> {
    let mut entries = match fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str()
            && let Ok(id) = Uuid::parse_str(name)
        {
            ids.insert(id);
        }
    }
    Ok(())
}

async fn collect_volume_ids(root: &Path, ids: &mut HashSet<Uuid>) -> Result<()> {
    let mut entries = match fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|value| value.to_str())
            && let Some(stem) = name
                .strip_suffix(".ext4")
                .or_else(|| name.strip_suffix(".ext4.initializing"))
            && let Ok(id) = Uuid::parse_str(stem)
        {
            ids.insert(id);
        }
    }
    Ok(())
}

async fn is_mountpoint(path: &Path) -> Result<bool> {
    if !fs::try_exists(path).await? {
        return Ok(false);
    }
    let status = Command::new("mountpoint")
        .arg("-q")
        .arg(path)
        .status()
        .await
        .context("failed to execute mountpoint")?;
    mountpoint_result(status.code())
}

fn mountpoint_result(code: Option<i32>) -> Result<bool> {
    match code {
        Some(0) => Ok(true),
        Some(32) => Ok(false),
        _ => bail!("mountpoint failed with exit code {code:?}"),
    }
}

async fn run(command: &mut Command, action: &str) -> Result<()> {
    let output = command.output().await?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "{action} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountpoint_distinguishes_an_unmounted_directory_from_system_failure() {
        assert!(mountpoint_result(Some(0)).unwrap());
        assert!(!mountpoint_result(Some(32)).unwrap());
        for code in [Some(1), Some(2), None] {
            assert!(mountpoint_result(code).is_err());
        }
    }

    #[tokio::test]
    async fn interrupted_format_is_discovered_and_removed_without_a_mount_directory() {
        let temp = tempfile::tempdir().unwrap();
        let manager = StorageManager::new(temp.path().to_path_buf(), StorageMode::LoopbackExt4);
        let id = Uuid::new_v4();
        fs::create_dir_all(temp.path().join("volumes"))
            .await
            .unwrap();
        let pending = manager.image_path(id).with_extension("ext4.initializing");
        fs::write(&pending, b"incomplete filesystem").await.unwrap();
        assert_eq!(
            manager.managed_server_ids().await.unwrap(),
            HashSet::from([id])
        );
        manager.remove(id).await.unwrap();
        assert!(!pending.exists());
        assert!(manager.managed_server_ids().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn directory_mode_reset_stays_inside_server_root() {
        let temp = tempfile::tempdir().unwrap();
        let manager = StorageManager::new(temp.path().to_path_buf(), StorageMode::Directory);
        let id = Uuid::new_v4();
        let root = manager.ensure(id).await.unwrap();
        fs::create_dir_all(root.join("world/region")).await.unwrap();
        fs::write(root.join("world/region/r.0.0.mca"), b"world")
            .await
            .unwrap();

        manager.reset(id).await.unwrap();

        assert!(root.exists());
        assert!(
            fs::read_dir(&root)
                .await
                .unwrap()
                .next_entry()
                .await
                .unwrap()
                .is_none()
        );
    }
}
