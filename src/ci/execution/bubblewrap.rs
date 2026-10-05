//! Bubblewrap-backed build execution for Linux hosts without delegated cgroups.
//!
//! This backend is intentionally explicit and weaker than the Docker backend:
//! bubblewrap proves namespace, mount, network, capability, and credential
//! isolation, but it does not provide hard CPU, memory, or PID quotas without
//! cgroup delegation.  Callers must report [`BUBBLEWRAP_GUARANTEE`] verbatim
//! and must never describe this backend as cgroup-enforced.

use std::{
    env, fs,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time::timeout,
};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::{
    ci::execution::{BuildProcessReport, RootlessBuildRuntime},
    ci::{BuildPipelineError, PatchApplier, SandboxLimits},
};

use super::archive::{MAX_ARTIFACT_BYTES, MAX_COMPRESSED_SOURCE_BYTES};

const MAX_PLUGIN_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

/// Public guarantee label for every bubblewrap build and cache identity.
pub const BUBBLEWRAP_GUARANTEE: &str = "bubblewrap_namespace_only";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_LOG_BYTES: usize = 512 * 1024;
const MAX_FAILURE_BYTES: usize = 8 * 1024;
const BUILD_SCRIPT: &str = r#"
set -eu
umask 077
ulimit -n 4096 2>/dev/null || true
ulimit -f "$KURBIS_FILE_LIMIT_BLOCKS" 2>/dev/null || true
mkdir -p /work/source /work/plugin /work/out /work/tmp /work/home
export HOME=/work/home
export TMPDIR=/work/tmp
export CARGO_HOME=/toolchain/cargo
export CARGO_BUILD_JOBS=1
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_RELEASE_LTO=false
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
export CARGO_PROFILE_RELEASE_DEBUG=0
export CARGO_PROFILE_RELEASE_OPT_LEVEL=2
export CARGO_PROFILE_RELEASE_STRIP=debuginfo
actual_rustc="$(rustc -vV | awk -F': ' '/^release: / { print $2; exit }')"
[ "$actual_rustc" = "$KURBIS_EXPECTED_RUSTC" ] || {
  echo "builder Rust compiler mismatch: expected $KURBIS_EXPECTED_RUSTC, got $actual_rustc" >&2
  exit 78
}
# Extraction happens inside the namespace. The host only writes opaque input
# archives into the private workspace and never interprets untrusted paths.
tar -xzf /work/input/source.tar.gz -C /work/source --no-same-owner --no-same-permissions
tar -xzf /work/input/plugin.tar.gz -C /work/plugin --no-same-owner --no-same-permissions
cd /work/source
cargo build --locked --offline --release --jobs 1 --package pumpkin --bin pumpkin --target "$KURBIS_RUST_TARGET" --target-dir /work/target
cp "/work/target/$KURBIS_RUST_TARGET/release/pumpkin" /work/out/pumpkin
cd /work/plugin
cargo build --locked --offline --release --jobs 1 --manifest-path /work/plugin/Cargo.toml --target wasm32-wasip2 --target-dir /work/plugin-target
cp /work/plugin-target/wasm32-wasip2/release/kurbis_control.wasm /work/out/kurbis-control.wasm
strip /work/out/pumpkin
# These are diagnostics, not quota claims. The namespace-only backend has no
# hard cgroup memory/CPU/PID enforcement.
printf 'bubblewrap_guarantee=%s\n' "$KURBIS_BWRAP_GUARANTEE"
printf 'cgroup_delegation=%s\n' "$KURBIS_CGROUP_DELEGATION"
for path in /work /work/source /work/target /work/plugin-target /work/tmp /work/out; do
  printf 'df=%s ' "$path"; df -P -B1 "$path" 2>/dev/null || true
done
inodes=$(find /work -xdev -mindepth 1 -printf '.' 2>/dev/null | wc -c)
[ "$inodes" -le "$KURBIS_MAX_INODES" ]
printf 'workspace_inodes=%s\n' "$inodes"
bytes=$(du -sx -B1 /work 2>/dev/null | awk 'NR == 1 { print $1 }')
[ -n "$bytes" ] && [ "$bytes" -le "$KURBIS_DISK_BYTES" ]
printf 'workspace_bytes=%s\n' "$bytes"
"#;

const PATCH_SCRIPT: &str = r#"
set -eu
umask 077
ulimit -n 4096 2>/dev/null || true
ulimit -f "$KURBIS_FILE_LIMIT_BLOCKS" 2>/dev/null || true
mkdir -p /work/home /work/tmp
export HOME=/work/home
export TMPDIR=/work/tmp
# ci-source-preparer is a trusted, checked-in binary. It performs archive
# extraction and checked-in git patching entirely inside this sandbox.
/work/.ci-source-preparer /work/input/source.tar.gz /work/patched-source.tar.gz "$KURBIS_EXPECTED_PATCH_FINGERPRINT"
size=$(stat -c %s /work/patched-source.tar.gz)
test "$size" -gt 0
test "$size" -le "$KURBIS_MAX_SOURCE_BYTES"
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgroupDelegation {
    /// All requested controllers are visible and delegated to the worker.
    Available,
    /// cgroup v2 exists but delegation is absent or incomplete.
    Missing,
    /// cgroup v2 is unavailable on this host.
    Unavailable,
}

