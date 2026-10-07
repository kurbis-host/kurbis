//! Remote Bubblewrap build transport.
//!
//! The control plane stays on the runtime worker host and sends one framed job
//! over a pinned SSH account to a build-only Linux host. The remote helper is
//! checked into this repository, validates private HOME staging, and returns
//! bounded artifacts with SHA-256 metadata before the coordinator packages and
//! re-hashes them into the normal CI artifact cache.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::Semaphore,
    time::timeout,
};

use super::{
    archive::{MAX_ARTIFACT_BYTES, MAX_COMPRESSED_SOURCE_BYTES},
    builder::{BuildProcessReport, RootlessBuildRuntime},
};
use crate::ci::{BuildPipelineError, SandboxLimits};

const MAX_PLUGIN_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_RESPONSE_LINE: usize = 256 * 1024;

#[derive(Clone)]
pub struct SshBubblewrapBuildRuntime {
    target: String,
    runner: PathBuf,
    bwrap: String,
    rootfs: String,
    toolchain: String,
    workspace_root: String,
    source_preparer: String,
    rustc_version: String,
    identity: String,
    slots: Arc<Semaphore>,
}

impl SshBubblewrapBuildRuntime {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
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
        let slots = usize::from(concurrency.max(1));
        let identity = format!(
            "bubblewrap_namespace_only;transport=ssh;target={target};runner={};bwrap={bwrap};rootfs={rootfs};toolchain={toolchain};workspace={workspace_root};source_preparer={source_preparer};rust={rustc_version}",
            runner.display()
        );
        Ok(Self {
            target,
            runner,
            bwrap,
            rootfs,
            toolchain,
            workspace_root,
            source_preparer,
            rustc_version,
            identity,
            slots: Arc::new(Semaphore::new(slots)),
        })
    }

    pub fn build_identity(&self) -> &str {
        &self.identity
    }

    fn command(&self) -> Command {
        let mut command = Command::new("ssh");
        command.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=15",
            "-o",
            "ServerAliveInterval=30",
            "-o",
            "ServerAliveCountMax=2",
            "-T",
            &self.target,
        ]);
        command
            .args(["unshare", "-Urnm", "python3"])
            .arg(&self.runner);
        command
    }

    async fn run_job(
        &self,
        mode: &str,
        source: &[u8],
        plugin: &[u8],
        target: &str,
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
        let job_id = uuid::Uuid::new_v4().simple().to_string();
        let header = JobHeader {
            version: 1,
            mode: mode.to_owned(),
            job_id,
            target: target.to_owned(),
            rustc: self.rustc_version.clone(),
            workspace_root: self.workspace_root.clone(),
            bwrap: self.bwrap.clone(),
            rootfs: self.rootfs.clone(),
            toolchain: self.toolchain.clone(),
            source_preparer: self.source_preparer.clone(),
            source_len: source.len(),
            plugin_len: plugin.len(),
            timeout_seconds: limits.process_timeout.as_secs().clamp(1, 90 * 60),
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
        let result: JobResult = serde_json::from_slice(&line).map_err(|e| {
            BuildPipelineError::transient(
                crate::ci::CiBuildStage::Compiling,
                format!("decode remote Bubblewrap result: {e}"),
            )
        })?;
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
    source_len: usize,
    plugin_len: usize,
    timeout_seconds: u64,
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
}

#[async_trait]
impl RootlessBuildRuntime for SshBubblewrapBuildRuntime {
    fn toolchain_version(&self) -> &str {
        &self.identity
    }
    async fn verify_sandbox(&self, limits: SandboxLimits) -> Result<(), BuildPipelineError> {
        let _permit = self.slots.acquire().await.map_err(|_| {
            BuildPipelineError::Sandbox("remote Bubblewrap semaphore closed".into())
        })?;
        self.run_job(
            "preflight",
            &[],
            &[],
            "x86_64-unknown-linux-gnu",
            limits,
            None,
        )
        .await
        .map(|_| ())
    }
    async fn compile(
        &self,
        source_archive: &[u8],
        plugin_archive: &[u8],
        output_dir: &Path,
        target: &str,
        limits: SandboxLimits,
    ) -> Result<BuildProcessReport, BuildPipelineError> {
        let _permit = self.slots.acquire().await.map_err(|_| {
            BuildPipelineError::Sandbox("remote Bubblewrap semaphore closed".into())
        })?;
        self.run_job(
            "compile",
            source_archive,
            plugin_archive,
            target,
            limits,
            Some(output_dir),
        )
        .await
    }
}
