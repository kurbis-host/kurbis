use std::{
    collections::HashSet,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use tar::{Archive, Builder, Header};
use tempfile::TempDir;
use thiserror::Error;
use walkdir::WalkDir;

pub const MAX_COMPRESSED_SOURCE_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_EXPANDED_SOURCE_BYTES: u64 = 768 * 1024 * 1024;
pub const MAX_SOURCE_FILE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_SOURCE_ENTRIES: usize = 100_000;
pub const MAX_ARTIFACT_BYTES: usize = 1024 * 1024 * 1024;
const SOURCE_BUNDLE_MAGIC: &[u8] = b"KURBIS-CI-SOURCE-BUNDLE-V1\0";
const MAX_SOURCE_BUNDLE_SUBMODULES: usize = 8;
pub const TRUSTED_PUMPKIN_SUBMODULE_PATHS: &[&str] = &["crates/pumpkin-plugin-wit"];

#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("source archive is invalid: {0}")]
    Invalid(String),
    #[error("source archive exceeds configured size limits")]
    Limit,
    #[error("source archive IO failed: {0}")]
    Io(#[from] std::io::Error),
}

/// A private build directory whose contents were extracted with strict path and type checks.
pub struct SourceWorkspace {
    temporary: TempDir,
    source: PathBuf,
    expanded_bytes: u64,
    entry_count: usize,
}

/// Compressed upstream source parts. This framing keeps archive expansion out
/// of the control-plane process; the rootless preprocessor safely extracts and
/// merges each part inside its quota-bounded container.
pub struct SourceBundle {
    pub primary: Vec<u8>,
    pub submodules: Vec<(String, Vec<u8>)>,
}

pub fn encode_source_bundle(
    primary: &[u8],
    submodules: &[(String, Vec<u8>)],
) -> Result<Vec<u8>, ArchiveError> {
    if primary.is_empty()
        || primary.len() > MAX_COMPRESSED_SOURCE_BYTES
        || submodules.len() > MAX_SOURCE_BUNDLE_SUBMODULES
    {
        return Err(ArchiveError::Limit);
    }
    let mut output = Vec::new();
    output.extend_from_slice(SOURCE_BUNDLE_MAGIC);
    append_bundle_length(&mut output, primary.len())?;
    output.extend_from_slice(primary);
    output.extend_from_slice(
        &u32::try_from(submodules.len())
            .map_err(|_| ArchiveError::Limit)?
            .to_be_bytes(),
    );
    let mut seen = HashSet::new();
    for (path, archive) in submodules {
        let relative = checked_bundle_path(path)?;
        if !TRUSTED_PUMPKIN_SUBMODULE_PATHS.contains(&path.as_str())
            || archive.is_empty()
            || archive.len() > MAX_COMPRESSED_SOURCE_BYTES
            || !seen.insert(relative.clone())
        {
            return Err(ArchiveError::Limit);
        }
        let path_bytes = path.as_bytes();
        let path_length = u16::try_from(path_bytes.len()).map_err(|_| ArchiveError::Limit)?;
        output.extend_from_slice(&path_length.to_be_bytes());
        output.extend_from_slice(path_bytes);
        append_bundle_length(&mut output, archive.len())?;
        output.extend_from_slice(archive);
        if output.len() > MAX_COMPRESSED_SOURCE_BYTES {
            return Err(ArchiveError::Limit);
        }
    }
    if output.len() > MAX_COMPRESSED_SOURCE_BYTES {
        return Err(ArchiveError::Limit);
    }
    Ok(output)
}

pub fn decode_source_bundle(bytes: &[u8]) -> Result<SourceBundle, ArchiveError> {
    if bytes.is_empty() || bytes.len() > MAX_COMPRESSED_SOURCE_BYTES {
        return Err(ArchiveError::Limit);
    }
    if !bytes.starts_with(SOURCE_BUNDLE_MAGIC) {
        return Ok(SourceBundle {
            primary: bytes.to_vec(),
            submodules: Vec::new(),
        });
    }
    let mut cursor = SOURCE_BUNDLE_MAGIC.len();
    let primary_length = take_bundle_length(bytes, &mut cursor)?;
    if primary_length == 0 || primary_length > MAX_COMPRESSED_SOURCE_BYTES {
        return Err(ArchiveError::Limit);
    }
    let primary = take_bundle_bytes(bytes, &mut cursor, primary_length)?.to_vec();
    let count_end = cursor.checked_add(4).ok_or(ArchiveError::Limit)?;
    let count_bytes = bytes.get(cursor..count_end).ok_or_else(|| {
        ArchiveError::Invalid("source bundle ended before its submodule count".into())
    })?;
    let count =
        u32::from_be_bytes(count_bytes.try_into().map_err(|_| ArchiveError::Limit)?) as usize;
    cursor = count_end;
    if count > MAX_SOURCE_BUNDLE_SUBMODULES {
        return Err(ArchiveError::Limit);
    }
    let mut submodules = Vec::with_capacity(count);
    let mut seen = HashSet::new();
    for _ in 0..count {
        let path_length_end = cursor.checked_add(2).ok_or(ArchiveError::Limit)?;
        let path_length = u16::from_be_bytes(
            bytes
                .get(cursor..path_length_end)
                .ok_or_else(|| {
                    ArchiveError::Invalid("source bundle ended before a submodule path".into())
                })?
                .try_into()
                .map_err(|_| ArchiveError::Limit)?,
        ) as usize;
        cursor = path_length_end;
        let path = std::str::from_utf8(take_bundle_bytes(bytes, &mut cursor, path_length)?)
            .map_err(|_| ArchiveError::Invalid("submodule path is not UTF-8".into()))?
            .to_owned();
        let relative = checked_bundle_path(&path)?;
        if !TRUSTED_PUMPKIN_SUBMODULE_PATHS.contains(&path.as_str()) {
            return Err(ArchiveError::Invalid(
                "source bundle contains an unapproved Pumpkin submodule".into(),
            ));
        }
        if !seen.insert(relative) {
            return Err(ArchiveError::Invalid(
                "source bundle contains duplicate submodule paths".into(),
            ));
        }
        let archive_length = take_bundle_length(bytes, &mut cursor)?;
        if archive_length == 0 || archive_length > MAX_COMPRESSED_SOURCE_BYTES {
            return Err(ArchiveError::Limit);
        }
        let archive = take_bundle_bytes(bytes, &mut cursor, archive_length)?.to_vec();
        submodules.push((path, archive));
    }
    if cursor != bytes.len() {
        return Err(ArchiveError::Invalid(
            "source bundle contains trailing bytes".into(),
        ));
    }
    Ok(SourceBundle {
        primary,
        submodules,
    })
}

fn append_bundle_length(output: &mut Vec<u8>, length: usize) -> Result<(), ArchiveError> {
    output.extend_from_slice(
        &u64::try_from(length)
            .map_err(|_| ArchiveError::Limit)?
            .to_be_bytes(),
    );
    Ok(())
}

fn take_bundle_length(bytes: &[u8], cursor: &mut usize) -> Result<usize, ArchiveError> {
    let end = cursor.checked_add(8).ok_or(ArchiveError::Limit)?;
    let encoded = bytes.get(*cursor..end).ok_or_else(|| {
        ArchiveError::Invalid("source bundle ended before an archive length".into())
    })?;
    *cursor = end;
    let length = u64::from_be_bytes(encoded.try_into().map_err(|_| ArchiveError::Limit)?);
    usize::try_from(length).map_err(|_| ArchiveError::Limit)
}

fn take_bundle_bytes<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
    length: usize,
) -> Result<&'a [u8], ArchiveError> {
    let end = cursor.checked_add(length).ok_or(ArchiveError::Limit)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or_else(|| ArchiveError::Invalid("source bundle is truncated".into()))?;
    *cursor = end;
    Ok(value)
}

