use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use serde::Deserialize;
use tar::Archive;
use uuid::Uuid;

use crate::{
    ci::{CI_RUNTIME_DISK_BYTES, CI_RUNTIME_MEMORY_BYTES},
    worker::sandbox::WorkloadLimits,
};

#[derive(Debug, Deserialize)]
pub(super) struct ArtifactMetadata {
    pub(super) repository: String,
    pub(super) commit_sha: String,
    pub(super) target_architecture: String,
    format_version: u32,
}

#[cfg(test)]
pub(super) fn unpack_artifact(bytes: &[u8], target: &Path) -> Result<ArtifactMetadata> {
    if bytes.is_empty() || bytes.len() > 1024 * 1024 * 1024 {
        bail!("CI artifact has invalid size");
    }
    unpack_artifact_reader(std::io::Cursor::new(bytes), target)
}

pub(super) fn unpack_artifact_file(path: &Path, target: &Path) -> Result<ArtifactMetadata> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("open CI artifact staging file {}", path.display()))?;
    if file.metadata()?.len() == 0 || file.metadata()?.len() > 1024 * 1024 * 1024 {
        bail!("CI artifact has invalid size");
    }
    unpack_artifact_reader(file, target)
}

fn unpack_artifact_reader<R: Read>(reader: R, target: &Path) -> Result<ArtifactMetadata> {
    let decoder = GzDecoder::new(reader);
    let mut archive = Archive::new(decoder);
    let mut found = std::collections::HashSet::new();
    let mut metadata = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        if path.components().count() != 1 {
            bail!("CI artifact contains a nested path");
        }
        let name = path
            .to_str()
            .context("CI artifact entry name is not UTF-8")?
            .to_owned();
        if ![
            "pumpkin",
            "kurbis-control.wasm",
            "metadata.json",
            "BUILD.log",
            "LICENSE",
            "REVISION",
        ]
        .contains(&name.as_str())
            || !found.insert(name.clone())
        {
            bail!("CI artifact contains an unexpected or duplicate entry");
        }
        if !entry.header().entry_type().is_file() {
            bail!("CI artifact contains a non-regular file");
        }
        let entry_size = entry.size();
        let limit = if name == "pumpkin" {
            512 * 1024 * 1024
        } else {
            64 * 1024 * 1024
        };
        if entry_size > limit {
            bail!("CI artifact entry is too large");
        }
        let output = target.join(&name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(if name == "pumpkin" { 0o755 } else { 0o644 })
            .open(output)?;
        let mut copy = (&mut entry).take(limit + 1);
        let mut content = Vec::new();
        copy.read_to_end(&mut content)?;
        if content.len() as u64 != entry_size {
            bail!("CI artifact entry was truncated");
        }
        if name == "metadata.json" {
            metadata = Some(serde_json::from_slice::<ArtifactMetadata>(&content)?);
        }
        file.write_all(&content)?;
        file.sync_all()?;
    }
    for required in ["pumpkin", "kurbis-control.wasm", "metadata.json"] {
        if !found.contains(required) {
            bail!("CI artifact is missing {required}");
        }
    }
    let metadata = metadata.context("CI artifact metadata is missing")?;
    if metadata.format_version != 1 {
        bail!("unsupported CI artifact format");
    }
    Ok(metadata)
}

pub(super) fn read_managed_artifact(source: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(source)
        .with_context(|| format!("open managed CI artifact at {}", source.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        bail!("managed CI artifact is not a bounded regular file");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() {
        bail!("managed CI artifact changed while it was read");
    }
    Ok(bytes)
}

pub(super) fn open_directory(path: &Path) -> Result<fs::File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("open non-symlink CI world directory {}", path.display()))
}

pub(super) fn ensure_child_directory(
    parent: &fs::File,
    name: &str,
    mode: u32,
    uid: u32,
    gid: u32,
) -> Result<fs::File> {
    let mode = libc::mode_t::try_from(mode).context("directory mode is out of range")?;
    let name = std::ffi::CString::new(name)?;
    let mut fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), mode) } < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.into());
            }
        }
        fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
    }
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("open safe CI world subdirectory");
    }
    let directory = unsafe { fs::File::from_raw_fd(fd) };
    set_owner_mode(&directory, uid, gid, u32::from(mode))?;
    Ok(directory)
}

