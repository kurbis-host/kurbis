use std::{path::Path, process::Command as HostCommand};

use include_dir::{Dir, include_dir};
use tokio::process::Command;

use super::archive::{
    ArchiveError, decode_source_bundle, merge_source_submodule, pack_source_archive,
    unpack_source_archive,
};
use crate::ci::BuildPipelineError;

const HOST_CONTROL_PATCH: &[u8] = include_bytes!("../../../runtime/pumpkin/host-control.patch");
const SNAPSHOT_BARRIER_PATCH: &[u8] =
    include_bytes!("../../../runtime/pumpkin/snapshot-barrier.patch");
const NETHERNET_PATCH: &[u8] = include_bytes!("../../../runtime/pumpkin/nethernet-gateway.patch");
const HOST_PATCHER_CAPABILITY: &[u8] = b"git-runtime-v1";
const HOST_CONTROL_RS: &[u8] = include_bytes!("../../../runtime/pumpkin/host_control.rs");
const PUMPKIN_REVISION: &[u8] = include_bytes!("../../../runtime/pumpkin/REVISION");
const PLUGIN_MANIFEST: &[u8] = include_bytes!("../../../plugins/kurbis-control/Cargo.toml");
const PLUGIN_LOCKFILE: &[u8] = include_bytes!("../../../plugins/kurbis-control/Cargo.lock");
/// The whole source tree, so a new plugin module can never be left out.
static PLUGIN_SOURCE: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/plugins/kurbis-control/src");