fn checked_bundle_path(path: &str) -> Result<PathBuf, ArchiveError> {
    let relative = checked_relative_path(Path::new(path))?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ArchiveError::Invalid("invalid source bundle path".into()));
    }
    Ok(relative)
}

impl SourceWorkspace {
    pub fn path(&self) -> &Path {
        &self.source
    }

    pub fn temporary_root(&self) -> &Path {
        self.temporary.path()
    }
}

fn validate_global_pax_header<R: Read>(entry: &mut tar::Entry<'_, R>) -> Result<(), ArchiveError> {
    const MAX_GLOBAL_PAX_BYTES: u64 = 64 * 1024;
    if entry.size() > MAX_GLOBAL_PAX_BYTES {
        return Err(ArchiveError::Limit);
    }
    let extensions = entry
        .pax_extensions()?
        .ok_or_else(|| ArchiveError::Invalid("global PAX header has no records".into()))?;
    for extension in extensions {
        let extension = extension?;
        // GitHub's codeload tarballs include a benign commit `comment` global
        // header. Reject every other global attribute: global `path`, `size`,
        // link, sparse, ownership, and permission overrides complicate the
        // archive parser's security model and are unnecessary for source code.
        if extension.key_bytes() != b"comment" {
            return Err(ArchiveError::Invalid(
                "source archive contains an unsupported global PAX attribute".into(),
            ));
        }
        if extension.value_bytes().len() > 1024 {
            return Err(ArchiveError::Limit);
        }
    }
    Ok(())
}

