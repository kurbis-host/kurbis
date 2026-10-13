//! Remote Bubblewrap build transport.
//!
//! The control plane stays on the runtime worker host and sends one framed job
//! over a pinned SSH account to a build-only Linux host. The remote helper is
//! checked into this repository, validates private HOME staging, and returns
//! bounded artifacts with SHA-256 metadata before the coordinator packages and
//! re-hashes them into the normal CI artifact cache.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    time::timeout,
};

use super::{
    admission::{AbandonedBuildCleaner, BuildAdmissionCoordinator, HostMemoryProbe},
    archive::{MAX_ARTIFACT_BYTES, MAX_COMPRESSED_SOURCE_BYTES},
    builder::{BuildProcessReport, RootlessBuildRuntime},
    profile::BuildResourceProfile,
};
use crate::ci::{BuildPipelineError, SandboxLimits};

const MAX_PLUGIN_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_RESPONSE_LINE: usize = 256 * 1024;
const CAPABILITY_LEASE_TTL: Duration = Duration::from_secs(2 * 60 * 60);
const REMOTE_PROTOCOL_VERSION: u8 = 3;

/// Evidence returned by the remote helper before CI is marked ready.
///
/// These values are deliberately measured on the builder, not copied from
/// deployment configuration.  The capability fingerprint is therefore tied
/// to the actual account, immutable inputs, workspace mount, and toolchain.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RemoteBuilderCapability {
    pub protocol_version: u8,
    pub host_uid: u32,
    pub host_gid: u32,
    pub sandbox_uid: u32,
    pub sandbox_gid: u32,
    pub bwrap_hash: String,
    pub rootfs_hash: String,
    pub toolchain_hash: String,
    pub source_preparer_hash: String,
    pub rustc_version: String,
    pub workspace_bytes: u64,
    pub workspace_inodes: u64,
    pub memory_total_bytes: u64,
    pub memory_available_bytes: u64,
    pub cgroup_mode: String,
    pub smoke_build: bool,
    pub artifact_transfer: bool,
    pub artifact_execution: bool,
}

impl RemoteBuilderCapability {
    fn validate(&self, profile: BuildResourceProfile, expected_rustc: &str) -> Result<(), String> {
        if self.protocol_version != REMOTE_PROTOCOL_VERSION {
            return Err(format!(
                "unsupported remote capability protocol {}",
                self.protocol_version
            ));
        }
        if self.host_uid == 0 || self.host_gid == 0 {
            return Err("remote builder must use a non-root host UID/GID".into());
        }
        if self.sandbox_uid != 0 || self.sandbox_gid != 0 {
            return Err("remote Bubblewrap sandbox did not enter UID/GID 0 namespace".into());
        }
        for (name, value) in [
            ("Bubblewrap", &self.bwrap_hash),
            ("rootfs", &self.rootfs_hash),
            ("toolchain", &self.toolchain_hash),
            ("source preparer", &self.source_preparer_hash),
        ] {
            if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(format!("remote {name} hash is not a SHA-256 digest"));
            }
        }
        if self.rustc_version != expected_rustc {
            return Err(format!(
                "remote rustc mismatch: expected {expected_rustc}, got {}",
                self.rustc_version
            ));
        }
        if self.workspace_bytes != profile.limits.disk_bytes
            || self.workspace_inodes != profile.workspace_inodes
        {
            return Err("remote workspace does not match the registered resource profile".into());
        }
        if !matches!(
            self.cgroup_mode.as_str(),
            "delegated" | "available_not_delegated" | "unavailable"
        ) {
            return Err("remote cgroup capability has an unknown mode".into());
        }
        if !self.smoke_build || !self.artifact_transfer || !self.artifact_execution {
            return Err(
                "remote builder smoke build, artifact transfer, or artifact execution failed"
                    .into(),
            );
        }
        profile.validate_measured_capacity(self.memory_available_bytes, self.memory_total_bytes)
    }

    fn fingerprint(&self) -> String {
        let encoded = serde_json::to_vec(self).expect("remote capability is serializable");
        blake3::hash(&encoded).to_hex().to_string()
    }
}

#[derive(Debug, Clone)]
struct RemoteBuilderCapabilityLease {
    capability: RemoteBuilderCapability,
    fingerprint: String,
    expires_at: Instant,
}