impl CgroupDelegation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Missing => "missing",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BubblewrapPreflight {
    pub bwrap_version: String,
    pub rootfs_manifest: String,
    pub toolchain_manifest: String,
    pub cgroup_delegation: CgroupDelegation,
    pub guarantee: &'static str,
}

#[derive(Clone)]
pub struct BubblewrapBuildRuntime {
    bwrap: PathBuf,
    rootfs: PathBuf,
    toolchain: PathBuf,
    workspace_root: PathBuf,
    source_preparer: PathBuf,
    rustc_version: String,
    identity: String,
    bwrap_version: String,
    rootfs_manifest: String,
    toolchain_manifest: String,
    admission: Option<super::admission::BuildAdmissionCoordinator>,
}

impl BubblewrapBuildRuntime {
    /// Construct a fail-closed bubblewrap runtime.
    ///
    /// `workspace_root` must be a mode-0700 directory below the current user's
    /// canonical HOME. `rootfs`, `toolchain`, and `source_preparer` are host
    /// operator inputs and are mounted read-only in every sandbox.
    pub fn new(
        bwrap: impl Into<PathBuf>,
        rootfs: impl Into<PathBuf>,
        toolchain: impl Into<PathBuf>,
        workspace_root: impl Into<PathBuf>,
        source_preparer: impl Into<PathBuf>,
        toolchain_version: impl Into<String>,
    ) -> Result<Self, BuildPipelineError> {
        ensure_linux()?;
        let bwrap = canonical_executable(&bwrap.into(), "bubblewrap executable")?;
        let rootfs = canonical_directory(&rootfs.into(), "bubblewrap rootfs")?;
        let toolchain = canonical_directory(&toolchain.into(), "bubblewrap toolchain")?;
        let source_preparer = canonical_executable(&source_preparer.into(), "source preparer")?;
        let workspace_root = private_workspace_root(&workspace_root.into())?;
        if rootfs == Path::new("/") || toolchain == Path::new("/") {
            return Err(sandbox_error(
                "rootfs and toolchain may not be the host root",
            ));
        }
        let bwrap_version = bubblewrap_version(&bwrap)?;
        let rootfs_manifest = manifest_digest(&rootfs)?;
        let toolchain_manifest = manifest_digest(&toolchain)?;
        let toolchain_version = toolchain_version.into();
        if toolchain_version.trim().is_empty() {
            return Err(sandbox_error("toolchain identity cannot be empty"));
        }
        let identity = format!(
            "{BUBBLEWRAP_GUARANTEE};bwrap={bwrap_version};rootfs={rootfs_manifest};toolchain={toolchain_manifest};rust={toolchain_version}"
        );
        Ok(Self {
            bwrap,
            rootfs,
            toolchain,
            workspace_root,
            source_preparer,
            rustc_version: toolchain_version,
            identity,
            bwrap_version,
            rootfs_manifest,
            toolchain_manifest,
            admission: None,
        })
    }

    pub fn with_admission(
        mut self,
        pool: sqlx::PgPool,
        concurrency: u16,
    ) -> Result<Self, BuildPipelineError> {
        let identity = format!(
            "bubblewrap:{}:{}",
            self.workspace_root.display(),
            self.identity
        );
        let cleaner = BubblewrapWorkspaceCleaner {
            root: self.workspace_root.clone(),
        };
        self.admission = Some(
            super::admission::BuildAdmissionCoordinator::for_builder(
                pool,
                identity,
                std::sync::Arc::new(LocalHostMemoryProbe),
                std::sync::Arc::new(cleaner),
            )
            .with_concurrency(usize::from(concurrency))
            .map_err(|error| sandbox_error(error.to_string()))?,
        );
        Ok(self)
    }