pub fn unpack_source_archive(bytes: &[u8]) -> Result<SourceWorkspace, ArchiveError> {
    let _preprocess_guard = archive_preprocess_guard();
    if bytes.is_empty() || bytes.len() > MAX_COMPRESSED_SOURCE_BYTES {
        return Err(ArchiveError::Limit);
    }
    let temporary = super::private_tempdir("kurbis-ci-source-")?;
    let source = temporary.path().join("source");
    fs::create_dir(&source)?;
    let decoder = GzDecoder::new(bytes);
    let mut archive = Archive::new(decoder);
    let entries = archive.entries()?;
    let mut seen = HashSet::new();
    let mut common_root: Option<OsPath> = None;
    let mut strip_root = None;
    let mut expanded_bytes = 0u64;
    let mut entry_count = 0usize;

    for entry in entries {
        let mut entry = entry?;
        if entry.header().entry_type().is_pax_global_extensions() {
            validate_global_pax_header(&mut entry)?;
            continue;
        }
        entry_count += 1;
        if entry_count > MAX_SOURCE_ENTRIES {
            return Err(ArchiveError::Limit);
        }
        let raw_path = entry.path_bytes();
        let path = std::str::from_utf8(&raw_path)
            .map_err(|_| ArchiveError::Invalid("archive paths must be UTF-8".into()))?;
        if path.contains('\\') {
            return Err(ArchiveError::Invalid(
                "archive path contains a backslash".into(),
            ));
        }
        let relative = checked_relative_path(Path::new(path))?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let first = relative
            .components()
            .next()
            .and_then(|component| match component {
                Component::Normal(value) => Some(value.to_os_string()),
                _ => None,
            })
            .ok_or_else(|| ArchiveError::Invalid("archive path has no root component".into()))?;
        if common_root.as_ref().is_some_and(|root| root.0 != first) {
            if strip_root == Some(true) {
                return Err(ArchiveError::Invalid(
                    "GitHub source archive contains multiple root folders".into(),
                ));
            }
            strip_root = Some(false);
        } else if common_root.is_none() {
            common_root = Some(OsPath(first.clone()));
        }
        if strip_root.is_none() {
            // GitHub archives contain a single explicit top-level directory. Normalized archives do not.
            strip_root =
                Some(entry.header().entry_type().is_dir() && relative.components().count() == 1);
        }
        let components = relative.components().collect::<Vec<_>>();
        let relative = if strip_root == Some(true) {
            if components.len() == 1 {
                continue;
            }
            components[1..]
                .iter()
                .fold(PathBuf::new(), |mut path, component| {
                    if let Component::Normal(value) = component {
                        path.push(value);
                    }
                    path
                })
        } else {
            relative
        };
        if relative.as_os_str().is_empty() {
            continue;
        }
        if relative
            .components()
            .any(|component| matches!(component, Component::Normal(value) if value == ".git"))
        {
            return Err(ArchiveError::Invalid(
                "source archives may not contain a .git directory".into(),
            ));
        }
        let name = relative.to_string_lossy().into_owned();
        if !seen.insert(name) {
            return Err(ArchiveError::Invalid(
                "archive contains duplicate paths".into(),
            ));
        }
        let target = source.join(&relative);
        match entry.header().entry_type() {
            kind if kind.is_dir() => {
                fs::create_dir_all(&target)?;
                set_safe_mode(&target, 0o755)?;
            }
            kind if kind.is_file() => {
                // `Entry::size` includes validated per-file PAX overrides;
                // header.size alone may be stale when an archive uses PAX.
                let size = entry.size();
                if size > MAX_SOURCE_FILE_BYTES {
                    return Err(ArchiveError::Limit);
                }
                expanded_bytes = expanded_bytes
                    .checked_add(size)
                    .ok_or(ArchiveError::Limit)?;
                if expanded_bytes > MAX_EXPANDED_SOURCE_BYTES {
                    return Err(ArchiveError::Limit);
                }
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)?;
                let mut limited = (&mut entry).take(MAX_SOURCE_FILE_BYTES + 1);
                let copied = std::io::copy(&mut limited, &mut file)?;
                if copied != size {
                    return Err(ArchiveError::Invalid(
                        "archive entry size did not match its header".into(),
                    ));
                }
                let mode = entry.header().mode().unwrap_or(0o644) & 0o111;
                set_safe_mode(&target, if mode > 0 { 0o755 } else { 0o644 })?;
            }
            _ => {
                return Err(ArchiveError::Invalid(
                    "archive contains a link or special file".into(),
                ));
            }
        }
    }
    if !source.join("Cargo.toml").is_file() {
        return Err(ArchiveError::Invalid(
            "source archive does not have a root Cargo.toml".into(),
        ));
    }
    Ok(SourceWorkspace {
        temporary,
        source,
        expanded_bytes,
        entry_count: entry_count.min(MAX_SOURCE_ENTRIES),
    })
}