impl RemoteBuilderCapabilityLease {
    fn is_valid(&self, profile: BuildResourceProfile, expected_rustc: &str) -> bool {
        Instant::now() < self.expires_at
            && self.capability.validate(profile, expected_rustc).is_ok()
            && self.fingerprint == self.capability.fingerprint()
    }
}

#[derive(Clone)]
pub struct SshBubblewrapBuildRuntime {
    target: String,
    runner: PathBuf,
    bwrap: String,
    rootfs: String,
    toolchain: String,
    workspace_root: String,
    source_preparer: String,
    patch_fingerprint: String,
    compiler_cache_root: String,
    compiler_cache_size: u64,
    rustc_version: String,
    identity: String,
    profile: BuildResourceProfile,
    capability: Arc<RwLock<Option<RemoteBuilderCapabilityLease>>>,
    admission: BuildAdmissionCoordinator,
}

impl SshBubblewrapBuildRuntime {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        target: impl Into<String>,
        runner: impl Into<PathBuf>,
        bwrap: impl Into<String>,
        rootfs: impl Into<String>,
        toolchain: impl Into<String>,
        workspace_root: impl Into<String>,
        source_preparer: impl Into<String>,
        rustc_version: impl Into<String>,
        concurrency: u16,
    ) -> Result<Self, BuildPipelineError> {
        let mut profile = BuildResourceProfile::default_rootless();
        profile.concurrency = concurrency;
        profile.validate().map_err(BuildPipelineError::Sandbox)?;
        let target = target.into();
        if target.trim().is_empty()
            || target.len() > 255
            || target.bytes().any(|b| b.is_ascii_whitespace())
        {
            return Err(BuildPipelineError::Sandbox(
                "CI_BUILDER_SSH_TARGET must be a single SSH target".into(),
            ));
        }
        let runner = runner.into();
        crate::config::validate_ci_remote_path(
            &runner.to_string_lossy(),
            "remote Bubblewrap runner",
        )
        .map_err(|error| BuildPipelineError::Sandbox(error.to_string()))?;
        let rustc_version = rustc_version.into();
        if rustc_version.trim().is_empty() {
            return Err(BuildPipelineError::Sandbox(
                "remote Bubblewrap Rust identity cannot be empty".into(),
            ));
        }
        let bwrap = bwrap.into();
        let rootfs = rootfs.into();
        let toolchain = toolchain.into();
        let workspace_root = workspace_root.into();
        let source_preparer = source_preparer.into();
        // The source preparer is an operator-mounted executable. Carry the
        // host-compiled fingerprint into every remote preflight so a stale
        // preparer cannot silently diverge from the patcher used for admission.
        let patch_fingerprint = crate::ci::checked_in_patch_fingerprint();
        let compiler_cache_root = std::path::Path::new(&workspace_root)
            .parent()
            .ok_or_else(|| {
                BuildPipelineError::Sandbox("remote Bubblewrap workspace has no parent".into())
            })?
            .join("compiler-cache")
            .display()
            .to_string();
        let compiler_cache_size = super::cache::configured_cache_size().map_err(|error| {
            BuildPipelineError::Sandbox(format!("configure compiler cache size: {error}"))
        })?;
        for (name, value) in [
            ("bwrap", &bwrap),
            ("rootfs", &rootfs),
            ("toolchain", &toolchain),
            ("workspace", &workspace_root),
            ("source preparer", &source_preparer),
        ] {
            crate::config::validate_ci_remote_path(value, &format!("remote Bubblewrap {name}"))
                .map_err(|error| BuildPipelineError::Sandbox(error.to_string()))?;
        }
        let full_identity = format!(
            "bubblewrap_namespace_only;transport=ssh;target={target};runner={};bwrap={bwrap};rootfs={rootfs};toolchain={toolchain};workspace={workspace_root};source_preparer={source_preparer};patch={patch_fingerprint};rust={rustc_version}",
            runner.display()
        );
        let identity = super::bounded_build_identity(
            "bubblewrap_namespace_only;transport=ssh",
            &full_identity,
            &rustc_version,
        );
        let admission_identity = format!("bubblewrap_ssh:{full_identity}");
        let admission = BuildAdmissionCoordinator::for_builder(
            pool,
            admission_identity,
            Arc::new(RemoteHostMemoryProbe {
                target: target.clone(),
            }),
            Arc::new(RemoteWorkspaceCleaner {
                target: target.clone(),
                workspace_root: workspace_root.clone(),
            }),
        )
        .with_concurrency(usize::from(concurrency))
        .map_err(|error| BuildPipelineError::Sandbox(error.to_string()))?;
        Ok(Self {
            target,
            runner,
            bwrap,
            rootfs,
            toolchain,
            workspace_root,
            source_preparer,
            patch_fingerprint,
            compiler_cache_root,
            compiler_cache_size,
            rustc_version,
            identity,
            profile,
            capability: Arc::new(RwLock::new(None)),
            admission,
        })
    }

    pub fn build_identity(&self) -> &str {
        &self.identity
    }

    pub fn capability(&self) -> Option<RemoteBuilderCapability> {
        self.capability
            .read()
            .ok()
            .and_then(|lease| lease.as_ref().map(|lease| lease.capability.clone()))
    }

    fn assert_capability_lease(&self) -> Result<(), BuildPipelineError> {
        let lease = self.capability.read().map_err(|_| {
            BuildPipelineError::Sandbox(
                "remote Bubblewrap capability lease lock was poisoned".into(),
            )
        })?;
        match lease.as_ref() {
            Some(lease) if lease.is_valid(self.profile, &self.rustc_version) => Ok(()),
            Some(_) => Err(BuildPipelineError::Sandbox(
                "remote Bubblewrap capability lease expired or no longer matches the profile"
                    .into(),
            )),
            None => Err(BuildPipelineError::Sandbox(
                "remote Bubblewrap capability has not been registered; CI is not ready".into(),
            )),
        }
    }

    /// Register the remote builder only after its measured preflight and
    /// smoke-build contract succeeds. Production startup calls this before it
    /// publishes a CI coordinator, so a broken builder cannot look ready.
    pub async fn register_capability(&self) -> Result<RemoteBuilderCapability, BuildPipelineError> {
        let snapshot = probe_remote_memory(&self.target).await?;
        self.profile
            .validate_measured_capacity(snapshot.available_bytes, snapshot.total_bytes)
            .map_err(BuildPipelineError::Sandbox)?;
        let lease = self
            .admission
            .acquire(self.profile.memory_bytes())
            .await
            .map_err(|error| BuildPipelineError::Sandbox(error.to_string()))?;
        let token = lease.token();
        let result = self
            .run_job(
                "preflight",
                &token,
                &lease,
                &[],
                &[],
                "x86_64-unknown-linux-gnu",
                None,
                self.profile.limits,
                None,
            )
            .await;
        lease.release().await;
        result?;
        self.capability().ok_or_else(|| {
            BuildPipelineError::Sandbox(
                "remote preflight completed without a capability lease".into(),
            )
        })
    }

    fn ssh_command(&self) -> Command {
        let mut command = Command::new("ssh");
        command.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ConnectTimeout=15",
            "-o",
            "ServerAliveInterval=30",
            "-o",
            "ServerAliveCountMax=2",
            "-T",
            "--",
            &self.target,
        ]);
        command
    }

    fn command(&self) -> Command {
        let mut command = self.ssh_command();
        // The runner keeps the host network for its dependency-fetch phase and
        // unshares it itself for every phase that executes fetched code.
        command
            .args(["unshare", "-Urm", "python3"])
            .arg(&self.runner);
        command
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_job(
        &self,
        mode: &str,
        job_id: &str,
        lease: &super::admission::BuildAdmissionLease,
        source: &[u8],
        plugin: &[u8],
        target: &str,
        source_revision: Option<&str>,
        limits: SandboxLimits,
        output_dir: Option<&Path>,
    ) -> Result<BuildProcessReport, BuildPipelineError> {
        if source.len() > MAX_COMPRESSED_SOURCE_BYTES || plugin.len() > MAX_PLUGIN_SOURCE_BYTES {
            return Err(BuildPipelineError::permanent(
                crate::ci::CiBuildStage::Compiling,
                "remote Bubblewrap input exceeds its bounded size",
            ));
        }
        if !target
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err(BuildPipelineError::permanent(
                crate::ci::CiBuildStage::Compiling,
                "remote Rust target is invalid",
            ));
        }
        lease
            .assert_alive()
            .map_err(|error| BuildPipelineError::Sandbox(error.to_string()))?;
        let header = JobHeader {
            version: REMOTE_PROTOCOL_VERSION,
            mode: mode.to_owned(),
            job_id: job_id.to_owned(),
            target: target.to_owned(),
            rustc: self.rustc_version.clone(),
            workspace_root: self.workspace_root.clone(),
            bwrap: self.bwrap.clone(),
            rootfs: self.rootfs.clone(),
            toolchain: self.toolchain.clone(),
            source_preparer: self.source_preparer.clone(),
            patch_fingerprint: self.patch_fingerprint.clone(),
            source_revision: source_revision.map(str::to_owned),
            cache_root: self.compiler_cache_root.clone(),
            cache_max_bytes: self.compiler_cache_size,
            source_len: source.len(),
            plugin_len: plugin.len(),
            timeout_seconds: limits.process_timeout.as_secs().clamp(1, 90 * 60),
            profile_memory_bytes: self.profile.limits.memory_bytes,
            profile_workspace_bytes: self.profile.limits.disk_bytes,
            profile_workspace_inodes: self.profile.workspace_inodes,
            profile_concurrency: self.profile.concurrency,
            capability_fingerprint: self
                .capability
                .read()
                .ok()
                .and_then(|lease| lease.as_ref().map(|lease| lease.fingerprint.clone())),
        };
        let mut child = self
            .command()
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                BuildPipelineError::transient(
                    crate::ci::CiBuildStage::Compiling,
                    format!("spawn remote Bubblewrap SSH job: {e}"),
                )
            })?;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            BuildPipelineError::transient(
                crate::ci::CiBuildStage::Compiling,
                "remote Bubblewrap SSH stdin unavailable",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            BuildPipelineError::transient(
                crate::ci::CiBuildStage::Compiling,
                "remote Bubblewrap SSH stdout unavailable",
            )
        })?;
        let mut stderr = child.stderr.take().ok_or_else(|| {
            BuildPipelineError::transient(
                crate::ci::CiBuildStage::Compiling,
                "remote Bubblewrap SSH stderr unavailable",
            )
        })?;
        let frame = serde_json::to_vec(&header).map_err(|e| {
            BuildPipelineError::permanent(crate::ci::CiBuildStage::Compiling, e.to_string())
        })?;
        let writer = async {
            stdin.write_all(&frame).await?;
            stdin.write_all(b"\n").await?;
            stdin.write_all(source).await?;
            stdin.write_all(plugin).await?;
            stdin.shutdown().await
        };
        timeout(Duration::from_secs(180), writer)
            .await
            .map_err(|_| {
                BuildPipelineError::transient(
                    crate::ci::CiBuildStage::Compiling,
                    "remote Bubblewrap input transfer timed out",
                )
            })?
            .map_err(|e| {
                BuildPipelineError::transient(
                    crate::ci::CiBuildStage::Compiling,
                    format!("remote Bubblewrap input transfer failed: {e}"),
                )
            })?;
        let mut reader = BufReader::new(stdout);
        let mut line = Vec::with_capacity(4096);
        let read_result = timeout(limits.process_timeout + Duration::from_secs(180), async {
            reader.read_until(b'\n', &mut line).await
        })
        .await
        .map_err(|_| {
            BuildPipelineError::transient(
                crate::ci::CiBuildStage::Compiling,
                "remote Bubblewrap SSH job timed out",
            )
        })?
        .map_err(|e| {
            BuildPipelineError::transient(
                crate::ci::CiBuildStage::Compiling,
                format!("read remote Bubblewrap result: {e}"),
            )
        })?;
        if read_result == 0 || line.len() > MAX_RESPONSE_LINE {
            let _ = child.kill().await;
            return Err(BuildPipelineError::transient(
                crate::ci::CiBuildStage::Compiling,
                "remote Bubblewrap result header is missing or oversized",
            ));
        }
        let mut result: JobResult = serde_json::from_slice(&line).map_err(|e| {
            BuildPipelineError::transient(
                crate::ci::CiBuildStage::Compiling,
                format!("decode remote Bubblewrap result: {e}"),
            )
        })?;
        if mode == "preflight" {
            let mut capability = result.capability.take().ok_or_else(|| {
                BuildPipelineError::permanent(
                    crate::ci::CiBuildStage::Compiling,
                    "remote Bubblewrap preflight did not return a capability report",
                )
            })?;
            let (host_uid, host_gid) = probe_remote_identity(&self.target).await?;
            capability.host_uid = host_uid;
            capability.host_gid = host_gid;
            capability
                .validate(self.profile, &self.rustc_version)
                .map_err(|error| {
                    BuildPipelineError::permanent(
                        crate::ci::CiBuildStage::Compiling,
                        format!("remote Bubblewrap capability rejected: {error}"),
                    )
                })?;
            let fingerprint = capability.fingerprint();
            if let Ok(mut slot) = self.capability.write() {
                *slot = Some(RemoteBuilderCapabilityLease {
                    capability,
                    fingerprint,
                    expires_at: Instant::now() + CAPABILITY_LEASE_TTL,
                });
            } else {
                return Err(BuildPipelineError::Sandbox(
                    "remote Bubblewrap capability lease lock was poisoned".into(),
                ));
            }
        }
        lease
            .assert_alive()
            .map_err(|error| BuildPipelineError::Sandbox(error.to_string()))?;
        let mut build_report = BuildProcessReport {
            stdout: result.stdout,
            stderr: result.stderr,
        };
        // The runner returns no output frames for a failed compile. Classify
        // that process result before validating the success-only artifact set;
        // otherwise an ordinary transient remote/compiler failure would be
        // misreported as a permanent packaging error and would skip retries.
        if mode != "preflight" && !result.ok {
            let mut remote_stderr = Vec::new();
            stderr.read_to_end(&mut remote_stderr).await.ok();
            let _status = child.wait().await.map_err(|e| {
                BuildPipelineError::transient(
                    crate::ci::CiBuildStage::Compiling,
                    format!("wait for remote Bubblewrap SSH job: {e}"),
                )
            })?;
            build_report
                .stderr
                .push_str(&String::from_utf8_lossy(&remote_stderr));
            return Err(BuildPipelineError::process_failure_code(
                crate::ci::CiBuildStage::Compiling,
                Some(result._status),
                format!("remote Bubblewrap build failed: {}", build_report.stderr),
            ));
        }
        let mut artifacts = result.artifacts;
        if mode == "preflight" {
            if !artifacts.is_empty() {
                let _ = child.kill().await;
                return Err(BuildPipelineError::permanent(
                    crate::ci::CiBuildStage::Packaging,
                    "remote Bubblewrap preflight returned artifacts",
                ));
            }
        } else {
            let Some(output_dir) = output_dir else {
                let _ = child.kill().await;
                return Err(BuildPipelineError::permanent(
                    crate::ci::CiBuildStage::Packaging,
                    "remote Bubblewrap compile did not provide an output directory",
                ));
            };
            let mut names = artifacts
                .iter()
                .map(|artifact| artifact.name.as_str())
                .collect::<Vec<_>>();
            names.sort_unstable();
            if names != ["kurbis-control.wasm", "pumpkin"] {
                let _ = child.kill().await;
                return Err(BuildPipelineError::permanent(
                    crate::ci::CiBuildStage::Packaging,
                    "remote Bubblewrap compile returned an unexpected artifact set",
                ));
            }
            for artifact in artifacts.drain(..) {
                let destination = output_dir.join(&artifact.name);
                if artifact.size == 0
                    || artifact.size > MAX_ARTIFACT_BYTES
                    || !matches!(artifact.name.as_str(), "pumpkin" | "kurbis-control.wasm")
                {
                    return Err(BuildPipelineError::permanent(
                        crate::ci::CiBuildStage::Packaging,
                        "remote Bubblewrap artifact metadata is invalid",
                    ));
                }
                let mut file = tokio::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&destination)
                    .await
                    .map_err(|e| {
                        BuildPipelineError::transient(
                            crate::ci::CiBuildStage::Packaging,
                            format!("stage remote artifact: {e}"),
                        )
                    })?;
                let mut hasher = Sha256::new();
                let mut limited = (&mut reader).take(artifact.size as u64);
                let copied = tokio::io::copy(&mut limited, &mut file)
                    .await
                    .map_err(|e| {
                        BuildPipelineError::transient(
                            crate::ci::CiBuildStage::Packaging,
                            format!("stream remote artifact: {e}"),
                        )
                    })?;
                if copied != artifact.size as u64 {
                    return Err(BuildPipelineError::transient(
                        crate::ci::CiBuildStage::Packaging,
                        "remote Bubblewrap artifact stream was truncated",
                    ));
                }
                file.flush().await.map_err(|e| {
                    BuildPipelineError::transient(
                        crate::ci::CiBuildStage::Packaging,
                        format!("flush remote artifact: {e}"),
                    )
                })?;
                drop(file);
                let bytes = tokio::fs::read(&destination).await.map_err(|e| {
                    BuildPipelineError::transient(
                        crate::ci::CiBuildStage::Packaging,
                        format!("verify remote artifact: {e}"),
                    )
                })?;
                hasher.update(&bytes);
                let digest = format!("{:x}", hasher.finalize());
                if digest != artifact.sha256 {
                    let _ = tokio::fs::remove_file(&destination).await;
                    return Err(BuildPipelineError::permanent(
                        crate::ci::CiBuildStage::Packaging,
                        format!(
                            "remote Bubblewrap artifact {} failed SHA-256 verification",
                            artifact.name
                        ),
                    ));
                }
            }
        }
        let mut trailing_stdout = Vec::new();
        reader
            .read_to_end(&mut trailing_stdout)
            .await
            .map_err(|e| {
                BuildPipelineError::transient(
                    crate::ci::CiBuildStage::Packaging,
                    format!("read remote Bubblewrap trailing output: {e}"),
                )
            })?;
        if !trailing_stdout.is_empty() {
            return Err(BuildPipelineError::permanent(
                crate::ci::CiBuildStage::Packaging,
                "remote Bubblewrap result contained trailing output after its framed payload",
            ));
        }
        let mut trailing = Vec::new();
        reader.read_to_end(&mut trailing).await.map_err(|error| {
            BuildPipelineError::transient(
                crate::ci::CiBuildStage::Packaging,
                format!("read remote Bubblewrap trailing output: {error}"),
            )
        })?;
        if !trailing.is_empty() {
            return Err(BuildPipelineError::permanent(
                crate::ci::CiBuildStage::Packaging,
                "remote Bubblewrap protocol returned unexpected trailing bytes",
            ));
        }
        let mut remote_stderr = Vec::new();
        stderr.read_to_end(&mut remote_stderr).await.ok();
        let status = child.wait().await.map_err(|e| {
            BuildPipelineError::transient(
                crate::ci::CiBuildStage::Compiling,
                format!("wait for remote Bubblewrap SSH job: {e}"),
            )
        })?;
        if !status.success() || !result.ok {
            build_report
                .stderr
                .push_str(&String::from_utf8_lossy(&remote_stderr));
            return Err(BuildPipelineError::process_failure(
                crate::ci::CiBuildStage::Compiling,
                status,
                format!("remote Bubblewrap build failed: {}", build_report.stderr),
            ));
        }
        Ok(build_report)
    }
}