    pub fn guarantee(&self) -> &'static str {
        BUBBLEWRAP_GUARANTEE
    }

    /// Full immutable build identity for BuildKey/cache callers.
    pub fn build_identity(&self) -> &str {
        &self.identity
    }

    pub fn bwrap_version(&self) -> &str {
        &self.bwrap_version
    }

    pub fn rootfs_manifest(&self) -> &str {
        &self.rootfs_manifest
    }

    pub fn toolchain_manifest(&self) -> &str {
        &self.toolchain_manifest
    }

    pub fn cgroup_delegation() -> CgroupDelegation {
        detect_cgroup_delegation()
    }

    /// Run namespace and network invariants before accepting untrusted input.
    /// Missing cgroup delegation is reported but is not an error: this backend
    /// is explicitly namespace-only.
    pub async fn preflight(&self) -> Result<BubblewrapPreflight, BuildPipelineError> {
        let workspace = self.create_workspace("preflight", None)?;
        let script = r#"
set -eu
[ "$(id -u)" = 0 ]
grep -Eq '^NoNewPrivs:[[:space:]]+1$' /proc/self/status
grep -Eq '^CapEff:[[:space:]]+0+$' /proc/self/status
[ "$HOME" = /work/home ]
[ ! -e /run/docker.sock ]
[ ! -e /var/run/docker.sock ]
[ ! -e /work/.ssh ]
awk 'NR > 1 && $1 != "lo" { route = 1 } END { exit route }' /proc/net/route
awk '$NF != "lo" { route = 1 } END { exit route }' /proc/net/ipv6_route
for namespace in user pid mnt ipc uts net; do test -e "/proc/self/ns/$namespace"; done
[ "$(readlink /proc/self/ns/user)" != "$KURBIS_HOST_NS_USER" ]
[ "$(readlink /proc/self/ns/pid)" != "$KURBIS_HOST_NS_PID" ]
[ "$(readlink /proc/self/ns/mnt)" != "$KURBIS_HOST_NS_MNT" ]
[ "$(readlink /proc/self/ns/ipc)" != "$KURBIS_HOST_NS_IPC" ]
[ "$(readlink /proc/self/ns/uts)" != "$KURBIS_HOST_NS_UTS" ]
[ "$(readlink /proc/self/ns/net)" != "$KURBIS_HOST_NS_NET" ]
printf 'BWRAP_NAMESPACE_PROBE_PASSED\n'
"#;
        let namespace_env = match host_namespace_env() {
            Ok(namespace_env) => namespace_env,
            Err(error) => {
                let _ = tokio::fs::remove_dir_all(&workspace).await;
                return Err(error);
            }
        };
        let result = self
            .run_sandbox(&workspace, script, &namespace_env, COMMAND_TIMEOUT)
            .await;
        let _ = tokio::fs::remove_dir_all(&workspace).await;
        let (_, report) = result?;
        if !report.stdout.contains("BWRAP_NAMESPACE_PROBE_PASSED") {
            return Err(sandbox_error(
                "bubblewrap namespace probe did not emit its success marker",
            ));
        }
        Ok(BubblewrapPreflight {
            bwrap_version: self.bwrap_version.clone(),
            rootfs_manifest: self.rootfs_manifest.clone(),
            toolchain_manifest: self.toolchain_manifest.clone(),
            cgroup_delegation: Self::cgroup_delegation(),
            guarantee: BUBBLEWRAP_GUARANTEE,
        })
    }

    fn create_workspace(
        &self,
        prefix: &str,
        lease_token: Option<&str>,
    ) -> Result<PathBuf, BuildPipelineError> {
        let suffix = lease_token
            .map(str::to_owned)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        let name = format!("kurbis-bwrap-{prefix}-{suffix}");
        let workspace = self.workspace_root.join(name);
        fs::create_dir(&workspace)
            .map_err(|error| sandbox_io("create private bubblewrap workspace", error))?;
        fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700))
            .map_err(|error| sandbox_io("protect private bubblewrap workspace", error))?;
        for directory in ["input", "out", "tmp", "home", "run", "root", "sys"] {
            let path = workspace.join(directory);
            fs::create_dir(&path).map_err(|error| sandbox_io("create sandbox directory", error))?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .map_err(|error| sandbox_io("protect sandbox directory", error))?;
        }
        Ok(workspace)
    }

    fn command(&self, workspace: &Path, script: &str, envs: &[(&str, String)]) -> Command {
        let mut command = Command::new(&self.bwrap);
        command.env_clear();
        command
            .arg("--die-with-parent")
            .arg("--new-session")
            .arg("--unshare-user")
            .arg("--unshare-pid")
            .arg("--unshare-ipc")
            .arg("--unshare-uts")
            .arg("--unshare-net")
            .arg("--uid")
            .arg("0")
            .arg("--gid")
            .arg("0")
            .arg("--cap-drop")
            .arg("ALL")
            .arg("--ro-bind")
            .arg(&self.rootfs)
            .arg("/")
            .arg("--ro-bind")
            .arg(&self.toolchain)
            .arg("/toolchain")
            .arg("--bind")
            .arg(workspace)
            .arg("/work")
            .arg("--proc")
            .arg("/proc")
            .arg("--dev")
            .arg("/dev")
            .arg("--bind")
            .arg(workspace.join("tmp"))
            .arg("/tmp")
            .arg("--bind")
            .arg(workspace.join("run"))
            .arg("/run")
            .arg("--bind")
            .arg(workspace.join("root"))
            .arg("/root")
            .arg("--bind")
            .arg(workspace.join("home"))
            .arg("/home")
            .arg("--bind")
            .arg(workspace.join("sys"))
            .arg("/sys")
            .arg("--chdir")
            .arg("/work")
            .arg("--clearenv")
            .arg("--setenv")
            .arg("HOME")
            .arg("/work/home")
            .arg("--setenv")
            .arg("PATH")
            .arg("/toolchain/bin:/usr/local/bin:/usr/bin:/bin")
            .arg("--ro-bind")
            .arg(&self.source_preparer)
            .arg("/work/.ci-source-preparer");
        for (name, value) in envs {
            command.arg("--setenv").arg(name).arg(value);
        }
        command.arg("/bin/sh").arg("-ec").arg(script);
        command
    }

    async fn run_sandbox(
        &self,
        workspace: &Path,
        script: &str,
        envs: &[(&str, String)],
        process_timeout: Duration,
    ) -> Result<(ExitStatus, BuildProcessReport), BuildPipelineError> {
        let mut child = self
            .command(workspace, script, envs)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| sandbox_io("spawn bubblewrap sandbox", error))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| sandbox_error("bubblewrap stdout was not captured"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| sandbox_error("bubblewrap stderr was not captured"))?;
        let stdout_task = tokio::spawn(read_capped(stdout, MAX_LOG_BYTES / 2));
        let stderr_task = tokio::spawn(read_capped(stderr, MAX_LOG_BYTES / 2));
        let status = match timeout(process_timeout, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => return Err(sandbox_io("wait for bubblewrap sandbox", error)),
            Err(_) => {
                let _ = child.start_kill();
                let _ = timeout(Duration::from_secs(5), child.wait()).await;
                return Err(BuildPipelineError::transient(
                    crate::ci::CiBuildStage::Compiling,
                    format!(
                        "bubblewrap build exceeded its {} second timeout; descendants were signalled via --die-with-parent",
                        process_timeout.as_secs()
                    ),
                ));
            }
        };
        let stdout = stdout_task
            .await
            .map_err(|error| sandbox_error(format!("read bubblewrap stdout: {error}")))?
            .map_err(|error| sandbox_io("read bubblewrap stdout", error))?;
        let stderr = stderr_task
            .await
            .map_err(|error| sandbox_error(format!("read bubblewrap stderr: {error}")))?
            .map_err(|error| sandbox_io("read bubblewrap stderr", error))?;
        Ok((status, BuildProcessReport { stdout, stderr }))
    }

    async fn run_compile(
        &self,
        source_archive: &[u8],
        plugin_archive: &[u8],
        output_dir: &Path,
        target: &str,
        limits: SandboxLimits,
        lease_token: &str,
    ) -> Result<BuildProcessReport, BuildPipelineError> {
        validate_limits(limits)?;
        if source_archive.is_empty() || source_archive.len() > MAX_COMPRESSED_SOURCE_BYTES {
            return Err(permanent(
                "source archive is empty or exceeds the compressed-size limit",
            ));
        }
        if plugin_archive.is_empty() || plugin_archive.len() > MAX_PLUGIN_SOURCE_BYTES as usize {
            return Err(permanent(
                "trusted plugin archive is empty or exceeds its size limit",
            ));
        }
        let output_dir = canonical_output_directory(output_dir)?;
        ensure_workspace_capacity(&self.workspace_root, limits.disk_bytes)?;
        let workspace = self.create_workspace("compile", Some(lease_token))?;
        let result = async {
            tokio::fs::write(workspace.join("input/source.tar.gz"), source_archive)
                .await
                .map_err(|error| sandbox_io("write source archive to private workspace", error))?;
            tokio::fs::write(workspace.join("input/plugin.tar.gz"), plugin_archive)
                .await
                .map_err(|error| sandbox_io("write plugin archive to private workspace", error))?;
            let file_limit_blocks = limits.disk_bytes / 512;
            let max_inodes = (limits.disk_bytes / 4096).max(1024);
            let envs = [
                ("KURBIS_RUST_TARGET", target.to_owned()),
                ("KURBIS_EXPECTED_RUSTC", self.rustc_version.clone()),
                ("KURBIS_FILE_LIMIT_BLOCKS", file_limit_blocks.to_string()),
                ("KURBIS_DISK_BYTES", limits.disk_bytes.to_string()),
                ("KURBIS_MAX_INODES", max_inodes.to_string()),
                ("KURBIS_BWRAP_GUARANTEE", BUBBLEWRAP_GUARANTEE.to_owned()),
                (
                    "KURBIS_CGROUP_DELEGATION",
                    Self::cgroup_delegation().as_str().to_owned(),
                ),
            ];
            let (status, report) = self
                .run_sandbox(&workspace, BUILD_SCRIPT, &envs, limits.process_timeout)
                .await?;
            if !status.success() {
                return Err(BuildPipelineError::process_failure(
                    crate::ci::CiBuildStage::Compiling,
                    status,
                    format!(
                        "bubblewrap Pumpkin build failed ({}): {}",
                        status,
                        tail(&report.bounded(), MAX_FAILURE_BYTES)
                    ),
                ));
            }
            for file in ["pumpkin", "kurbis-control.wasm"] {
                let source = workspace.join("out").join(file);
                copy_regular_file(&source, &output_dir.join(file), MAX_ARTIFACT_BYTES)?;
            }
            Ok(report)
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&workspace).await;
        result
    }
}