pub fn merge_source_submodule(
    workspace: &mut SourceWorkspace,
    path: &str,
    archive: &[u8],
) -> Result<(), ArchiveError> {
    let relative = checked_bundle_path(path)?;
    if !TRUSTED_PUMPKIN_SUBMODULE_PATHS.contains(&path) {
        return Err(ArchiveError::Invalid(
            "source bundle contains an unapproved Pumpkin submodule".into(),
        ));
    }
    let submodule = unpack_source_archive(archive)?;
    if workspace
        .expanded_bytes
        .checked_add(submodule.expanded_bytes)
        .is_none_or(|total| total > MAX_EXPANDED_SOURCE_BYTES)
        || workspace
            .entry_count
            .checked_add(submodule.entry_count)
            .is_none_or(|count| count > MAX_SOURCE_ENTRIES)
    {
        return Err(ArchiveError::Limit);
    }
    let target = workspace.source.join(relative);
    if !target.exists() {
        fs::create_dir_all(&target)?;
    }
    if fs::read_dir(&target)?.next().is_some() {
        return Err(ArchiveError::Invalid(
            "submodule archive target is not empty".into(),
        ));
    }
    for entry in WalkDir::new(submodule.path())
        .follow_links(false)
        .min_depth(1)
    {
        let entry = entry.map_err(|error| ArchiveError::Invalid(error.to_string()))?;
        let relative = entry
            .path()
            .strip_prefix(submodule.path())
            .map_err(|error| ArchiveError::Invalid(error.to_string()))?;
        let destination = target.join(relative);
        if entry.file_type().is_symlink() {
            return Err(ArchiveError::Invalid(
                "submodule archive contains a symlink".into(),
            ));
        }
        if entry.file_type().is_dir() {
            fs::create_dir_all(&destination)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &destination)?;
        } else {
            return Err(ArchiveError::Invalid(
                "submodule archive contains a special file".into(),
            ));
        }
    }
    workspace.expanded_bytes += submodule.expanded_bytes;
    workspace.entry_count += submodule.entry_count;
    Ok(())
}

