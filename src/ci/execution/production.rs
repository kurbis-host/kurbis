use std::{path::PathBuf, sync::Arc};

use crate::ci::{
    ArtifactCache, BuildPipelineError, CheckRunClient, CiPreviewRepository,
    ControlledBuildPipeline, SandboxLimits, SourceControlClient,
};

use super::{
    BubblewrapBuildRuntime, BuildResourceProfile, DockerRootlessBuildRuntime, RustPumpkinBuilder,
    SshBubblewrapBuildRuntime,
};

/// Limits used by the rootless Docker Pumpkin builder. These are deliberately separate
/// from runtime limits: compilation needs more memory, but remains bounded.
pub const ROOTLESS_BUILD_LIMITS: SandboxLimits = BuildResourceProfile::default_rootless().limits;

/// Construct the complete host-owned rootless Docker pipeline. The returned pipeline
/// never reads a fetched Dockerfile; source retrieval, patching, and compilation
/// are all explicit trusted-host components.
#[allow(clippy::too_many_arguments)]
pub fn docker_pipeline<S, A>(
    repository: CiPreviewRepository,
    source: Arc<S>,
    artifacts: Arc<A>,
    builder_image: impl Into<String>,
    builder_docker_host: impl Into<String>,
    toolchain_version: impl Into<String>,
    build_concurrency: u16,
    plugin_source: impl Into<PathBuf>,
    check_runs: Option<Arc<dyn CheckRunClient>>,
) -> Result<
    ControlledBuildPipeline<
        S,
        DockerRootlessBuildRuntime,
        RustPumpkinBuilder<DockerRootlessBuildRuntime>,
        A,
    >,
    BuildPipelineError,
>
where
    S: SourceControlClient + 'static,
    A: ArtifactCache + 'static,
{
    let runtime = Arc::new(DockerRootlessBuildRuntime::new(
        repository.pool(),
        builder_image,
        builder_docker_host,
        toolchain_version,
        build_concurrency,
    )?);
    let builder = RustPumpkinBuilder::new(runtime.clone(), plugin_source)?;
    let pipeline = ControlledBuildPipeline::new(
        repository,
        source,
        runtime,
        Arc::new(builder),
        artifacts,
        ROOTLESS_BUILD_LIMITS,
    );
    Ok(match check_runs {
        Some(client) => pipeline.with_check_runs(client),
        None => pipeline,
    })
}

/// Construct the explicit Bubblewrap namespace-only pipeline. Docker is never
/// used as an implicit fallback; the returned identity includes the Bubblewrap
/// version and immutable root/toolchain manifests for cache separation.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub fn bubblewrap_pipeline<S, A>(
    repository: CiPreviewRepository,
    source: Arc<S>,
    artifacts: Arc<A>,
    bwrap: impl Into<PathBuf>,
    rootfs: impl Into<PathBuf>,
    toolchain: impl Into<PathBuf>,
    workspace_root: impl Into<PathBuf>,
    source_preparer: impl Into<PathBuf>,
    rustc_version: impl Into<String>,
    plugin_source: impl Into<PathBuf>,
    build_concurrency: u16,
    check_runs: Option<Arc<dyn CheckRunClient>>,
) -> Result<
    (
        ControlledBuildPipeline<
            S,
            BubblewrapBuildRuntime,
            RustPumpkinBuilder<BubblewrapBuildRuntime>,
            A,
        >,
        String,
    ),
    BuildPipelineError,
>
where
    S: SourceControlClient + 'static,
    A: ArtifactCache + 'static,
{
    let runtime = Arc::new(
        BubblewrapBuildRuntime::new(
            bwrap,
            rootfs,
            toolchain,
            workspace_root,
            source_preparer,
            rustc_version,
        )?
        .with_admission(repository.pool(), build_concurrency)?,
    );
    let identity = runtime.build_identity().to_owned();
    let builder = RustPumpkinBuilder::new(runtime.clone(), plugin_source)?;
    let pipeline = ControlledBuildPipeline::new(
        repository,
        source,
        runtime,
        Arc::new(builder),
        artifacts,
        ROOTLESS_BUILD_LIMITS,
    );
    let pipeline = match check_runs {
        Some(client) => pipeline.with_check_runs(client),
        None => pipeline,
    };
    Ok((pipeline, identity))
}

