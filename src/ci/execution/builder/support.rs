use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use tokio::process::Command;

use crate::ci::{BuildPipelineError, SandboxLimits};

pub(super) const MAX_PLUGIN_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

pub(super) fn default_build_limits() -> SandboxLimits {
    SandboxLimits {
        memory_bytes: 8 * 1024 * 1024 * 1024,
        cpu_millis: 2000,
        pid_limit: 512,
        disk_bytes: 5 * 1024 * 1024 * 1024,
        process_timeout: Duration::from_secs(60 * 60),
    }
}

pub fn toolchain_identity(image: &str, rustc_release: &str) -> anyhow::Result<String> {
    let (name, digest) = image
        .rsplit_once("@sha256:")
        .context("builder image must be pinned by sha256 digest")?;
    anyhow::ensure!(
        !name.is_empty()
            && digest.len() == 64
            && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            && !image.chars().any(char::is_whitespace),
        "builder image must be pinned by a valid sha256 digest"
    );
    anyhow::ensure!(
        valid_rustc_release(rustc_release),
        "invalid expected Rust release"
    );
    Ok(format!("rustc-{rustc_release}@sha256:{digest}"))
}

pub(super) fn rustc_release_from_identity(image: &str, identity: &str) -> anyhow::Result<String> {
    let (prefix, image_digest) = identity
        .split_once("@sha256:")
        .context("CI toolchain identity must include the builder image digest")?;
    let release = prefix
        .strip_prefix("rustc-")
        .context("CI toolchain identity must include an exact rustc release")?;
    anyhow::ensure!(
        valid_rustc_release(release),
        "invalid expected Rust release"
    );
    let (_, expected_digest) = image
        .rsplit_once("@sha256:")
        .context("builder image must be pinned by sha256 digest")?;
    anyhow::ensure!(
        image_digest.eq_ignore_ascii_case(expected_digest),
        "CI toolchain identity is for a different builder image"
    );
    Ok(release.to_owned())
}

pub(super) fn valid_rustc_release(release: &str) -> bool {
    let parts = release.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

pub(super) fn docker_command(docker_host: &str) -> Command {
    let mut command = Command::new("docker");
    command.args(["--host", docker_host]);
    command
}

pub(super) fn docker_platform(target: &str) -> Result<&'static str, BuildPipelineError> {
    match target {
        "x86_64-unknown-linux-gnu" => Ok("linux/amd64"),
        "aarch64-unknown-linux-gnu" => Ok("linux/arm64"),
        _ => Err(BuildPipelineError::Compile(format!(
            "unsupported native Linux target {target}"
        ))),
    }
}

pub(super) fn accessible_directory(
    path: &Path,
    label: &str,
) -> Result<PathBuf, BuildPipelineError> {
    let path = fs::canonicalize(path).map_err(|error| {
        BuildPipelineError::Sandbox(format!("invalid {label} directory: {error}"))
    })?;
    if !path.is_dir() {
        return Err(BuildPipelineError::Sandbox(format!(
            "{label} is not a directory"
        )));
    }
    Ok(path)
}

pub(super) fn validate_plugin_source(path: &Path) -> Result<(), BuildPipelineError> {
    let root = fs::canonicalize(path).map_err(|error| {
        BuildPipelineError::Sandbox(format!("invalid trusted plugin source: {error}"))
    })?;
    if !root.join("Cargo.toml").is_file()
        || !root.join("Cargo.lock").is_file()
        || !root.join("src").is_dir()
        || !root
            .join("Cargo.toml")
            .to_string_lossy()
            .ends_with("plugins/kurbis-control/Cargo.toml")
    {
        return Err(BuildPipelineError::Sandbox(
            "trusted plugin source must be the checked-in plugins/kurbis-control package".into(),
        ));
    }
    Ok(())
}

pub(super) fn plugin_build_context(source: &Path) -> Result<tempfile::TempDir, BuildPipelineError> {
    let source = fs::canonicalize(source).map_err(|error| {
        BuildPipelineError::Compile(format!("resolve trusted plugin source: {error}"))
    })?;
    validate_plugin_source(&source)?;
    let context = tempfile::Builder::new()
        .prefix("kurbis-ci-plugin-")
        .tempdir()
        .map_err(|error| {
            BuildPipelineError::Compile(format!("create plugin build context: {error}"))
        })?;
    let mut total = 0u64;
    for name in ["Cargo.toml", "Cargo.lock", "src", "build.rs"] {
        let from = source.join(name);
        if !from.exists() {
            continue;
        }
        copy_trusted_tree(&from, &context.path().join(name), &mut total)?;
    }
    if total > MAX_PLUGIN_SOURCE_BYTES {
        return Err(BuildPipelineError::Compile(
            "trusted plugin source exceeds its size limit".into(),
        ));
    }
    Ok(context)
}

pub(super) fn copy_trusted_tree(
    from: &Path,
    to: &Path,
    total: &mut u64,
) -> Result<(), BuildPipelineError> {
    let metadata = fs::symlink_metadata(from).map_err(|error| {
        BuildPipelineError::Compile(format!("inspect plugin build input: {error}"))
    })?;
    if metadata.file_type().is_symlink() {
        return Err(BuildPipelineError::Compile(
            "trusted plugin build input contains a symlink".into(),
        ));
    }
    if metadata.is_dir() {
        fs::create_dir_all(to).map_err(|error| {
            BuildPipelineError::Compile(format!("copy plugin directory: {error}"))
        })?;
        for entry in fs::read_dir(from).map_err(|error| {
            BuildPipelineError::Compile(format!("read plugin directory: {error}"))
        })? {
            let entry = entry.map_err(|error| BuildPipelineError::Compile(error.to_string()))?;
            copy_trusted_tree(&entry.path(), &to.join(entry.file_name()), total)?;
        }
    } else if metadata.is_file() {
        *total = total.checked_add(metadata.len()).ok_or_else(|| {
            BuildPipelineError::Compile("trusted plugin source size overflow".into())
        })?;
        if *total > MAX_PLUGIN_SOURCE_BYTES {
            return Err(BuildPipelineError::Compile(
                "trusted plugin source exceeds its size limit".into(),
            ));
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                BuildPipelineError::Compile(format!("create plugin directory: {error}"))
            })?;
        }
        fs::copy(from, to).map_err(|error| {
            BuildPipelineError::Compile(format!("copy trusted plugin file: {error}"))
        })?;
    } else {
        return Err(BuildPipelineError::Compile(
            "trusted plugin build input contains a special file".into(),
        ));
    }
    Ok(())
}

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Rootless Docker defaults tmpfs mounts to noexec, so opt in to exec only for
/// compilation workspaces that run source-controlled Cargo build scripts.
pub(super) const fn workspace_tmpfs_options(allow_workspace_exec: bool) -> &'static str {
    if allow_workspace_exec {
        "rw,exec,nosuid,nodev"
    } else {
        "rw,noexec,nosuid,nodev"
    }
}