pub fn pack_source_archive(source: &Path) -> Result<Vec<u8>, ArchiveError> {
    let _preprocess_guard = archive_preprocess_guard();
    let output = Vec::new();
    let encoder = GzEncoder::new(output, Compression::default());
    let mut archive = Builder::new(encoder);
    archive.mode(tar::HeaderMode::Deterministic);
    let mut total = 0u64;
    let mut count = 0usize;
    let mut directories = WalkDir::new(source)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ArchiveError::Invalid(error.to_string()))?;
    directories.sort_by(|left, right| left.path().cmp(right.path()));
    for entry in directories {
        let path = entry.path();
        if path == source {
            continue;
        }
        let relative = path
            .strip_prefix(source)
            .map_err(|_| ArchiveError::Invalid("source path escaped workspace".into()))?;
        let relative = checked_relative_path(relative)?;
        if relative
            .components()
            .any(|component| matches!(component, Component::Normal(value) if value == ".git"))
        {
            continue;
        }
        if entry.file_type().is_symlink() {
            return Err(ArchiveError::Invalid(
                "patched source contains a symlink".into(),
            ));
        }
        count += 1;
        if count > MAX_SOURCE_ENTRIES {
            return Err(ArchiveError::Limit);
        }
        let metadata = fs::metadata(path)?;
        let mode = if metadata.permissions().readonly() {
            0o444
        } else if is_executable(&metadata) {
            0o755
        } else {
            0o644
        };
        let mut header = Header::new_gnu();
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_mode(mode);
        if entry.file_type().is_dir() {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_cksum();
            archive.append_data(&mut header, relative, std::io::empty())?;
        } else if entry.file_type().is_file() {
            let size = metadata.len();
            total = total.checked_add(size).ok_or(ArchiveError::Limit)?;
            if size > MAX_SOURCE_FILE_BYTES || total > MAX_EXPANDED_SOURCE_BYTES {
                return Err(ArchiveError::Limit);
            }
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(size);
            header.set_cksum();
            archive.append_data(&mut header, relative, File::open(path)?)?;
        } else {
            return Err(ArchiveError::Invalid(
                "source contains a special file".into(),
            ));
        }
    }
    let encoder = archive.into_inner()?;
    let bytes = encoder.finish()?;
    if bytes.is_empty() || bytes.len() > MAX_COMPRESSED_SOURCE_BYTES {
        return Err(ArchiveError::Limit);
    }
    Ok(bytes)
}

fn archive_preprocess_guard() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn checked_relative_path(path: &Path) -> Result<PathBuf, ArchiveError> {
    if path.is_absolute() {
        return Err(ArchiveError::Invalid("archive path is absolute".into()));
    }
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) if value != "." && value != ".." => relative.push(value),
            Component::CurDir => {}
            _ => {
                return Err(ArchiveError::Invalid(
                    "archive path traverses its root".into(),
                ));
            }
        }
    }
    Ok(relative)
}

#[cfg(unix)]
fn set_safe_mode(path: &Path, mode: u32) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_safe_mode(_path: &Path, _mode: u32) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}