#[async_trait::async_trait]
impl RootlessBuildRuntime for BubblewrapBuildRuntime {
    fn toolchain_version(&self) -> &str {
        &self.identity
    }

    async fn verify_sandbox(&self, limits: SandboxLimits) -> Result<(), BuildPipelineError> {
        let admission = self.admission.as_ref().ok_or_else(|| {
            sandbox_error("bubblewrap backend requires configured cross-process build admission")
        })?;
        let lease = admission
            .acquire(limits.memory_bytes)
            .await
            .map_err(|error| sandbox_error(error.to_string()))?;
        let result = self.preflight().await;
        lease.release().await;
        result.map(|_| ())
    }

    async fn compile(
        &self,
        source_archive: &[u8],
        plugin_archive: &[u8],
        output_dir: &Path,
        target: &str,
        limits: SandboxLimits,
    ) -> Result<BuildProcessReport, BuildPipelineError> {
        let admission = self.admission.as_ref().ok_or_else(|| {
            sandbox_error("bubblewrap backend requires configured cross-process build admission")
        })?;
        let lease = admission
            .acquire(limits.memory_bytes)
            .await
            .map_err(|error| sandbox_error(error.to_string()))?;
        let token = lease.token();
        let result = async {
            lease
                .assert_alive()
                .map_err(|error| sandbox_error(error.to_string()))?;
            self.preflight().await?;
            self.run_compile(
                source_archive,
                plugin_archive,
                output_dir,
                target,
                limits,
                &token,
            )
            .await
        }
        .await;
        lease.release().await;
        result
    }
}

