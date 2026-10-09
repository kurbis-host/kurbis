//! Trusted host-side preparation and isolated compilation for untrusted Pumpkin commits.

use std::{env, fs, io, path::PathBuf};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

/// Keep immutable builder identities within the database's 255-byte toolchain key limit.
/// The full identity remains the input to this digest, so changing any operator-owned
/// Bubblewrap executable, path, manifest, transport, or Rust release produces a new
/// cache/admission identity without storing unbounded path text in `ci_builds`.
pub(super) fn bounded_build_identity(
    label: &str,
    full_identity: &str,
    rustc_version: &str,
) -> String {
    let digest = Sha256::digest(full_identity.as_bytes());
    format!("{label};identity_sha256={digest:x};rust={rustc_version}")
}

/// Create a build-only temporary directory below the current user's private home.
/// Remote builders must never spill source, patches, or artifacts into a shared
/// public `/tmp` directory.
pub(super) fn private_tempdir(prefix: &str) -> io::Result<TempDir> {
    if prefix.is_empty()
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid private temporary directory prefix",
        ));
    }
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    let home = fs::canonicalize(home)?;
    let home_metadata = fs::metadata(&home)?;
    let root = home.join(".cache/kurbis/build-tmp");
    fs::create_dir_all(&root)?;
    let root = fs::canonicalize(root)?;
    if !root.starts_with(&home) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private build temporary root escaped HOME",
        ));
    }
    let metadata = fs::metadata(&root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private build temporary root is not a directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() }
            || home_metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private build temporary root is not owned by the current user",
            ));
        }
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    }
    let temporary = tempfile::Builder::new().prefix(prefix).tempdir_in(root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))?;
    }
    Ok(temporary)
}

mod admission;
mod archive;
mod bubblewrap;
mod builder;
mod cache;
mod patcher;
mod production;
mod remote_bubblewrap;
mod source;

pub(crate) use admission::parse_build_concurrency;

pub use archive::{
    ArchiveError, MAX_ARTIFACT_BYTES, MAX_COMPRESSED_SOURCE_BYTES, MAX_EXPANDED_SOURCE_BYTES,
    MAX_SOURCE_ENTRIES, MAX_SOURCE_FILE_BYTES, pack_source_archive, unpack_source_archive,
};
pub use bubblewrap::{
    BUBBLEWRAP_GUARANTEE, BUBBLEWRAP_WORKSPACE_INODES, BUBBLEWRAP_WORKSPACE_SIZE_BYTES,
    BubblewrapBuildRuntime, BubblewrapPreflight, CgroupDelegation,
};
pub use builder::{
    BuildProcessReport, DockerRootlessBuildRuntime, RootlessBuildRuntime, RustPumpkinBuilder,
    toolchain_identity,
};
pub use patcher::{CheckedInPumpkinPatcher, checked_in_patch_fingerprint};
pub use source::GitHubExactSource;

pub use production::{
    ROOTLESS_BUILD_LIMITS, bubblewrap_pipeline, docker_pipeline, remote_bubblewrap_pipeline,
};
pub use remote_bubblewrap::SshBubblewrapBuildRuntime;