/// The exact build integration compiled into this binary: pinned Pumpkin
/// patches plus every trusted plugin source/lockfile input used in artifacts.
pub fn checked_in_patch_fingerprint() -> String {
    let mut hasher = blake3::Hasher::new();
    let pinned = [
        ("host-patcher-capability", HOST_PATCHER_CAPABILITY),
        ("pumpkin-revision", PUMPKIN_REVISION),
        ("host-control.patch", HOST_CONTROL_PATCH),
        ("snapshot-barrier.patch", SNAPSHOT_BARRIER_PATCH),
        ("nethernet-gateway.patch", NETHERNET_PATCH),
        ("host_control.rs", HOST_CONTROL_RS),
    ]
    .map(|(name, bytes)| (name.to_owned(), bytes));
    for (name, bytes) in pinned.into_iter().chain(trusted_plugin_inputs()) {
        hasher.update(&(name.len() as u64).to_be_bytes());
        hasher.update(name.as_bytes());
        hasher.update(&(bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    hasher.finalize().to_hex().to_string()
}

/// The plugin manifest, lockfile and every source file, sources by name.
fn trusted_plugin_inputs() -> Vec<(String, &'static [u8])> {
    fn collect(dir: &'static Dir<'static>, sources: &mut Vec<(String, &'static [u8])>) {
        sources.extend(dir.files().map(|file| {
            (
                format!("trusted-plugin/src/{}", file.path().display()),
                file.contents(),
            )
        }));
        for child in dir.dirs() {
            collect(child, sources);
        }
    }
    let mut sources = Vec::new();
    collect(&PLUGIN_SOURCE, &mut sources);
    sources.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    let mut inputs = vec![
        ("trusted-plugin/Cargo.toml".to_owned(), PLUGIN_MANIFEST),
        ("trusted-plugin/Cargo.lock".to_owned(), PLUGIN_LOCKFILE),
    ];
    inputs.extend(sources);
    inputs
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CheckedInPumpkinPatcher;

impl CheckedInPumpkinPatcher {
    /// Verify the host dependency used by the patcher before exposing CI.
    ///
    /// Remote Bubblewrap compilation still performs source patching in the
    /// control-plane process, so a runtime image without Git cannot be
    /// considered CI-capable even when the remote builder is healthy.
    pub fn verify_runtime_dependencies() -> Result<(), BuildPipelineError> {
        let output = HostCommand::new("git")
            .arg("--version")
            .output()
            .map_err(|error| {
                BuildPipelineError::Sandbox(format!(
                    "CI patcher requires git in the control-plane runtime: {error}"
                ))
            })?;
        if !output.status.success() {
            return Err(BuildPipelineError::Sandbox(format!(
                "CI patcher git preflight failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(())
    }

    async fn apply_patch(
        root: &Path,
        filename: &str,
        patch: &[u8],
        zero_context: bool,
    ) -> Result<(), BuildPipelineError> {
        let patch_path = root.join(filename);
        tokio::fs::write(&patch_path, patch)
            .await
            .map_err(|error| {
                BuildPipelineError::transient(
                    crate::ci::CiBuildStage::Patching,
                    format!("could not stage {filename}: {error}"),
                )
            })?;
        let mut check = git_command(root);
        check.arg("apply");
        if zero_context {
            check.arg("--unidiff-zero");
        }
        let check_output = check
            .arg("--check")
            .arg(&patch_path)
            .output()
            .await
            .map_err(|error| {
                BuildPipelineError::transient(
                    crate::ci::CiBuildStage::Patching,
                    format!("could not check {filename}: {error}"),
                )
            })?;
        if !check_output.status.success() {
            return Err(BuildPipelineError::process_failure(
                crate::ci::CiBuildStage::Patching,
                check_output.status,
                // Shown to whoever joins the preview: say what to do, since a
                // pull request branched before an upstream restructuring can
                // only be fixed by its author.
                format!(
                    "the Kurbis integration ({filename}) does not apply to this commit; a pull request needs to be rebased onto the current default branch: {}",
                    bounded_output(&check_output.stderr)
                ),
            ));
        }
        let mut apply = git_command(root);
        apply.arg("apply");
        if zero_context {
            apply.arg("--unidiff-zero");
        }
        let output = apply.arg(&patch_path).output().await.map_err(|error| {
            BuildPipelineError::transient(
                crate::ci::CiBuildStage::Patching,
                format!("could not apply {filename}: {error}"),
            )
        })?;
        tokio::fs::remove_file(patch_path).await.ok();
        if !output.status.success() {
            return Err(BuildPipelineError::process_failure(
                crate::ci::CiBuildStage::Patching,
                output.status,
                format!(
                    "{filename} application failed: {}",
                    bounded_output(&output.stderr)
                ),
            ));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::ci::PatchApplier for CheckedInPumpkinPatcher {
    async fn apply(
        &self,
        source_archive: Vec<u8>,
        fingerprint: &str,
    ) -> Result<Vec<u8>, BuildPipelineError> {
        if fingerprint != checked_in_patch_fingerprint() {
            return Err(BuildPipelineError::permanent(
                crate::ci::CiBuildStage::Patching,
                "build key does not match the host-compiled Pumpkin patch set",
            ));
        }
        let bundle = decode_source_bundle(&source_archive).map_err(archive_error)?;
        let mut workspace = unpack_source_archive(&bundle.primary).map_err(archive_error)?;
        for (path, archive) in &bundle.submodules {
            merge_source_submodule(&mut workspace, path, archive).map_err(archive_error)?;
        }
        let root = workspace.path();
        let init = git_command(root)
            .arg("init")
            .arg("--quiet")
            .output()
            .await
            .map_err(|error| {
                BuildPipelineError::transient(
                    crate::ci::CiBuildStage::Patching,
                    format!("git init failed: {error}"),
                )
            })?;
        if !init.status.success() {
            return Err(BuildPipelineError::process_failure(
                crate::ci::CiBuildStage::Patching,
                init.status,
                format!("git init failed: {}", bounded_output(&init.stderr)),
            ));
        }
        Self::apply_patch(root, "host-control.patch", HOST_CONTROL_PATCH, false).await?;
        Self::apply_patch(
            root,
            "snapshot-barrier.patch",
            SNAPSHOT_BARRIER_PATCH,
            false,
        )
        .await?;
        Self::apply_patch(root, "nethernet-gateway.patch", NETHERNET_PATCH, true).await?;

        let host_control = root.join("crates/pumpkin-core/src/net/host_control.rs");
        if let Some(parent) = host_control.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                BuildPipelineError::transient(
                    crate::ci::CiBuildStage::Patching,
                    format!("could not prepare host-control source: {error}"),
                )
            })?;
        }
        tokio::fs::write(host_control, HOST_CONTROL_RS)
            .await
            .map_err(|error| {
                BuildPipelineError::transient(
                    crate::ci::CiBuildStage::Patching,
                    format!("could not install host-control source: {error}"),
                )
            })?;
        pack_source_archive(root).map_err(archive_error)
    }
}

fn git_command(root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-C")
        .arg(root)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin")
        .env("HOME", "/nonexistent")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    command
}

fn bounded_output(bytes: &[u8]) -> String {
    const MAX_DIAGNOSTIC_BYTES: usize = 8 * 1024;
    String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(MAX_DIAGNOSTIC_BYTES)..])
        .trim()
        .to_owned()
}

fn archive_error(error: ArchiveError) -> BuildPipelineError {
    let detail = error.to_string();
    match error {
        ArchiveError::Invalid(_) | ArchiveError::Limit => {
            BuildPipelineError::permanent(crate::ci::CiBuildStage::Patching, detail)
        }
        ArchiveError::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::InvalidData
            ) =>
        {
            BuildPipelineError::permanent(crate::ci::CiBuildStage::Patching, detail)
        }
        ArchiveError::Io(_) => {
            BuildPipelineError::transient(crate::ci::CiBuildStage::Patching, detail)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::PatchApplier;

    #[test]
    fn patcher_runtime_dependency_preflight_succeeds_when_git_is_available() {
        CheckedInPumpkinPatcher::verify_runtime_dependencies().unwrap();
    }

    #[test]
    fn patch_fingerprint_is_derived_from_every_checked_in_input() {
        let fingerprint = checked_in_patch_fingerprint();
        assert_eq!(fingerprint.len(), 64);
        assert!(fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn patch_fingerprint_covers_every_trusted_plugin_source() {
        let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/kurbis-control");
        let mut on_disk = vec![
            "trusted-plugin/Cargo.toml".to_owned(),
            "trusted-plugin/Cargo.lock".to_owned(),
        ];
        let mut sources = walkdir::WalkDir::new(plugin.join("src"))
            .into_iter()
            .map(Result::unwrap)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| {
                let relative = entry.path().strip_prefix(&plugin).unwrap();
                format!("trusted-plugin/{}", relative.display())
            })
            .collect::<Vec<_>>();
        sources.sort();
        on_disk.extend(sources);
        let fingerprinted = trusted_plugin_inputs()
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(fingerprinted, on_disk);
        assert!(fingerprinted.contains(&"trusted-plugin/src/ai/mod.rs".to_owned()));
    }

    #[test]
    fn invalid_patch_source_is_permanent_but_local_storage_errors_are_transient() {
        let invalid = archive_error(ArchiveError::Invalid("malformed archive".into()));
        assert_eq!(
            invalid.failure_class(),
            crate::ci::CiBuildFailureClass::Permanent
        );
        let storage = archive_error(ArchiveError::Io(std::io::Error::from(
            std::io::ErrorKind::Other,
        )));
        assert_eq!(
            storage.failure_class(),
            crate::ci::CiBuildFailureClass::Transient
        );
    }

    #[tokio::test]
    async fn patcher_fails_closed_when_the_source_does_not_match_the_patch_set() {
        let error = CheckedInPumpkinPatcher
            .apply(Vec::new(), "not-the-patch-fingerprint")
            .await
            .unwrap_err();
        assert_eq!(
            error.failure_class(),
            crate::ci::CiBuildFailureClass::Permanent
        );
    }
}
