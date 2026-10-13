//! Files the worker manages inside a server's data directory.
//!
//! The runtime owns that directory and can plant symlinks in it, so every
//! path is opened one component at a time relative to its parent's handle and
//! never follows a link. A worker running as root hands each new file and
//! directory to the parent directory's owner, so the runtime account can read
//! and replace them.

use std::{
    ffi::{CStr, CString},
    fs::{File, OpenOptions},
    io::Write,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
};

use anyhow::{Context, Result};
use uuid::Uuid;

/// Replace `name` in `directory` with `bytes` in one rename.
pub(super) fn atomic_replace_at(
    directory: &File,
    name: &str,
    bytes: &[u8],
    mode: u32,
) -> Result<()> {
    let directory_metadata = directory.metadata()?;
    let temporary = format!(".{name}.{}.tmp", Uuid::new_v4());
    let temporary_c = CString::new(temporary.as_str())?;
    let name_c = CString::new(name)?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            temporary_c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("create managed temporary file");
    }
    let write_result = (|| -> Result<()> {
        let mut file = unsafe { File::from_raw_fd(fd) };
        if let Some((uid, gid)) = managed_file_owner(
            unsafe { libc::geteuid() },
            directory_metadata.uid(),
            directory_metadata.gid(),
        ) && unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } < 0
        {
            return Err(std::io::Error::last_os_error()).context("set managed file ownership");
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                temporary_c.as_ptr(),
                directory.as_raw_fd(),
                name_c.as_ptr(),
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error()).context("publish managed file");
        }
        Ok(())
    })();
    if write_result.is_err() {
        unsafe {
            libc::unlinkat(directory.as_raw_fd(), temporary_c.as_ptr(), 0);
        }
    }
    write_result
}

/// Remove the file `name` from `directory`; a missing file is not an error.
pub(super) fn remove_file_at(directory: &File, name: &str) -> Result<()> {
    let name_c = CString::new(name)?;
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name_c.as_ptr(), 0) } < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error).with_context(|| format!("remove managed file {name}"));
        }
    }
    Ok(())
}

/// The names of the regular files in `directory`.
pub(super) fn file_names_at(directory: &File) -> Result<Vec<String>> {
    let fd = unsafe { libc::dup(directory.as_raw_fd()) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("open managed directory listing");
    }
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(error).context("open managed directory listing");
    }
    // A directory stream shares its file offset with the handle it came from.
    unsafe { libc::rewinddir(stream) };
    let mut names = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        let entry = unsafe { &*entry };
        let name = unsafe { CStr::from_ptr(entry.d_name.as_ptr()) };
        let regular = match entry.d_type {
            libc::DT_REG => true,
            // Some filesystems do not report entry types in the listing.
            libc::DT_UNKNOWN => {
                let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
                unsafe {
                    libc::fstatat(
                        directory.as_raw_fd(),
                        name.as_ptr(),
                        status.as_mut_ptr(),
                        libc::AT_SYMLINK_NOFOLLOW,
                    ) == 0
                        && status.assume_init().st_mode & libc::S_IFMT == libc::S_IFREG
                }
            }
            _ => false,
        };
        if regular && let Ok(name) = name.to_str() {
            names.push(name.to_owned());
        }
    }
    unsafe { libc::closedir(stream) };
    names.sort();
    Ok(names)
}

fn managed_file_owner(
    worker_uid: u32,
    directory_uid: u32,
    directory_gid: u32,
) -> Option<(u32, u32)> {
    (worker_uid == 0).then_some((directory_uid, directory_gid))
}

pub(super) fn open_dir(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

fn open_dir_at(parent: &File, name: &CStr) -> std::io::Result<File> {
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Open the directory `name` in `parent`, creating it first if it is missing.
pub(super) fn ensure_dir_at(parent: &File, name: &str) -> Result<File> {
    let name_c = CString::new(name)?;
    let created = unsafe { libc::mkdirat(parent.as_raw_fd(), name_c.as_ptr(), 0o770) } == 0;
    if !created {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error).with_context(|| format!("create managed directory {name}"));
        }
    }
    let directory =
        open_dir_at(parent, &name_c).with_context(|| format!("open managed directory {name}"))?;
    if created {
        let parent_metadata = parent.metadata()?;
        if let Some((uid, gid)) = managed_file_owner(
            unsafe { libc::geteuid() },
            parent_metadata.uid(),
            parent_metadata.gid(),
        ) && unsafe { libc::fchown(directory.as_raw_fd(), uid, gid) } < 0
        {
            return Err(std::io::Error::last_os_error()).context("set managed directory ownership");
        }
    }
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn root_worker_publishes_managed_files_with_the_parent_directory_owner() {
        assert_eq!(managed_file_owner(0, 2613, 2613), Some((2613, 2613)));
        assert_eq!(managed_file_owner(501, 2613, 2613), None);
    }

    #[test]
    fn managed_directories_list_replace_and_remove_files_without_following_links() {
        let root = tempdir().unwrap();
        std::fs::create_dir(root.path().join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(root.path().join("elsewhere"), root.path().join("link"))
            .unwrap();
        let directory = open_dir(root.path()).unwrap();
        assert!(ensure_dir_at(&directory, "link").is_err());

        let managed = ensure_dir_at(&directory, "managed").unwrap();
        atomic_replace_at(&managed, "b.wasm", b"b", 0o644).unwrap();
        atomic_replace_at(&managed, "a.wasm", b"a", 0o644).unwrap();
        atomic_replace_at(&managed, "a.wasm", b"new", 0o644).unwrap();
        std::fs::create_dir(root.path().join("managed/nested")).unwrap();
        assert_eq!(file_names_at(&managed).unwrap(), ["a.wasm", "b.wasm"]);
        assert_eq!(
            std::fs::read(root.path().join("managed/a.wasm")).unwrap(),
            b"new"
        );

        remove_file_at(&managed, "b.wasm").unwrap();
        remove_file_at(&managed, "b.wasm").unwrap();
        assert_eq!(file_names_at(&managed).unwrap(), ["a.wasm"]);
    }
}