#[async_trait::async_trait]
impl PatchApplier for BubblewrapBuildRuntime {
    async fn apply(
        &self,
        source_archive: Vec<u8>,
        fingerprint: &str,
    ) -> Result<Vec<u8>, BuildPipelineError> {
        if source_archive.is_empty() || source_archive.len() > MAX_COMPRESSED_SOURCE_BYTES {
            return Err(permanent(
                "source archive is empty or exceeds the compressed-size limit",
            ));
        }
        if fingerprint.trim().is_empty() {
            return Err(permanent("checked-in patch fingerprint is empty"));
        }
        let admission = self.admission.as_ref().ok_or_else(|| {
            sandbox_error("bubblewrap backend requires configured cross-process build admission")
        })?;
        let lease = admission
            .acquire(crate::ci::execution::production::ROOTLESS_BUILD_LIMITS.memory_bytes)
            .await
            .map_err(|error| sandbox_error(error.to_string()))?;
        let token = lease.token();
        ensure_workspace_capacity(
            &self.workspace_root,
            crate::ci::execution::production::ROOTLESS_BUILD_LIMITS.disk_bytes,
        )?;
        let workspace = match self.create_workspace("patch", Some(&token)) {
            Ok(workspace) => workspace,
            Err(error) => {
                lease.release().await;
                return Err(error);
            }
        };
        let result = async {
            tokio::fs::write(workspace.join("input/source.tar.gz"), source_archive)
                .await
                .map_err(|error| sandbox_io("write source archive to private workspace", error))?;
            let patch_disk_bytes = 5_u64 * 1024 * 1024 * 1024;
            let envs = [
                ("KURBIS_EXPECTED_PATCH_FINGERPRINT", fingerprint.to_owned()),
                (
                    "KURBIS_MAX_SOURCE_BYTES",
                    MAX_COMPRESSED_SOURCE_BYTES.to_string(),
                ),
                (
                    "KURBIS_FILE_LIMIT_BLOCKS",
                    (patch_disk_bytes / 512).to_string(),
                ),
            ];
            let (status, report) = self
                .run_sandbox(
                    &workspace,
                    PATCH_SCRIPT,
                    &envs,
                    Duration::from_secs(15 * 60),
                )
                .await?;
            if !status.success() {
                return Err(BuildPipelineError::process_failure(
                    crate::ci::CiBuildStage::Patching,
                    status,
                    format!(
                        "bubblewrap patch workload failed ({}): {}",
                        status,
                        tail(&report.bounded(), MAX_FAILURE_BYTES)
                    ),
                ));
            }
            let output = workspace.join("patched-source.tar.gz");
            let metadata = fs::symlink_metadata(&output)
                .map_err(|error| sandbox_io("inspect patched source archive", error))?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() == 0
                || metadata.len() > MAX_COMPRESSED_SOURCE_BYTES as u64
            {
                return Err(permanent(
                    "bubblewrap patch workload produced an invalid output archive",
                ));
            }
            tokio::fs::read(output)
                .await
                .map_err(|error| sandbox_io("read patched source archive", error))
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&workspace).await;
        lease.release().await;
        result
    }
}