#[derive(Debug, Serialize)]
struct JobHeader {
    version: u8,
    mode: String,
    job_id: String,
    target: String,
    rustc: String,
    workspace_root: String,
    bwrap: String,
    rootfs: String,
    toolchain: String,
    source_preparer: String,
    patch_fingerprint: String,
    /// The Pumpkin commit being built; the server reports it as its brand.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_revision: Option<String>,
    cache_root: String,
    cache_max_bytes: u64,
    source_len: usize,
    plugin_len: usize,
    timeout_seconds: u64,
    profile_memory_bytes: u64,
    profile_workspace_bytes: u64,
    profile_workspace_inodes: u64,
    profile_concurrency: u16,
    capability_fingerprint: Option<String>,
}
#[derive(Debug, Deserialize)]
struct JobArtifact {
    name: String,
    size: usize,
    sha256: String,
}
#[derive(Debug, Deserialize)]
struct JobResult {
    ok: bool,
    #[serde(rename = "status")]
    _status: i32,
    stdout: String,
    stderr: String,
    artifacts: Vec<JobArtifact>,
    #[serde(default)]
    capability: Option<RemoteBuilderCapability>,
}

#[async_trait]
impl RootlessBuildRuntime for SshBubblewrapBuildRuntime {
    fn toolchain_version(&self) -> &str {
        &self.identity
    }

