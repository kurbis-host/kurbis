use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Semaphore,
    time::{timeout, timeout as tokio_timeout},
};
use uuid::Uuid;

use crate::{
    ci::{BuildKey, BuildPipelineError, ControlledBuildOutput, ControlledBuilder, SandboxLimits},
    worker::sandbox::{DockerSandbox, WorkloadLimits},
};

use super::archive::{MAX_ARTIFACT_BYTES, pack_source_archive};

const MAX_BUILD_LOG_BYTES: usize = 512 * 1024;
const MAX_FAILURE_LOG_BYTES: usize = 8 * 1024;
const BUILD_SCRIPT: &str = r#"
set -eu
actual_rustc="$(rustc -vV | awk -F': ' '/^release: / { print $2; exit }')"
[ "$actual_rustc" = "$KURBIS_EXPECTED_RUSTC" ] || {
    echo "builder Rust compiler mismatch: expected $KURBIS_EXPECTED_RUSTC, got $actual_rustc" >&2
    exit 78
}
mkdir -p /work/source /work/plugin /work/out /work/tmp
export TMPDIR=/work/tmp
# The image cache is immutable and already contains every locked offline input.
# Avoid duplicating several GiB into the memory-backed workspace.
export CARGO_HOME=/usr/local/cargo
# Only bounded archives created by trusted control-plane preprocessing and the
# checked-in plugin packer reach this offline extraction step.
tar -xzf /input/source.tar.gz -C /work/source --no-same-owner --no-same-permissions
tar -xzf /input/plugin.tar.gz -C /work/plugin --no-same-owner --no-same-permissions
cd /work/source
cargo build --locked --offline --release --jobs 1 --package pumpkin --bin pumpkin --target "$KURBIS_RUST_TARGET" --target-dir /work/target
cp "/work/target/$KURBIS_RUST_TARGET/release/pumpkin" /work/out/pumpkin
cd /work/plugin
cargo build --locked --offline --release --jobs 1 --manifest-path /work/plugin/Cargo.toml --target wasm32-wasip2 --target-dir /work/plugin-target
cp /work/plugin-target/wasm32-wasip2/release/kurbis_control.wasm /work/out/kurbis-control.wasm
strip /work/out/pumpkin
# Source-controlled build scripts may leave background processes behind.
# Kill every task process except this exec and PID 1 before the output copy.
kill -TERM -1 2>/dev/null || true
sleep 1
kill -KILL -1 2>/dev/null || true
"#;

#[derive(Debug, Clone, Default)]
pub struct BuildProcessReport {
    pub stdout: String,
    pub stderr: String,
}
impl BuildProcessReport {
    fn bounded_log(&self) -> String {
        let combined = format!("{}\n{}", self.stdout, self.stderr);
        tail(&combined, MAX_BUILD_LOG_BYTES)
    }
}

/// Runs build workloads under the worker's verified rootless Docker sandbox.
#[async_trait::async_trait]
pub trait RootlessBuildRuntime: Send + Sync {
    fn toolchain_version(&self) -> &str;
    async fn verify_sandbox(&self, limits: SandboxLimits) -> Result<(), BuildPipelineError>;
    async fn compile(
        &self,
        source_archive: &[u8],
        plugin_archive: &[u8],
        output_dir: &Path,
        target: &str,
        limits: SandboxLimits,
    ) -> Result<BuildProcessReport, BuildPipelineError>;
}

/// Production build runtime. Its image is immutable and contains only the
/// host-owned Rust toolchain/cache; no repository Dockerfile is ever consumed.
#[derive(Clone)]
pub struct DockerRootlessBuildRuntime {
    image: String,
    docker_host: String,
    toolchain_version: String,
    expected_rustc: String,
    sandbox: DockerSandbox,
    build_slots: Arc<Semaphore>,
}
impl DockerRootlessBuildRuntime {
    pub fn new(
        image: impl Into<String>,
        docker_host: impl Into<String>,
        toolchain_version: impl Into<String>,
    ) -> Result<Self, BuildPipelineError> {
        let image = image.into();
        let docker_host = docker_host.into();
        crate::config::validate_ci_builder_docker_host(&docker_host)
            .map_err(|error| BuildPipelineError::Sandbox(error.to_string()))?;
        let toolchain_version = toolchain_version.into();
        let expected_rustc = rustc_release_from_identity(&image, &toolchain_version)
            .map_err(|error| BuildPipelineError::Sandbox(error.to_string()))?;
        Ok(Self {
            image,
            docker_host,
            toolchain_version,
            expected_rustc,
            sandbox: DockerSandbox,
            // A compile uses a 10 GiB memory limit and bounded tmpfs-backed workspace. Keep one
            // host build active per control-plane process to avoid aggregate
            // memory overcommit from concurrent PR and branch requests.
            build_slots: Arc::new(Semaphore::new(1)),
        })
    }