pub(super) fn set_owner_mode(file: &fs::File, uid: u32, gid: u32, mode: u32) -> Result<()> {
    let mode = libc::mode_t::try_from(mode).context("file mode is out of range")?;
    if unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } < 0 {
        return Err(std::io::Error::last_os_error()).context("set safe CI world ownership");
    }
    if unsafe { libc::fchmod(file.as_raw_fd(), mode) } < 0 {
        return Err(std::io::Error::last_os_error()).context("set safe CI world permissions");
    }
    Ok(())
}

pub(super) fn atomic_write_at(
    parent: &fs::File,
    name: &str,
    bytes: &[u8],
    mode: u32,
    uid: u32,
    gid: u32,
) -> Result<()> {
    let name = std::ffi::CString::new(name)?;
    let temporary = std::ffi::CString::new(format!(".ci-{}.tmp", Uuid::new_v4()))?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            temporary.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("create CI world temporary file");
    }
    let mut file = unsafe { fs::File::from_raw_fd(fd) };
    let result = (|| -> Result<()> {
        set_owner_mode(&file, uid, gid, mode)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if unsafe {
            libc::renameat(
                parent.as_raw_fd(),
                temporary.as_ptr(),
                parent.as_raw_fd(),
                name.as_ptr(),
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error())
                .context("atomically publish CI world file");
        }
        Ok(())
    })();
    if result.is_err() {
        unsafe {
            libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), 0);
        }
    }
    result
}

pub(super) fn write_world_file(
    world: &Path,
    name: &str,
    bytes: &[u8],
    mode: u32,
    uid: u32,
    gid: u32,
) -> Result<()> {
    let root = open_directory(world)?;
    atomic_write_at(&root, name, bytes, mode, uid, gid)
}

pub(super) fn write_secret(path: &Path, value: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(value.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

pub(super) fn write_runtime_owner(runtime_dir: &Path, preview_id: Uuid) -> Result<()> {
    // Keep the marker creation atomic and reject a pre-existing symlink. The
    // owner marker is created exactly once for a newly provisioned runtime;
    // using the path-based O_NOFOLLOW path here also works on macOS, where
    // renameat through a read-only directory descriptor is not reliable.
    let marker = runtime_dir.join(".ci-preview-owner");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .mode(0o600)
        .open(&marker)
        .with_context(|| format!("create managed CI owner marker at {}", marker.display()))?;
    file.write_all(preview_id.to_string().as_bytes())?;
    file.sync_all()?;
    Ok(())
}

pub(super) fn remove_owned_runtime_dir(allowed_root: &Path, preview_id: Uuid) -> Result<()> {
    let path = allowed_root.join(preview_id.to_string());
    let root =
        fs::canonicalize(allowed_root).context("CI runtime allowlisted root is unavailable")?;
    let resolved = match fs::canonicalize(&path) {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if resolved.parent() != Some(root.as_path()) || resolved == root {
        bail!("refusing CI runtime deletion outside its managed root");
    }
    let owner = read_managed_artifact(&resolved.join(".ci-preview-owner"), 64)?;
    if owner != preview_id.to_string().as_bytes() {
        bail!("CI runtime cleanup ownership marker does not match preview");
    }
    let metadata = fs::symlink_metadata(&resolved)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("refusing to remove non-directory CI runtime path");
    }
    fs::remove_dir_all(resolved)?;
    Ok(())
}

pub(super) fn container_name(preview_id: Uuid) -> String {
    format!("kurbis-ci-{}", preview_id.simple())
}
pub(super) fn ci_limits() -> WorkloadLimits {
    WorkloadLimits::server(CI_RUNTIME_MEMORY_BYTES as u64, CI_RUNTIME_DISK_BYTES as u64)
}

pub(super) fn ensure_linux() -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("CI previews require a Linux rootless Docker worker");
    }
    Ok(())
}
