//! Trusted host-side preparation and isolated compilation for untrusted Pumpkin commits.

mod archive;
mod builder;
mod patcher;
mod production;
mod source;

pub use archive::{
    ArchiveError, MAX_ARTIFACT_BYTES, MAX_COMPRESSED_SOURCE_BYTES, MAX_EXPANDED_SOURCE_BYTES,
    MAX_SOURCE_ENTRIES, MAX_SOURCE_FILE_BYTES, pack_source_archive, unpack_source_archive,
};
pub use builder::{
    BuildProcessReport, DockerRootlessBuildRuntime, RootlessBuildRuntime, RustPumpkinBuilder,
    toolchain_identity,
};
pub use patcher::{CheckedInPumpkinPatcher, checked_in_patch_fingerprint};
pub use source::GitHubExactSource;

pub use production::{ROOTLESS_BUILD_LIMITS, docker_pipeline};