    async fn verify_sandbox(&self, limits: SandboxLimits) -> Result<(), BuildPipelineError> {
        self.assert_capability_lease()?;
        if limits != self.profile.limits {
            return Err(BuildPipelineError::Sandbox(
                "remote Bubblewrap workload limits differ from the registered resource profile"
                    .into(),
            ));
        }
        let lease = self
            .admission
            .acquire(limits.memory_bytes)
            .await
            .map_err(|error| BuildPipelineError::Sandbox(error.to_string()))?;
        let token = lease.token();
        let result = self
            .run_job(
                "preflight",
                &token,
                &lease,
                &[],
                &[],
                "x86_64-unknown-linux-gnu",
                None,
                limits,
                None,
            )
            .await;
        lease.release().await;
        result.map(|_| ())
    }

    async fn compile(
        &self,
        source_archive: &[u8],
        plugin_archive: &[u8],
        output_dir: &Path,
        target: &str,
        source_revision: &str,
        limits: SandboxLimits,
    ) -> Result<BuildProcessReport, BuildPipelineError> {
        self.assert_capability_lease()?;
        if limits != self.profile.limits {
            return Err(BuildPipelineError::Sandbox(
                "remote Bubblewrap workload limits differ from the registered resource profile"
                    .into(),
            ));
        }
        let lease = self
            .admission
            .acquire(limits.memory_bytes)
            .await
            .map_err(|error| BuildPipelineError::Sandbox(error.to_string()))?;
        let token = lease.token();
        let result = self
            .run_job(
                "compile",
                &token,
                &lease,
                source_archive,
                plugin_archive,
                target,
                Some(source_revision),
                limits,
                Some(output_dir),
            )
            .await;
        lease.release().await;
        result
    }
}