    fn workload_limits(limits: SandboxLimits) -> Result<WorkloadLimits, BuildPipelineError> {
        let limits = WorkloadLimits {
            memory_bytes: limits.memory_bytes,
            cpu_millis: limits.cpu_millis,
            pid_limit: limits.pid_limit,
            disk_bytes: limits.disk_bytes,
        };
        limits
            .validate()
            .map_err(|error| BuildPipelineError::Sandbox(error.to_string()))
    }

    async fn create_task_container(
        &self,
        name: &str,
        limits: WorkloadLimits,
        target: Option<&str>,
        env_names: &[&str],
        env_values: &[&str],
        input_bytes: usize,
        allow_workspace_exec: bool,
    ) -> Result<(), BuildPipelineError> {
        if env_names.len() != env_values.len() {
            return Err(BuildPipelineError::Sandbox(
                "task environment is malformed".into(),
            ));
        }
        let security = self
            .sandbox
            .run_security_args(limits, "0:0", "none", true)
            .map_err(|error| BuildPipelineError::Sandbox(error.to_string()))?;
        const MIB: usize = 1024 * 1024;
        let input_tmpfs_bytes = input_bytes
            .div_ceil(MIB)
            .saturating_add(16)
            .checked_mul(MIB)
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or_else(|| {
                BuildPipelineError::Sandbox("builder input workspace size overflow".into())
            })?;
        let fixed_tmpfs_bytes = 128 * MIB as u64; // /tmp and /dev/shm from run_security_args.
        let workspace_bytes = limits
            .disk_bytes
            .checked_sub(input_tmpfs_bytes.saturating_add(fixed_tmpfs_bytes))
            .filter(|bytes| *bytes > 0)
            .ok_or_else(|| {
                BuildPipelineError::Sandbox(
                    "builder inputs leave no quota for build workspace".into(),
                )
            })?;
        let workspace_options = workspace_tmpfs_options(allow_workspace_exec);
        let mut create = docker_command(&self.docker_host);
        create.arg("create").arg("--pull=never");
        if let Some(target) = target {
            create.args(["--platform", docker_platform(target)?]);
        }
        create
            .arg("--name")
            .arg(name)
            .args(security)
            .args(["--log-driver", "none"])
            .args([
            "--tmpfs",
            &format!(
                "/input:rw,noexec,nosuid,nodev,size={input_tmpfs_bytes},nr_inodes=262144,mode=0700"
            ),
            "--tmpfs",
            &format!(
                "/work:{workspace_options},size={workspace_bytes},nr_inodes=1310720,mode=0700"
            ),
        ]);
        for (key, value) in env_names.iter().zip(env_values) {
            create.args(["--env", &format!("{key}={value}")]);
        }
        let output = create
            .args([
                "--entrypoint",
                "/bin/sh",
                &self.image,
                "-c",
                "while :; do sleep 3600; done",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output();
        let output = timeout(Duration::from_secs(30), output)
            .await
            .map_err(|_| {
                BuildPipelineError::Sandbox("rootless builder container creation timed out".into())
            })?
            .map_err(|error| {
                BuildPipelineError::Sandbox(format!("create rootless builder: {error}"))
            })?;
        if !output.status.success() {
            return Err(BuildPipelineError::Sandbox(format!(
                "could not create rootless Docker task: {}",
                tail(
                    &String::from_utf8_lossy(&output.stderr),
                    MAX_FAILURE_LOG_BYTES
                )
            )));
        }
        Ok(())
    }

    async fn copy_bytes_into(
        &self,
        container: &str,
        filename: &str,
        bytes: &[u8],
    ) -> Result<(), BuildPipelineError> {
        if filename.is_empty()
            || !filename
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err(BuildPipelineError::Sandbox(
                "builder input filename is invalid".into(),
            ));
        }
        let destination = format!("/input/{filename}");
        let script = format!("umask 077; cat > {}", shell_quote(&destination));
        let mut child = docker_command(&self.docker_host)
            .args(["exec", "-i", container, "/bin/sh", "-ec", &script])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                BuildPipelineError::Sandbox(format!("upload input to rootless Docker: {error}"))
            })?;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            BuildPipelineError::Sandbox("builder upload stream was not opened".into())
        })?;
        let output = timeout(Duration::from_secs(180), async {
            stdin.write_all(bytes).await?;
            drop(stdin);
            child.wait_with_output().await
        })
        .await
        .map_err(|_| {
            BuildPipelineError::Sandbox("source upload to rootless Docker timed out".into())
        })?
        .map_err(|error| {
            BuildPipelineError::Sandbox(format!("upload source to rootless Docker: {error}"))
        })?;
        if !output.status.success() {
            return Err(BuildPipelineError::Sandbox(format!(
                "rootless Docker rejected source upload: {}",
                tail(
                    &String::from_utf8_lossy(&output.stderr),
                    MAX_FAILURE_LOG_BYTES
                )
            )));
        }
        Ok(())
    }

    async fn start_task_container(&self, name: &str) -> Result<(), BuildPipelineError> {
        let output = timeout(
            Duration::from_secs(30),
            docker_command(&self.docker_host)
                .args(["start", name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| BuildPipelineError::Sandbox("start rootless builder timed out".into()))?
        .map_err(|error| BuildPipelineError::Sandbox(format!("start rootless builder: {error}")))?;
        if !output.status.success() {
            return Err(BuildPipelineError::Sandbox(format!(
                "could not start rootless builder: {}",
                tail(
                    &String::from_utf8_lossy(&output.stderr),
                    MAX_FAILURE_LOG_BYTES
                )
            )));
        }
        Ok(())
    }

    async fn run_task_container(
        &self,
        name: &str,
        script: &str,
        process_timeout: Duration,
    ) -> Result<(std::process::ExitStatus, BuildProcessReport), BuildPipelineError> {
        let mut child = docker_command(&self.docker_host)
            .args(["exec", name, "/bin/sh", "-ec", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                BuildPipelineError::Sandbox(format!("execute rootless builder task: {error}"))
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| BuildPipelineError::Sandbox("builder stdout was not captured".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| BuildPipelineError::Sandbox("builder stderr was not captured".into()))?;
        let stdout_reader = tokio::spawn(read_capped(stdout, MAX_BUILD_LOG_BYTES / 2));
        let stderr_reader = tokio::spawn(read_capped(stderr, MAX_BUILD_LOG_BYTES / 2));
        let status = match timeout(process_timeout, child.wait()).await {
            Ok(status) => status.map_err(|error| {
                BuildPipelineError::Compile(format!("builder process failed: {error}"))
            })?,
            Err(_) => {
                let _ = child.start_kill();
                let _ = timeout(Duration::from_secs(5), child.wait()).await;
                self.remove_container(name).await;
                return Err(BuildPipelineError::Compile(format!(
                    "controlled build exceeded its {} second limit",
                    process_timeout.as_secs()
                )));
            }
        };
        let stdout = stdout_reader
            .await
            .map_err(|error| BuildPipelineError::Compile(format!("read build stdout: {error}")))?
            .map_err(|error| BuildPipelineError::Compile(format!("read build stdout: {error}")))?;
        let stderr = stderr_reader
            .await
            .map_err(|error| BuildPipelineError::Compile(format!("read build stderr: {error}")))?
            .map_err(|error| BuildPipelineError::Compile(format!("read build stderr: {error}")))?;
        Ok((status, BuildProcessReport { stdout, stderr }))
    }

    async fn validate_container_file(
        &self,
        name: &str,
        source: &str,
        maximum_bytes: usize,
    ) -> Result<(), BuildPipelineError> {
        let script = format!(
            "set -eu; path={}; test -f \"$path\"; test ! -L \"$path\"; size=$(stat -c %s \"$path\"); case \"$size\" in ''|*[!0-9]*) exit 91;; esac; test \"$size\" -gt 0; test \"$size\" -le {maximum_bytes}",
            shell_quote(source)
        );
        let output = timeout(
            Duration::from_secs(15),
            docker_command(&self.docker_host)
                .args(["exec", name, "/bin/sh", "-ec", &script])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| BuildPipelineError::Package("validate rootless output timed out".into()))?
        .map_err(|error| {
            BuildPipelineError::Package(format!("validate rootless output: {error}"))
        })?;
        if !output.status.success() {
            return Err(BuildPipelineError::Package(format!(
                "rootless output is not a bounded regular file: {}",
                tail(
                    &String::from_utf8_lossy(&output.stderr),
                    MAX_FAILURE_LOG_BYTES
                )
            )));
        }
        Ok(())
    }

    async fn copy_from_container(
        &self,
        name: &str,
        source: &str,
        destination: &Path,
        limit: usize,
    ) -> Result<(), BuildPipelineError> {
        if !source.starts_with("/work/") || source.contains("..") {
            return Err(BuildPipelineError::Package(
                "rootless builder output path is invalid".into(),
            ));
        }
        let mut child = docker_command(&self.docker_host)
            .args(["exec", name, "/bin/cat", source])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                BuildPipelineError::Package(format!("stream from rootless builder: {error}"))
            })?;
        let mut stdout = child.stdout.take().ok_or_else(|| {
            BuildPipelineError::Package("builder output stream was not opened".into())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            BuildPipelineError::Package("builder error stream was not opened".into())
        })?;
        let stderr_reader = tokio::spawn(read_capped(stderr, MAX_FAILURE_LOG_BYTES));
        let mut output = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .await
            .map_err(|error| {
                BuildPipelineError::Package(format!(
                    "create bounded artifact staging file: {error}"
                ))
            })?;
        let copied = async {
            let mut total = 0usize;
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let read = stdout.read(&mut buffer).await?;
                if read == 0 {
                    break;
                }
                total = total
                    .checked_add(read)
                    .ok_or_else(|| std::io::Error::other("rootless output size overflow"))?;
                if total > limit {
                    return Err(std::io::Error::other(
                        "rootless output exceeded its transfer limit",
                    ));
                }
                output.write_all(&buffer[..read]).await?;
            }
            output.flush().await?;
            drop(output);
            let status = child.wait().await?;
            if !status.success() {
                return Err(std::io::Error::other(format!(
                    "rootless output stream exited with {status}"
                )));
            }
            Ok::<usize, std::io::Error>(total)
        };
        let copied = match timeout(Duration::from_secs(180), copied).await {
            Ok(Ok(copied)) => copied,
            Ok(Err(error)) => {
                let _ = child.start_kill();
                let _ = tokio::fs::remove_file(destination).await;
                let detail = stderr_reader
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or_default();
                return Err(BuildPipelineError::Package(format!(
                    "rootless output stream failed: {error}; {}",
                    tail(&detail, MAX_FAILURE_LOG_BYTES)
                )));
            }
            Err(_) => {
                let _ = child.start_kill();
                let _ = tokio::fs::remove_file(destination).await;
                return Err(BuildPipelineError::Package(
                    "copy from rootless builder timed out".into(),
                ));
            }
        };
        let stderr = stderr_reader
            .await
            .map_err(|error| BuildPipelineError::Package(format!("read builder error: {error}")))?
            .map_err(|error| BuildPipelineError::Package(format!("read builder error: {error}")))?;
        let metadata = fs::symlink_metadata(destination).map_err(|error| {
            BuildPipelineError::Package(format!("inspect rootless output: {error}"))
        })?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() == 0
            || metadata.len() > limit as u64
            || metadata.len() != copied as u64
        {
            let _ = fs::remove_file(destination);
            return Err(BuildPipelineError::Package(format!(
                "rootless builder output is not a bounded regular file: {}",
                tail(&stderr, MAX_FAILURE_LOG_BYTES)
            )));
        }
        Ok(())
    }

    async fn run_docker(
        &self,
        source_archive: &[u8],
        plugin_archive: &[u8],
        output_dir: &Path,
        target: &str,
        limits: SandboxLimits,
    ) -> Result<BuildProcessReport, BuildPipelineError> {
        let process_timeout = limits.process_timeout;
        let limits = Self::workload_limits(limits)?;
        if source_archive.is_empty()
            || source_archive.len() > super::archive::MAX_COMPRESSED_SOURCE_BYTES
            || plugin_archive.is_empty()
            || plugin_archive.len() > MAX_PLUGIN_SOURCE_BYTES as usize
        {
            return Err(BuildPipelineError::Compile(
                "source or trusted plugin archive exceeds the bounded input limit".into(),
            ));
        }
        let output_dir = accessible_directory(output_dir, "artifact output")?;
        let _build_slot = self.build_slots.acquire().await.map_err(|error| {
            BuildPipelineError::Compile(format!("builder admission closed: {error}"))
        })?;
        if output_dir.to_string_lossy().contains(',') {
            return Err(BuildPipelineError::Sandbox(
                "CI artifact output path may not contain Docker mount separators".into(),
            ));
        }
        let name = format!("kurbis-ci-build-{}", Uuid::new_v4().simple());
        if let Err(error) = self
            .create_task_container(
                &name,
                limits,
                Some(target),
                &[
                    "KURBIS_EXPECTED_RUSTC",
                    "KURBIS_RUST_TARGET",
                    "CARGO_NET_OFFLINE",
                    "CARGO_BUILD_JOBS",
                    "CARGO_PROFILE_RELEASE_LTO",
                    "CARGO_PROFILE_RELEASE_CODEGEN_UNITS",
                ],
                &[&self.expected_rustc, target, "true", "1", "false", "16"],
                source_archive.len().saturating_add(plugin_archive.len()),
                true,
            )
            .await
        {
            self.remove_container(&name).await;
            return Err(error);
        }
        if let Err(error) = self.start_task_container(&name).await {
            self.remove_container(&name).await;
            return Err(error);
        }
        for (name_in_container, archive) in [
            ("source.tar.gz", source_archive),
            ("plugin.tar.gz", plugin_archive),
        ] {
            if let Err(error) = self
                .copy_bytes_into(&name, name_in_container, archive)
                .await
            {
                self.remove_container(&name).await;
                return Err(error);
            }
        }
        let (status, report) = match self
            .run_task_container(&name, BUILD_SCRIPT, process_timeout)
            .await
        {
            Ok((status, report)) if status.success() => (status, report),
            Ok((status, report)) => {
                self.remove_container(&name).await;
                return Err(BuildPipelineError::Compile(format!(
                    "sandboxed Pumpkin build failed ({status})\n{}",
                    tail(&report.bounded_log(), MAX_FAILURE_LOG_BYTES)
                )));
            }
            Err(error) => {
                self.remove_container(&name).await;
                return Err(error);
            }
        };
        let max_artifact = u64::try_from(MAX_ARTIFACT_BYTES)
            .map_err(|error| BuildPipelineError::Package(error.to_string()))?;
        let copy_results = async {
            for filename in ["pumpkin", "kurbis-control.wasm"] {
                let maximum = usize::try_from(max_artifact.min(limits.disk_bytes))
                    .map_err(|error| BuildPipelineError::Package(error.to_string()))?;
                let container_path = format!("/work/out/{filename}");
                self.validate_container_file(&name, &container_path, maximum)
                    .await?;
                self.copy_from_container(
                    &name,
                    &format!("/work/out/{filename}"),
                    &output_dir.join(filename),
                    maximum,
                )
                .await?;
            }
            Ok::<(), BuildPipelineError>(())
        }
        .await;
        self.remove_container(&name).await;
        copy_results?;
        let _ = status;
        Ok(report)
    }

    async fn remove_container(&self, name: &str) {
        let _ = tokio_timeout(
            Duration::from_secs(10),
            docker_command(&self.docker_host)
                .args(["rm", "--force", name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await;
    }
}

#[async_trait::async_trait]
impl crate::ci::PatchApplier for DockerRootlessBuildRuntime {
    async fn apply(
        &self,
        source_archive: Vec<u8>,
        fingerprint: &str,
    ) -> Result<Vec<u8>, BuildPipelineError> {
        if source_archive.is_empty()
            || source_archive.len() > super::archive::MAX_COMPRESSED_SOURCE_BYTES
            || fingerprint != super::checked_in_patch_fingerprint()
        {
            return Err(BuildPipelineError::Patch(
                "source archive or checked-in patch fingerprint is invalid".into(),
            ));
        }
        let limits = Self::workload_limits(crate::ci::ROOTLESS_BUILD_LIMITS)?;
        let _build_slot = self.build_slots.acquire().await.map_err(|error| {
            BuildPipelineError::Patch(format!("builder admission closed: {error}"))
        })?;
        let name = format!("kurbis-ci-prepare-{}", Uuid::new_v4().simple());
        let script = "set -eu; mkdir -p /work/tmp; export TMPDIR=/work/tmp; /usr/local/bin/ci-source-preparer /input/source.tar.gz /work/patched-source.tar.gz \"$KURBIS_EXPECTED_PATCH_FINGERPRINT\"";
        if let Err(error) = self
            .create_task_container(
                &name,
                limits,
                None,
                &["KURBIS_EXPECTED_PATCH_FINGERPRINT"],
                &[fingerprint],
                source_archive.len(),
                false,
            )
            .await
        {
            self.remove_container(&name).await;
            return Err(BuildPipelineError::Patch(error.to_string()));
        }
        if let Err(error) = self.start_task_container(&name).await {
            self.remove_container(&name).await;
            return Err(BuildPipelineError::Patch(error.to_string()));
        }
        if let Err(error) = self
            .copy_bytes_into(&name, "source.tar.gz", &source_archive)
            .await
        {
            self.remove_container(&name).await;
            return Err(BuildPipelineError::Patch(error.to_string()));
        }
        let result = self
            .run_task_container(&name, script, Duration::from_secs(15 * 60))
            .await;
        let (status, report) = match result {
            Ok((status, report)) if status.success() => (status, report),
            Ok((status, report)) => {
                self.remove_container(&name).await;
                return Err(BuildPipelineError::Patch(format!(
                    "rootless Pumpkin patch workload failed ({status}): {}",
                    tail(&report.bounded_log(), MAX_FAILURE_LOG_BYTES)
                )));
            }
            Err(error) => {
                self.remove_container(&name).await;
                return Err(BuildPipelineError::Patch(error.to_string()));
            }
        };
        let directory = tempfile::Builder::new()
            .prefix("kurbis-ci-patched-source-")
            .tempdir()
            .map_err(|error| BuildPipelineError::Patch(error.to_string()))?;
        let output = directory.path().join("patched-source.tar.gz");
        let copied = self
            .copy_from_container(
                &name,
                "/work/patched-source.tar.gz",
                &output,
                super::archive::MAX_COMPRESSED_SOURCE_BYTES,
            )
            .await;
        self.remove_container(&name).await;
        copied.map_err(|error| BuildPipelineError::Patch(error.to_string()))?;
        let metadata = fs::symlink_metadata(&output).map_err(|error| {
            BuildPipelineError::Patch(format!("inspect patched source archive: {error}"))
        })?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() == 0
            || metadata.len() > super::archive::MAX_COMPRESSED_SOURCE_BYTES as u64
        {
            return Err(BuildPipelineError::Patch(
                "rootless patch workload produced invalid output".into(),
            ));
        }
        let _ = (status, report);
        fs::read(output)
            .map_err(|error| BuildPipelineError::Patch(format!("read patched source: {error}")))
    }
}

#[async_trait::async_trait]
impl RootlessBuildRuntime for DockerRootlessBuildRuntime {
    fn toolchain_version(&self) -> &str {
        &self.toolchain_version
    }

    async fn verify_sandbox(&self, limits: SandboxLimits) -> Result<(), BuildPipelineError> {
        let limits = Self::workload_limits(limits)?;
        self.sandbox
            .verify_builder_with_limits_at(
                &self.image,
                limits,
                &self.docker_host,
                std::env::consts::ARCH,
            )
            .await
            .map_err(|error| BuildPipelineError::Sandbox(error.to_string()))
    }

    async fn compile(
        &self,
        source_archive: &[u8],
        plugin_archive: &[u8],
        output_dir: &Path,
        target: &str,
        limits: SandboxLimits,
    ) -> Result<BuildProcessReport, BuildPipelineError> {
        self.run_docker(source_archive, plugin_archive, output_dir, target, limits)
            .await
    }
}

pub struct RustPumpkinBuilder<R> {
    runtime: Arc<R>,
    plugin_source: PathBuf,
    limits: SandboxLimits,
}
impl<R> RustPumpkinBuilder<R> {
    pub fn new(
        runtime: Arc<R>,
        plugin_source: impl Into<PathBuf>,
    ) -> Result<Self, BuildPipelineError> {
        let plugin_source = plugin_source.into();
        validate_plugin_source(&plugin_source)?;
        Ok(Self {
            runtime,
            plugin_source,
            limits: default_build_limits(),
        })
    }

    pub fn with_limits(mut self, limits: SandboxLimits) -> Self {
        self.limits = limits;
        self
    }
}

#[async_trait::async_trait]
impl<R> ControlledBuilder for RustPumpkinBuilder<R>
where
    R: RootlessBuildRuntime + 'static,
{
    async fn verify_sandbox(&self, limits: SandboxLimits) -> Result<(), BuildPipelineError> {
        self.runtime.verify_sandbox(limits).await
    }

    async fn compile_and_package(
        &self,
        patched_source: Vec<u8>,
        key: &BuildKey,
    ) -> Result<ControlledBuildOutput, BuildPipelineError> {
        key.validate()
            .map_err(|error| BuildPipelineError::Compile(error.to_string()))?;
        if key.toolchain_version != self.runtime.toolchain_version() {
            return Err(BuildPipelineError::Compile(
                "build key toolchain identity does not match the active immutable builder image"
                    .into(),
            ));
        }
        let target = rust_target(&key.target_architecture)?;
        if patched_source.is_empty()
            || patched_source.len() > super::archive::MAX_COMPRESSED_SOURCE_BYTES
        {
            return Err(BuildPipelineError::Compile(
                "patched source archive is empty or exceeds the compressed-size limit".into(),
            ));
        }
        let plugin_context = plugin_build_context(&self.plugin_source)
            .map_err(|error| BuildPipelineError::Compile(error.to_string()))?;
        let plugin_archive = pack_source_archive(plugin_context.path())
            .map_err(|error| BuildPipelineError::Compile(error.to_string()))?;
        if plugin_archive.len() > MAX_PLUGIN_SOURCE_BYTES as usize {
            return Err(BuildPipelineError::Compile(
                "trusted plugin archive exceeds its size limit".into(),
            ));
        }
        let output = tempfile::Builder::new()
            .prefix("kurbis-ci-artifact-")
            .tempdir()
            .map_err(|error| BuildPipelineError::Package(error.to_string()))?;
        let report = self
            .runtime
            .compile(
                &patched_source,
                &plugin_archive,
                output.path(),
                target,
                self.limits,
            )
            .await?;
        let binary = output.path().join("pumpkin");
        let plugin = output.path().join("kurbis-control.wasm");
        validate_pumpkin_binary(&binary, &key.target_architecture)?;
        validate_wasm_plugin(&plugin)?;
        let build_log = report.bounded_log();
        let artifact = package_artifact(&binary, &plugin, output.path(), key, &build_log)?;
        Ok(ControlledBuildOutput {
            artifact,
            build_log,
        })
    }
}

mod artifact;
mod support;
use artifact::*;
pub use support::toolchain_identity;
use support::*;

fn tail(value: &str, limit: usize) -> String {
    let mut start = value.len().saturating_sub(limit);
    while start < value.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    value[start..].to_owned()
}

async fn read_capped<R>(mut reader: R, limit: usize) -> Result<String, std::io::Error>
where
    R: tokio::io::AsyncRead + Unpin,
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
            captured.extend_from_slice(&buffer[read.saturating_sub(limit)..read]);
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

#[cfg(test)]
mod tests;