/// Construct the respond5-side coordinator pipeline that delegates only the
/// untrusted compilation namespace to a dedicated lxhalle SSH account.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub fn remote_bubblewrap_pipeline<S, A>(
    repository: CiPreviewRepository,
    source: Arc<S>,
    artifacts: Arc<A>,
    ssh_target: impl Into<String>,
    runner: impl Into<PathBuf>,
    bwrap: impl Into<String>,
    rootfs: impl Into<String>,
    toolchain: impl Into<String>,
    workspace_root: impl Into<String>,
    source_preparer: impl Into<String>,
    rustc_version: impl Into<String>,
    plugin_source: impl Into<PathBuf>,
    build_concurrency: u16,
    check_runs: Option<Arc<dyn CheckRunClient>>,
) -> Result<
    (
        ControlledBuildPipeline<
            S,
            super::CheckedInPumpkinPatcher,
            RustPumpkinBuilder<SshBubblewrapBuildRuntime>,
            A,
        >,
        String,
    ),
    BuildPipelineError,
>
where
    S: SourceControlClient + 'static,
    A: ArtifactCache + 'static,
{
    let runtime = Arc::new(SshBubblewrapBuildRuntime::new(
        repository.pool(),
        ssh_target,
        runner,
        bwrap,
        rootfs,
        toolchain,
        workspace_root,
        source_preparer,
        rustc_version,
        build_concurrency,
    )?);
    let identity = runtime.build_identity().to_owned();
    let builder = RustPumpkinBuilder::new(runtime, plugin_source)?;
    let pipeline = ControlledBuildPipeline::new(
        repository,
        source,
        Arc::new(super::CheckedInPumpkinPatcher),
        Arc::new(builder),
        artifacts,
        ROOTLESS_BUILD_LIMITS,
    );
    let pipeline = match check_runs {
        Some(client) => pipeline.with_check_runs(client),
        None => pipeline,
    };
    Ok((pipeline, identity))
}

/// Construct and register the remote builder before exposing CI readiness.
///
/// Registration performs the measured UID/GID, immutable-input, toolchain,
/// workspace, memory/cgroup, smoke-build, and artifact transfer/execution
/// preflight. A pipeline is never returned when that capability lease is
/// missing or invalid.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub async fn remote_bubblewrap_pipeline_ready<S, A>(
    repository: CiPreviewRepository,
    source: Arc<S>,
    artifacts: Arc<A>,
    ssh_target: impl Into<String>,
    runner: impl Into<PathBuf>,
    bwrap: impl Into<String>,
    rootfs: impl Into<String>,
    toolchain: impl Into<String>,
    workspace_root: impl Into<String>,
    source_preparer: impl Into<String>,
    rustc_version: impl Into<String>,
    plugin_source: impl Into<PathBuf>,
    build_concurrency: u16,
    check_runs: Option<Arc<dyn CheckRunClient>>,
) -> Result<
    (
        ControlledBuildPipeline<
            S,
            super::CheckedInPumpkinPatcher,
            RustPumpkinBuilder<SshBubblewrapBuildRuntime>,
            A,
        >,
        String,
    ),
    BuildPipelineError,
>
where
    S: SourceControlClient + 'static,
    A: ArtifactCache + 'static,
{
    let runtime = Arc::new(SshBubblewrapBuildRuntime::new(
        repository.pool(),
        ssh_target,
        runner,
        bwrap,
        rootfs,
        toolchain,
        workspace_root,
        source_preparer,
        rustc_version,
        build_concurrency,
    )?);
    runtime.register_capability().await?;
    let identity = runtime.build_identity().to_owned();
    let builder = RustPumpkinBuilder::new(runtime, plugin_source)?;
    let pipeline = ControlledBuildPipeline::new(
        repository,
        source,
        Arc::new(super::CheckedInPumpkinPatcher),
        Arc::new(builder),
        artifacts,
        ROOTLESS_BUILD_LIMITS,
    );
    let pipeline = match check_runs {
        Some(client) => pipeline.with_check_runs(client),
        None => pipeline,
    };
    Ok((pipeline, identity))
}