struct RemoteHostMemoryProbe {
    target: String,
}

#[async_trait]
impl HostMemoryProbe for RemoteHostMemoryProbe {
    async fn snapshot(
        &self,
    ) -> Result<super::admission::HostMemorySnapshot, super::admission::BuildAdmissionError> {
        let mut command = ssh_base_command(&self.target);
        let output = command
            .args(["cat", "/proc/meminfo"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .output();
        let output = timeout(Duration::from_secs(30), output)
            .await
            .map_err(|_| {
                super::admission::BuildAdmissionError::Unavailable(
                    "remote Bubblewrap host memory probe timed out".into(),
                )
            })?
            .map_err(|error| {
                super::admission::BuildAdmissionError::Unavailable(format!(
                    "remote Bubblewrap host memory probe: {error}"
                ))
            })?;
        if !output.status.success() {
            return Err(super::admission::BuildAdmissionError::Unavailable(format!(
                "remote Bubblewrap host memory probe failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        super::admission::parse_meminfo(&String::from_utf8_lossy(&output.stdout)).map_err(|error| {
            super::admission::BuildAdmissionError::Unavailable(format!(
                "remote Bubblewrap host memory probe returned invalid data: {error}"
            ))
        })
    }
}

struct RemoteWorkspaceCleaner {
    target: String,
    workspace_root: String,
}

#[async_trait]
impl AbandonedBuildCleaner for RemoteWorkspaceCleaner {
    async fn remove_lease(
        &self,
        lease_token: &str,
    ) -> Result<(), super::admission::BuildAdmissionError> {
        if lease_token.len() != 32 || !lease_token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(super::admission::BuildAdmissionError::Unavailable(
                "remote Bubblewrap lease token is malformed".into(),
            ));
        }
        let workspace = format!(
            "{}/kurbis-bwrap-remote-{lease_token}",
            self.workspace_root.trim_end_matches('/')
        );
        let mut command = ssh_base_command(&self.target);
        let output = command
            .args(["rm", "-rf", "--", &workspace])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .output();
        let output = timeout(Duration::from_secs(30), output)
            .await
            .map_err(|_| {
                super::admission::BuildAdmissionError::Unavailable(
                    "remote Bubblewrap workspace cleanup timed out".into(),
                )
            })?
            .map_err(|error| {
                super::admission::BuildAdmissionError::Unavailable(format!(
                    "remote Bubblewrap workspace cleanup: {error}"
                ))
            })?;
        if output.status.success() {
            Ok(())
        } else {
            Err(super::admission::BuildAdmissionError::Unavailable(format!(
                "remote Bubblewrap workspace cleanup failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )))
        }
    }
}

async fn probe_remote_memory(
    target: &str,
) -> Result<super::admission::HostMemorySnapshot, BuildPipelineError> {
    RemoteHostMemoryProbe {
        target: target.to_owned(),
    }
    .snapshot()
    .await
    .map_err(|error| BuildPipelineError::Sandbox(error.to_string()))
}

async fn probe_remote_identity(target: &str) -> Result<(u32, u32), BuildPipelineError> {
    let mut command = ssh_base_command(target);
    let output = timeout(
        Duration::from_secs(30),
        command
            // OpenSSH concatenates remote command arguments before invoking the
            // target shell. Passing `sh`, `-c`, and the script as separate SSH
            // arguments makes `printf` become the `sh -c` command's `$0`, so
            // dash receives the format string as an option/argument and exits
            // before emitting the UID/GID. Pass the complete shell command as
            // one SSH argument instead.
            .arg("printf '%s\n%s\n' \"$(id -u)\" \"$(id -g)\"")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| {
        BuildPipelineError::transient(
            crate::ci::CiBuildStage::Compiling,
            "remote Bubblewrap UID/GID probe timed out",
        )
    })?
    .map_err(|error| {
        BuildPipelineError::transient(
            crate::ci::CiBuildStage::Compiling,
            format!("remote Bubblewrap UID/GID probe failed: {error}"),
        )
    })?;
    if !output.status.success() {
        return Err(BuildPipelineError::transient(
            crate::ci::CiBuildStage::Compiling,
            format!(
                "remote Bubblewrap UID/GID probe failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let values = stdout.lines().map(str::trim).collect::<Vec<_>>();
    if values.len() != 2 {
        return Err(BuildPipelineError::Sandbox(
            "remote Bubblewrap UID/GID probe returned malformed output".into(),
        ));
    }
    let uid = values[0].parse::<u32>().map_err(|_| {
        BuildPipelineError::Sandbox("remote Bubblewrap UID probe returned a non-integer".into())
    })?;
    let gid = values[1].parse::<u32>().map_err(|_| {
        BuildPipelineError::Sandbox("remote Bubblewrap GID probe returned a non-integer".into())
    })?;
    Ok((uid, gid))
}

fn ssh_base_command(target: &str) -> Command {
    let mut command = Command::new("ssh");
    command.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "IdentitiesOnly=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ConnectTimeout=15",
        "-o",
        "ServerAliveInterval=30",
        "-o",
        "ServerAliveCountMax=2",
        "-T",
        "--",
        target,
    ]);
    command
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    fn capability() -> RemoteBuilderCapability {
        RemoteBuilderCapability {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            host_uid: 1000,
            host_gid: 1000,
            sandbox_uid: 0,
            sandbox_gid: 0,
            bwrap_hash: "a".repeat(64),
            rootfs_hash: "b".repeat(64),
            toolchain_hash: "c".repeat(64),
            source_preparer_hash: "d".repeat(64),
            rustc_version: "1.98.1".into(),
            workspace_bytes: BuildResourceProfile::default_rootless().limits.disk_bytes,
            workspace_inodes: BuildResourceProfile::default_rootless().workspace_inodes,
            memory_total_bytes: 32 * 1024 * 1024 * 1024,
            memory_available_bytes: 32 * 1024 * 1024 * 1024,
            cgroup_mode: "available_not_delegated".into(),
            smoke_build: true,
            artifact_transfer: true,
            artifact_execution: true,
        }
    }

    #[test]
    fn capability_requires_non_root_remote_account_and_all_smoke_evidence() {
        let profile = BuildResourceProfile::default_rootless();
        assert!(capability().validate(profile, "1.98.1").is_ok());
        let mut invalid = capability();
        invalid.host_uid = 0;
        assert!(invalid.validate(profile, "1.98.1").is_err());
        let mut invalid = capability();
        invalid.artifact_transfer = false;
        assert!(invalid.validate(profile, "1.98.1").is_err());
    }

    #[test]
    fn capability_fingerprint_changes_when_immutable_inputs_change() {
        let first = capability();
        let mut second = first.clone();
        second.toolchain_hash = "e".repeat(64);
        assert_ne!(first.fingerprint(), second.fingerprint());
    }
}