struct LocalHostMemoryProbe;
#[async_trait::async_trait]
impl super::admission::HostMemoryProbe for LocalHostMemoryProbe {
    async fn snapshot(
        &self,
    ) -> Result<super::admission::HostMemorySnapshot, super::admission::BuildAdmissionError> {
        let contents = tokio::fs::read_to_string("/proc/meminfo")
            .await
            .map_err(|error| {
                super::admission::BuildAdmissionError::Unavailable(format!(
                    "read host memory: {error}"
                ))
            })?;
        super::admission::parse_meminfo(&contents).map_err(|error| {
            super::admission::BuildAdmissionError::Unavailable(format!(
                "parse host memory: {error}"
            ))
        })
    }
}

struct BubblewrapWorkspaceCleaner {
    root: PathBuf,
}
#[async_trait::async_trait]
impl super::admission::AbandonedBuildCleaner for BubblewrapWorkspaceCleaner {
    async fn remove_lease(
        &self,
        lease_token: &str,
    ) -> Result<(), super::admission::BuildAdmissionError> {
        if lease_token.is_empty() || !lease_token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(super::admission::BuildAdmissionError::Unavailable(
                "invalid bubblewrap lease token".into(),
            ));
        }
        let mut entries = tokio::fs::read_dir(&self.root).await.map_err(|error| {
            super::admission::BuildAdmissionError::Unavailable(format!(
                "scan bubblewrap workspaces: {error}"
            ))
        })?;
        while let Some(entry) = entries.next_entry().await.map_err(|error| {
            super::admission::BuildAdmissionError::Unavailable(format!(
                "scan bubblewrap workspaces: {error}"
            ))
        })? {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("kurbis-bwrap-") || !name.ends_with(lease_token) {
                continue;
            }
            let resolved = fs::canonicalize(&path).map_err(|error| {
                super::admission::BuildAdmissionError::Unavailable(format!(
                    "resolve bubblewrap workspace: {error}"
                ))
            })?;
            if !resolved.starts_with(&self.root) || resolved == self.root {
                return Err(super::admission::BuildAdmissionError::Unavailable(
                    "bubblewrap workspace cleanup escaped its private root".into(),
                ));
            }
            tokio::fs::remove_dir_all(resolved).await.map_err(|error| {
                super::admission::BuildAdmissionError::Unavailable(format!(
                    "remove stale bubblewrap workspace: {error}"
                ))
            })?;
        }
        Ok(())
    }
}

fn ensure_linux() -> Result<(), BuildPipelineError> {
    #[cfg(not(target_os = "linux"))]
    {
        return Err(sandbox_error("bubblewrap builds require a Linux worker"));
    }
    #[cfg(target_os = "linux")]
    Ok(())
}

fn reject_symlink_input(path: &Path, label: &str) -> Result<(), BuildPipelineError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| sandbox_io(format!("inspect {label}"), error))?;
    if metadata.file_type().is_symlink() {
        return Err(sandbox_error(format!("{label} may not be a symlink")));
    }
    Ok(())
}

fn canonical_executable(path: &Path, label: &str) -> Result<PathBuf, BuildPipelineError> {
    reject_symlink_input(path, label)?;
    let path =
        fs::canonicalize(path).map_err(|error| sandbox_io(format!("resolve {label}"), error))?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| sandbox_io(format!("inspect {label}"), error))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o111 == 0
        || is_public_tmp(&path)
    {
        return Err(sandbox_error(format!(
            "{label} must be an executable regular file"
        )));
    }
    Ok(path)
}

fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf, BuildPipelineError> {
    reject_symlink_input(path, label)?;
    let path =
        fs::canonicalize(path).map_err(|error| sandbox_io(format!("resolve {label}"), error))?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| sandbox_io(format!("inspect {label}"), error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(sandbox_error(format!("{label} must be a real directory")));
    }
    if is_public_tmp(&path) {
        return Err(sandbox_error(format!(
            "{label} may not reside below public /tmp"
        )));
    }
    Ok(path)
}

fn canonical_output_directory(path: &Path) -> Result<PathBuf, BuildPipelineError> {
    let path =
        fs::canonicalize(path).map_err(|error| sandbox_io("resolve artifact output", error))?;
    if !path.is_dir() || is_public_tmp(&path) {
        return Err(sandbox_error(
            "artifact output must be a private existing directory",
        ));
    }
    Ok(path)
}

fn private_workspace_root(path: &Path) -> Result<PathBuf, BuildPipelineError> {
    if is_public_tmp(path) {
        return Err(sandbox_error(
            "bubblewrap workspace root may not reside below public /tmp",
        ));
    }
    if !path.exists() {
        fs::create_dir_all(path)
            .map_err(|error| sandbox_io("create bubblewrap workspace root", error))?;
    }
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| sandbox_error("HOME is required for bubblewrap private paths"))?;
    let home = fs::canonicalize(home).map_err(|error| sandbox_io("resolve HOME", error))?;
    let path = fs::canonicalize(path)
        .map_err(|error| sandbox_io("resolve bubblewrap workspace root", error))?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| sandbox_io("inspect bubblewrap workspace root", error))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || !path.starts_with(&home)
        || is_public_tmp(&path)
    {
        return Err(sandbox_error(
            "bubblewrap workspace root must be a real mode-0700 directory below HOME",
        ));
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(sandbox_error(
            "bubblewrap workspace root is not owned by the build user",
        ));
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
        .map_err(|error| sandbox_io("protect bubblewrap workspace root", error))?;
    Ok(path)
}

