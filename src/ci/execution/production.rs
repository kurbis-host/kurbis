use std::{path::PathBuf, sync::Arc, time::Duration};

use crate::ci::{
    ArtifactCache, BuildPipelineError, CheckRunClient, CiPreviewRepository,
    ControlledBuildPipeline, SandboxLimits, SourceControlClient,
};

use super::{DockerRootlessBuildRuntime, RustPumpkinBuilder};

/// Limits used by the rootless Docker Pumpkin builder. These are deliberately separate
/// from runtime limits: compilation needs more memory, but remains bounded.
pub const ROOTLESS_BUILD_LIMITS: SandboxLimits = SandboxLimits {
    memory_bytes: 10 * 1024 * 1024 * 1024,
    cpu_millis: 2000,
    pid_limit: 512,
    disk_bytes: 5 * 1024 * 1024 * 1024,
    process_timeout: Duration::from_secs(90 * 60),
};

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
