use std::{
    collections::HashSet,
    fs::{self, File},
    io::Read,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::Path,
};

use anyhow::{Context, Result, bail};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use tar::{Archive, Builder, Header};
use walkdir::WalkDir;

const MAX_WORLD_ENTRIES: usize = 250_000;

pub(super) fn pack_world(source: &Path, destination: &Path, maximum: u64) -> Result<(u64, String)> {
    let output = File::create(destination).context("create CI world snapshot archive")?;
    let encoder = GzEncoder::new(output, Compression::default());
    let mut archive = Builder::new(encoder);
    archive.mode(tar::HeaderMode::Deterministic);
    let mut entries = 0_usize;
    let mut expanded = 0_u64;
    let paths = WalkDir::new(source)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();
    for entry in paths {
        let entry = entry?;
        let path = entry.path();
        if path == source {
            continue;
        }
        if entry.file_type().is_symlink()
            || !(entry.file_type().is_dir() || entry.file_type().is_file())
        {
            bail!("CI world contains a symlink or special file");
        }
        let relative = path.strip_prefix(source)?;
        // These are worker/runtime-owned inputs, not durable world state.
        // The capability JSON contains a generation secret and pumpkin.toml
        // bakes ports/settings; both are regenerated on the next start.
        if relative == Path::new("pumpkin.toml")
            || relative == Path::new("plugins/kurbis-control.wasm")
            || relative == Path::new("plugins/data/kurbis-control/config.json")
            || relative.starts_with("run")
        {
            continue;
        }
        let name = relative
            .to_str()
            .context("CI world snapshot paths must be UTF-8")?;
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
            || name.contains('\\')
        {
            bail!("CI world snapshot contains an unsafe path");
        }
        entries += 1;
        if entries > MAX_WORLD_ENTRIES {
            bail!("CI world snapshot has too many files");
        }
        let metadata = fs::symlink_metadata(path)?;
        let mut header = Header::new_gnu();
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        if metadata.is_dir() {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_mode(0o700);
            header.set_size(0);
            header.set_cksum();
            archive.append_data(&mut header, relative, std::io::empty())?;
        } else {
            expanded = expanded
                .checked_add(metadata.len())
                .context("CI world size overflow")?;
            if expanded > maximum {
                bail!("CI world exceeds its configured storage quota");
            }
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(0o600);
            header.set_size(metadata.len());
            header.set_cksum();
            archive.append_data(&mut header, relative, File::open(path)?)?;
        }
    }
    let encoder = archive.into_inner()?;
    encoder.finish()?.sync_all()?;
    let size = fs::metadata(destination)?.len();
    if size == 0 || size > maximum {
        bail!("compressed CI world snapshot exceeds its transfer limit");
    }
    let mut file = File::open(destination)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok((size, hasher.finalize().to_hex().to_string()))
}

pub(super) fn unpack_world_snapshot(
    source: &Path,
    destination: &Path,
    maximum: u64,
    uid: u32,
    gid: u32,
) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > maximum
    {
        bail!("CI world snapshot archive is not a bounded regular file");
    }
    if !fs::symlink_metadata(destination)?.is_dir() {
        bail!("CI world restore target is not a directory");
    }
    let decoder = GzDecoder::new(File::open(source)?);
    let mut archive = Archive::new(decoder);
    let mut count = 0_usize;
    let mut expanded = 0_u64;
    let mut seen = HashSet::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path.as_os_str().is_empty()
            || path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
            || path.to_string_lossy().contains('\\')
        {
            bail!("CI world archive contains an unsafe path");
        }
        if !seen.insert(path.to_string_lossy().into_owned()) {
            bail!("CI world archive contains a duplicate path");
        }
        count += 1;
        if count > MAX_WORLD_ENTRIES {
            bail!("CI world archive has too many entries");
        }
        if entry.header().entry_type().is_dir() {
            if !entry.unpack_in(destination)? {
                bail!("CI world directory escaped the restore root");
            }
            continue;
        }
        if !entry.header().entry_type().is_file() {
            bail!("CI world archive contains a link or special file");
        }
        let size = entry.size();
        expanded = expanded
            .checked_add(size)
            .context("CI world expanded size overflow")?;
        if size > maximum || expanded > maximum {
            bail!("CI world archive expands beyond its configured quota");
        }
        if !entry.unpack_in(destination)? {
            bail!("CI world file escaped the restore root");
        }
    }
    set_tree_owner_and_mode(destination, uid, gid)?;
    Ok(())
}

fn set_tree_owner_and_mode(root: &Path, uid: u32, gid: u32) -> Result<()> {
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            bail!("restored CI world unexpectedly contains a symlink");
        }
        let path_c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: `path_c` is a NUL-terminated path from the validated, symlink-free
        // archive tree; ownership changes target only this worker-managed root.
        if unsafe { libc::chown(path_c.as_ptr(), uid, gid) } < 0 {
            return Err(std::io::Error::last_os_error()).context("set restored CI world ownership");
        }
        let mode = if metadata.is_dir() { 0o700 } else { 0o600 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_snapshot_round_trips_and_rejects_unsafe_archive_members() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let restored = root.path().join("restored");
        fs::create_dir_all(source.join("region")).unwrap();
        fs::create_dir_all(&restored).unwrap();
        fs::write(source.join("level.dat"), b"level data").unwrap();
        fs::write(source.join("region/r.0.0.mca"), vec![7; 4096]).unwrap();
        let archive = root.path().join("world.tar.gz");
        let (size, checksum) = pack_world(&source, &archive, 1024 * 1024).unwrap();
        assert!(size > 0);
        assert_eq!(checksum.len(), 64);
        unpack_world_snapshot(
            &archive,
            &restored,
            1024 * 1024,
            unsafe { libc::geteuid() },
            unsafe { libc::getegid() },
        )
        .unwrap();
        assert_eq!(fs::read(restored.join("level.dat")).unwrap(), b"level data");
        assert_eq!(
            fs::read(restored.join("region/r.0.0.mca")).unwrap(),
            vec![7; 4096]
        );

        let bad_archive = root.path().join("bad.tar.gz");
        let encoder = GzEncoder::new(File::create(&bad_archive).unwrap(), Compression::default());
        let mut tar = Builder::new(encoder);
        let mut header = Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_link_name("../../outside").unwrap();
        header.set_cksum();
        tar.append_data(&mut header, "escape", std::io::empty())
            .unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        let empty = root.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert!(
            unpack_world_snapshot(
                &bad_archive,
                &empty,
                1024 * 1024,
                unsafe { libc::geteuid() },
                unsafe { libc::getegid() },
            )
            .is_err()
        );
    }
}
