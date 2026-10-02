use std::path::{Path as FsPath, PathBuf};

use anyhow::{Result, bail};
use walkdir::WalkDir;

use super::is_runtime_file;

pub(crate) struct ScannedFile {
    pub(crate) relative: String,
    pub(crate) absolute: PathBuf,
    pub(crate) size: u64,
}

pub(crate) fn collect_files(root: &FsPath) -> Result<Vec<ScannedFile>> {
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

pub(crate) fn safe_relative_path(value: &str) -> Result<PathBuf> {
    let path = FsPath::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        bail!("snapshot contains unsafe path");
    }
    Ok(path.to_path_buf())
}