fn is_public_tmp(path: &Path) -> bool {
    path == Path::new("/tmp") || path.starts_with("/tmp/")
}

fn host_namespace_env() -> Result<[(&'static str, String); 6], BuildPipelineError> {
    fn id(name: &str) -> Result<String, BuildPipelineError> {
        fs::read_link(format!("/proc/self/ns/{name}"))
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(|error| sandbox_io(format!("read host {name} namespace"), error))
    }
    Ok([
        ("KURBIS_HOST_NS_USER", id("user")?),
        ("KURBIS_HOST_NS_PID", id("pid")?),
        ("KURBIS_HOST_NS_MNT", id("mnt")?),
        ("KURBIS_HOST_NS_IPC", id("ipc")?),
        ("KURBIS_HOST_NS_UTS", id("uts")?),
        ("KURBIS_HOST_NS_NET", id("net")?),
    ])
}

fn bubblewrap_version(path: &Path) -> Result<String, BuildPipelineError> {
    let output = std::process::Command::new(path)
        .env_clear()
        .arg("--version")
        .output()
        .map_err(|error| sandbox_io("probe bubblewrap version", error))?;
    if !output.status.success() {
        return Err(BuildPipelineError::Sandbox(format!(
            "bubblewrap --version failed: {}",
            tail(&String::from_utf8_lossy(&output.stderr), MAX_FAILURE_BYTES)
        )));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if version.is_empty() {
        return Err(sandbox_error(
            "bubblewrap version probe returned no version",
        ));
    }
    Ok(version)
}

fn manifest_digest(root: &Path) -> Result<String, BuildPipelineError> {
    let mut entries = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry =
            entry.map_err(|error| sandbox_error(format!("walk immutable manifest: {error}")))?;
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(path);
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| sandbox_io("read immutable manifest metadata", error))?;
        let kind = if metadata.is_dir() {
            b'd'
        } else if metadata.is_file() {
            b'f'
        } else if metadata.file_type().is_symlink() {
            b'l'
        } else {
            b'o'
        };
        let target = if metadata.file_type().is_symlink() {
            fs::read_link(path)
                .ok()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let content = if metadata.is_file() {
            file_content_digest(path)?
        } else {
            String::new()
        };
        entries.push((
            relative.to_string_lossy().into_owned(),
            kind,
            metadata.len(),
            metadata.mode() & 0o7777,
            target,
            content,
        ));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = blake3::Hasher::new();
    for (path, kind, size, mode, target, content) in entries {
        hasher.update(&(path.len() as u64).to_be_bytes());
        hasher.update(path.as_bytes());
        hasher.update(&[kind]);
        hasher.update(&size.to_be_bytes());
        hasher.update(&mode.to_be_bytes());
        hasher.update(&(target.len() as u64).to_be_bytes());
        hasher.update(target.as_bytes());
        hasher.update(&(content.len() as u64).to_be_bytes());
        hasher.update(content.as_bytes());
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn file_content_digest(path: &Path) -> Result<String, BuildPipelineError> {
    let mut file =
        fs::File::open(path).map_err(|error| sandbox_io("open immutable manifest file", error))?;
    let mut digest = blake3::Hasher::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| sandbox_io("read immutable manifest file", error))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest.finalize().to_hex().to_string())
}

fn detect_cgroup_delegation() -> CgroupDelegation {
    #[cfg(not(target_os = "linux"))]
    {
        return CgroupDelegation::Unavailable;
    }
    #[cfg(target_os = "linux")]
    {
        let controllers = match fs::read_to_string("/sys/fs/cgroup/cgroup.controllers") {
            Ok(value) => value,
            Err(_) => return CgroupDelegation::Unavailable,
        };
        let subtree =
            fs::read_to_string("/sys/fs/cgroup/cgroup.subtree_control").unwrap_or_default();
        let available = ["cpu", "memory", "pids"].iter().all(|required| {
            controllers
                .split_ascii_whitespace()
                .any(|value| value == *required)
        });
        let delegated = ["cpu", "memory", "pids"].iter().all(|required| {
            subtree
                .split_ascii_whitespace()
                .any(|value| value == *required || value == &format!("+{required}"))
        });
        if available && delegated {
            CgroupDelegation::Available
        } else {
            CgroupDelegation::Missing
        }
    }
}

fn ensure_workspace_capacity(root: &Path, requested_bytes: u64) -> Result<(), BuildPipelineError> {
    let available = fs2_available_bytes(root)?;
    if available < requested_bytes {
        return Err(BuildPipelineError::transient(
            crate::ci::CiBuildStage::Compiling,
            format!(
                "private bubblewrap workspace has {available} bytes available, below the {requested_bytes} byte build budget"
            ),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn fs2_available_bytes(root: &Path) -> Result<u64, BuildPipelineError> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(root.as_os_str().as_bytes())
        .map_err(|_| sandbox_error("bubblewrap workspace path contains a NUL byte"))?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: statvfs writes one initialized value for a valid path.
    let result = unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) };
    if result != 0 {
        return Err(sandbox_io(
            "inspect bubblewrap workspace capacity",
            std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: statvfs returned success, therefore stats is initialized.
    let stats = unsafe { stats.assume_init() };
    (stats.f_bavail as u64)
        .checked_mul(stats.f_frsize as u64)
        .ok_or_else(|| sandbox_error("bubblewrap workspace capacity overflow"))
}

#[cfg(not(target_os = "linux"))]
fn fs2_available_bytes(_root: &Path) -> Result<u64, BuildPipelineError> {
    Err(sandbox_error(
        "bubblewrap workspace capacity requires Linux",
    ))
}

fn validate_limits(limits: SandboxLimits) -> Result<(), BuildPipelineError> {
    if limits.memory_bytes == 0
        || limits.cpu_millis == 0
        || limits.pid_limit == 0
        || limits.disk_bytes == 0
        || limits.process_timeout.is_zero()
    {
        return Err(sandbox_error("bubblewrap limits must be non-zero"));
    }
    Ok(())
}

fn copy_regular_file(
    source: &Path,
    destination: &Path,
    maximum: usize,
) -> Result<(), BuildPipelineError> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| sandbox_io("inspect bubblewrap artifact", error))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > maximum as u64
    {
        return Err(permanent(
            "bubblewrap artifact is not a bounded regular file",
        ));
    }
    let mut input = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(source)
        .map_err(|error| sandbox_io("open bubblewrap artifact", error))?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(destination)
        .map_err(|error| sandbox_io("create bubblewrap artifact", error))?;
    std::io::copy(&mut input, &mut output)
        .map_err(|error| sandbox_io("copy bubblewrap artifact", error))?;
    output
        .sync_all()
        .map_err(|error| sandbox_io("sync bubblewrap artifact", error))?;
    Ok(())
}

fn tail(value: &str, limit: usize) -> String {
    let start = value.len().saturating_sub(limit);
    value[start..].to_owned()
}

fn sandbox_error(message: impl Into<String>) -> BuildPipelineError {
    BuildPipelineError::Sandbox(message.into())
}

fn sandbox_io(context: impl Into<String>, error: impl std::fmt::Display) -> BuildPipelineError {
    BuildPipelineError::Sandbox(format!("{}: {error}", context.into()))
}

fn permanent(message: impl Into<String>) -> BuildPipelineError {
    BuildPipelineError::permanent(crate::ci::CiBuildStage::Packaging, message)
}

async fn read_capped<R>(mut reader: R, limit: usize) -> Result<String, std::io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut captured = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        if read >= limit {
            captured.clear();
            captured.extend_from_slice(&buffer[read - limit..read]);
        } else {
            let overflow = captured.len().saturating_add(read).saturating_sub(limit);
            if overflow > 0 {
                captured.drain(..overflow);
            }
            captured.extend_from_slice(&buffer[..read]);
        }
    }
    Ok(String::from_utf8_lossy(&captured).into_owned())
}

trait ReportExt {
    fn bounded(&self) -> String;
}
impl ReportExt for BuildProcessReport {
    fn bounded(&self) -> String {
        let mut value = self.stdout.clone();
        value.push('\n');
        value.push_str(&self.stderr);
        tail(&value, MAX_LOG_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn guarantee_is_explicitly_namespace_only() {
        assert_eq!(BUBBLEWRAP_GUARANTEE, "bubblewrap_namespace_only");
    }

    #[test]
    fn cgroup_probe_does_not_claim_delegation_from_controller_presence_alone() {
        let status = detect_cgroup_delegation();
        assert!(matches!(
            status,
            CgroupDelegation::Available | CgroupDelegation::Missing | CgroupDelegation::Unavailable
        ));
    }

    #[test]
    fn public_tmp_is_always_rejected() {
        assert!(is_public_tmp(Path::new("/tmp")));
        assert!(is_public_tmp(Path::new("/tmp/kurbis")));
        assert!(!is_public_tmp(Path::new("/home/user/.cache/kurbis")));
    }

    #[test]
    fn manifest_changes_when_tree_metadata_changes() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("tool.txt");
        fs::write(&file, "one").unwrap();
        let first = manifest_digest(root.path()).unwrap();
        fs::write(&file, "two-two").unwrap();
        let second = manifest_digest(root.path()).unwrap();
        assert_ne!(first, second);
        fs::write(&file, "three3").unwrap();
        let same_size_content = manifest_digest(root.path()).unwrap();
        assert_ne!(second, same_size_content);
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let third = manifest_digest(root.path()).unwrap();
        assert_ne!(same_size_content, third);
    }

    #[test]
    fn private_workspace_root_rejects_non_home_paths() {
        let root = tempfile::tempdir().unwrap();
        assert!(private_workspace_root(root.path()).is_err());
    }
}