#[derive(Clone)]
struct OsPath(std::ffi::OsString);

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn global_pax_archive(source: &[u8], key: &str, value: &str) -> Vec<u8> {
        let body = format!("{key}={value}\n");
        let mut length = body.len() + 2;
        let record = loop {
            let record = format!("{length} {body}");
            if record.len() == length {
                break record.into_bytes();
            }
            length = record.len();
        };
        let mut header = Header::new_gnu();
        header.set_path("pax_global_header").unwrap();
        header.set_entry_type(tar::EntryType::XGlobalHeader);
        header.set_size(record.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        let mut raw = header.as_bytes().to_vec();
        raw.extend_from_slice(&record);
        let padding = (512 - (record.len() % 512)) % 512;
        raw.resize(raw.len() + padding, 0);
        let mut source_tar = Vec::new();
        GzDecoder::new(source).read_to_end(&mut source_tar).unwrap();
        raw.extend_from_slice(&source_tar);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&raw).unwrap();
        encoder.finish().unwrap()
    }

    fn archive(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = Builder::new(&mut tar_bytes);
            for (path, entry_type, data) in entries {
                let mut header = Header::new_gnu();
                header.set_entry_type(*entry_type);
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, *path, *data).unwrap();
            }
            builder.finish().unwrap();
        }
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&tar_bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn github_root_is_stripped_and_source_is_repacked_deterministically() {
        let bytes = archive(&[
            ("Pumpkin-0123/", tar::EntryType::Directory, b""),
            (
                "Pumpkin-0123/Cargo.toml",
                tar::EntryType::Regular,
                b"[workspace]",
            ),
        ]);
        let first = unpack_source_archive(&bytes).unwrap();
        assert_eq!(
            fs::read(first.path().join("Cargo.toml")).unwrap(),
            b"[workspace]"
        );
        let repacked = pack_source_archive(first.path()).unwrap();
        let second = unpack_source_archive(&repacked).unwrap();
        assert_eq!(
            fs::read(second.path().join("Cargo.toml")).unwrap(),
            b"[workspace]"
        );
        assert_eq!(repacked, pack_source_archive(second.path()).unwrap());
    }

    #[test]
    fn source_bundle_round_trips_compressed_parts_and_merges_only_in_sandbox_workspace() {
        let primary = archive(&[
            ("Pumpkin/", tar::EntryType::Directory, b""),
            (
                "Pumpkin/Cargo.toml",
                tar::EntryType::Regular,
                b"[workspace]",
            ),
            (
                "Pumpkin/crates/pumpkin-plugin-wit/",
                tar::EntryType::Directory,
                b"",
            ),
        ]);
        let submodule = archive(&[
            ("plugin-wit/", tar::EntryType::Directory, b""),
            (
                "plugin-wit/Cargo.toml",
                tar::EntryType::Regular,
                b"[package]",
            ),
            (
                "plugin-wit/src/lib.rs",
                tar::EntryType::Regular,
                b"pub fn trusted_source() {}",
            ),
        ]);
        let bundle =
            encode_source_bundle(&primary, &[("crates/pumpkin-plugin-wit".into(), submodule)])
                .unwrap();
        let decoded = decode_source_bundle(&bundle).unwrap();
        assert_eq!(decoded.submodules.len(), 1);
        let mut workspace = unpack_source_archive(&decoded.primary).unwrap();
        merge_source_submodule(
            &mut workspace,
            &decoded.submodules[0].0,
            &decoded.submodules[0].1,
        )
        .unwrap();
        assert_eq!(
            fs::read(
                workspace
                    .path()
                    .join("crates/pumpkin-plugin-wit/src/lib.rs")
            )
            .unwrap(),
            b"pub fn trusted_source() {}"
        );
    }

    #[test]
    fn extraction_ignores_only_a_bounded_benign_github_global_pax_comment() {
        let source = archive(&[
            ("Pumpkin/", tar::EntryType::Directory, b""),
            (
                "Pumpkin/Cargo.toml",
                tar::EntryType::Regular,
                b"[workspace]",
            ),
        ]);
        let wrapped = global_pax_archive(&source, "comment", "validated-fixture-commit");
        let workspace = unpack_source_archive(&wrapped).unwrap();
        assert_eq!(
            fs::read(workspace.path().join("Cargo.toml")).unwrap(),
            b"[workspace]"
        );
    }

    #[test]
    fn extraction_rejects_global_pax_path_overrides() {
        let source = archive(&[("Cargo.toml", tar::EntryType::Regular, b"[workspace]")]);
        let wrapped = global_pax_archive(&source, "path", "../../escape");
        assert!(unpack_source_archive(&wrapped).is_err());
    }

    #[test]
    fn source_bundle_rejects_untrusted_paths_and_trailing_data() {
        assert!(
            encode_source_bundle(
                b"compressed source",
                &[("../../outside".into(), b"archive".to_vec())]
            )
            .is_err()
        );
        let mut valid = encode_source_bundle(b"compressed source", &[]).unwrap();
        valid.push(0);
        assert!(decode_source_bundle(&valid).is_err());
        assert!(decode_source_bundle(b"KURBIS-CI-SOURCE-BUNDLE-V1\0short").is_err());
    }

    #[test]
    fn extraction_rejects_parent_paths_and_symlinks() {
        assert!(checked_relative_path(Path::new("Pumpkin/../escape")).is_err());
        let link = archive(&[("Pumpkin/link", tar::EntryType::Symlink, b"../../secret")]);
        assert!(unpack_source_archive(&link).is_err());
    }
}
