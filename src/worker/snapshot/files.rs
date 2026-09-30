use std::path::{Path as FsPath, PathBuf};

use anyhow::{Result, bail};
use walkdir::WalkDir;

use crate::worker::persistence::is_runtime_file;

pub(super) struct ScannedFile {
    pub(super) relative: String,
    pub(super) absolute: PathBuf,
    pub(super) size: u64,
}

pub(super) fn collect_files(root: &FsPath) -> Result<Vec<ScannedFile>> {
    let mut files = Vec::new();
    if !root.exists() {
        return Ok(files);
    }
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(root)?;
        if is_runtime_file(relative) {
            continue;
        }
        let relative = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        files.push(ScannedFile {
            relative,
            absolute: entry.path().to_path_buf(),
            size: entry.metadata()?.len(),
        });
    }
    Ok(files)
}

pub(super) fn safe_relative_path(value: &str) -> Result<PathBuf> {
    let path = FsPath::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        bail!("snapshot contains unsafe path");
    }
    Ok(path.to_path_buf())
}
