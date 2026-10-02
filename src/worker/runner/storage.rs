use std::path::Path;

use anyhow::{Context, Result, bail};
use walkdir::WalkDir;

use crate::{model::SERVER_STORAGE_BYTES, snapshot::is_runtime_file};

pub(super) fn persistent_size(root: &Path) -> Result<i64> {
    if !root.exists() {
        return Ok(0);
    }
    let mut bytes = 0_i64;
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(root)?;
        if is_runtime_file(relative) {
            continue;
        }
        bytes = bytes
            .checked_add(i64::try_from(entry.metadata()?.len())?)
            .context("storage size overflow")?;
    }
    Ok(bytes)
}

pub(super) fn ensure_storage_limit(bytes: i64) -> Result<()> {
    if bytes > SERVER_STORAGE_BYTES {
        bail!("server storage quota exceeded: {bytes} > {SERVER_STORAGE_BYTES}");
    }
    Ok(())
}
