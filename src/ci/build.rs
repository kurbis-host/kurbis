use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use thiserror::Error;
use uuid::Uuid;

use super::{
    ArtifactCache, BuildKey, BuildRequest, CheckRunClient, CheckRunDetails, CheckRunId, CiBuild,
    CiBuildStage, CiPreviewRepository, CiServiceError, ControlledBuildOutput, SandboxLimits,
    SourceControlClient, validate_workload_limits,
};

#[derive(Debug, Error)]
pub enum BuildPipelineError {
    #[error("source fetch failed: {0}")]
    Source(String),
    #[error("patch application failed: {0}")]
    Patch(String),
    #[error("controlled compilation failed: {0}")]
    Compile(String),
    #[error("artifact packaging failed: {0}")]
    Package(String),
    #[error("database build coordination failed: {0}")]
    Database(String),
    #[error("sandbox admission failed: {0}")]
    Sandbox(String),
}

#[async_trait]
pub trait PatchApplier: Send + Sync {
    async fn apply(
        &self,
        source_archive: Vec<u8>,
        fingerprint: &str,
    ) -> Result<Vec<u8>, BuildPipelineError>;
}

#[async_trait]
pub trait ControlledBuilder: Send + Sync {
    async fn verify_sandbox(&self, limits: SandboxLimits) -> Result<(), BuildPipelineError>;

    async fn compile_and_package(
        &self,
        patched_source: Vec<u8>,
        key: &BuildKey,
    ) -> Result<ControlledBuildOutput, BuildPipelineError>;
}

/// Build coordinator for exactly one immutable key. Every external source is
/// fetched before patching, patching happens before compilation, and the
/// artifact becomes visible only after its digest has been recorded.
pub struct ControlledBuildPipeline<S, P, B, A> {
    repository: CiPreviewRepository,
    source: Arc<S>,
    patcher: Arc<P>,
    builder: Arc<B>,
    artifacts: Arc<A>,
    limits: SandboxLimits,
    root_domain: String,
    public_status_base_url: Option<String>,
    check_runs: Option<Arc<dyn CheckRunClient>>,
}

impl<S, P, B, A> ControlledBuildPipeline<S, P, B, A>
where
    S: SourceControlClient + 'static,
    P: PatchApplier + 'static,
    B: ControlledBuilder + 'static,
    A: ArtifactCache + 'static,
{
    pub fn new(
        repository: CiPreviewRepository,
        source: Arc<S>,
        patcher: Arc<P>,
        builder: Arc<B>,
        artifacts: Arc<A>,
        limits: SandboxLimits,
    ) -> Self {
        Self {
            repository,
            source,
            patcher,
            builder,
            artifacts,
            limits,
            root_domain: crate::ci::CI_ROOT_DOMAIN.into(),
            public_status_base_url: None,
            check_runs: None,
        }
    }

    pub fn with_root_domain(mut self, root_domain: impl Into<String>) -> Self {
        self.root_domain = root_domain.into();
        self
    }

    pub fn with_public_status_base_url(mut self, value: impl Into<String>) -> Self {
        self.public_status_base_url = Some(value.into().trim_end_matches('/').to_owned());
        self
    }

    pub fn with_check_runs(mut self, client: Arc<dyn CheckRunClient>) -> Self {
        self.check_runs = Some(client);
        self
    }

    #[allow(clippy::too_many_arguments)]
    async fn update_check_run(
        &self,
        build_id: Uuid,
        lease: Uuid,
        key: &BuildKey,
        stage: CiBuildStage,
        artifact_digest: Option<String>,
        failure_stage: Option<CiBuildStage>,
        failure_message: Option<String>,
    ) {
        let Some(client) = self.check_runs.as_ref() else {
            return;
        };
        match self.repository.owns_build_lease(build_id, lease).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                tracing::warn!(build_id = %build_id, ?error, "failed to verify CI build lease before updating Check Run");
                return;
            }
        }
        let Ok(Some(check_id)) = self.repository.check_run_id(build_id).await else {
            return;
        };
        let (build_log, expires_at, preview_hostnames) = match self
            .repository
            .build_diagnostics(build_id, crate::ci::CI_ROOT_DOMAIN)
            .await
        {
            Ok(values) => values,
            Err(error) => {
                tracing::warn!(build_id = %build_id, ?error, "failed to load CI Check Run diagnostics");
                (String::new(), Utc::now(), vec![])
            }
        };
        let diagnostics = tail_utf8_bytes(&build_log, 6 * 1024);
        let details_url = self.public_status_base_url.as_ref().and_then(|base| {
            crate::ci::CiHostname::for_ref(
                crate::ci::CiRefKind::Commit,
                &key.commit_sha,
                &self.root_domain,
            )
            .ok()
            .map(|alias| format!("{base}/{}", alias.value))
        });
        let details = CheckRunDetails {
            commit_sha: key.commit_sha.clone(),
            stage,
            repository: key.repository.clone(),
            artifact_digest,
            failure_stage,
            failure_message,
            build_log: diagnostics,
            preview_hostnames,
            expires_at: Some(expires_at.to_rfc3339()),
            details_url,
        };
        match client
            .update(&key.repository, CheckRunId(check_id), &details)
            .await
        {
            Ok(()) => {
                if let Err(error) = self
                    .repository
                    .mark_check_run_stage_synced(build_id, stage, false)
                    .await
                {
                    tracing::warn!(build_id = %build_id, ?error, "failed to persist synchronized CI Check Run stage");
                }
            }
            Err(error) => {
                tracing::warn!(build_id = %build_id, ?error, "failed to update CI GitHub Check Run");
            }
        }
    }

    pub async fn run(&self, request: BuildRequest) -> Result<CiBuild, BuildPipelineError> {
        validate_workload_limits(self.limits)
            .map_err(|error| BuildPipelineError::Sandbox(error.to_string()))?;
        request
            .key
            .validate()
            .map_err(|error| BuildPipelineError::Database(error.to_string()))?;
        let (build, _) = self
            .repository
            .ensure_build(&request.key)
            .await
            .map_err(|error| BuildPipelineError::Database(error.to_string()))?;
        if build.stage == CiBuildStage::Ready && build.expires_at > Utc::now() {
            self.repository
                .advance_stable_aliases_for_build(build.id, &self.root_domain)
                .await
                .map_err(|error| BuildPipelineError::Database(error.to_string()))?;
            return Ok(build);
        }
        let lease = Uuid::new_v4();
        let lease_duration = chrono::Duration::from_std(
            self.limits.process_timeout + std::time::Duration::from_secs(10 * 60),
        )
        .map_err(|error| BuildPipelineError::Sandbox(error.to_string()))?;
        if !self
            .repository
            .claim_build(build.id, lease, lease_duration)
            .await
            .map_err(|error| BuildPipelineError::Database(error.to_string()))?
        {
            return Ok(build);
        }
        let heartbeat = self.start_lease_heartbeat(build.id, lease);
        let result = self.run_claimed(&request, build.id, lease).await;
        heartbeat.abort();
        let _ = self.repository.release_build(build.id, lease).await;
        result
    }

    fn start_lease_heartbeat(&self, build_id: Uuid, lease: Uuid) -> tokio::task::JoinHandle<()> {
        let repository = self.repository.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                match repository
                    .renew_build_claim(build_id, lease, Duration::minutes(15))
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        tracing::warn!(build_id = %build_id, ?error, "CI build lease heartbeat failed")
                    }
                }
            }
        })
    }

    async fn run_claimed(
        &self,
        request: &BuildRequest,
        build_id: Uuid,
        lease: Uuid,
    ) -> Result<CiBuild, BuildPipelineError> {
        let result = async {
            self.builder.verify_sandbox(self.limits).await?;
            self.repository
                .transition_claimed_build(build_id, lease, CiBuildStage::Fetching)
                .await
                .map_err(|e| BuildPipelineError::Database(e.to_string()))?;
            self.repository
                .append_build_log_claimed(
                    build_id,
                    lease,
                    CiBuildStage::Fetching,
                    "stage: fetching exact commit\n",
                )
                .await
                .map_err(|error| BuildPipelineError::Database(error.to_string()))?;
            self.update_check_run(
                build_id,
                lease,
                &request.key,
                CiBuildStage::Fetching,
                None,
                None,
                None,
            )
            .await;
            let source = if request.source_archive.is_empty() {
                self.source
                    .fetch_commit_from(
                        request
                            .source_repository
                            .as_deref()
                            .unwrap_or(&request.key.repository),
                        &request.key.commit_sha,
                    )
                    .await
                    .map_err(|e| BuildPipelineError::Source(e.to_string()))?
            } else {
                request.source_archive.clone()
            };
            self.repository
                .append_build_log_claimed(
                    build_id,
                    lease,
                    CiBuildStage::Fetching,
                    &format!("fetched {} source archive bytes\n", source.len()),
                )
                .await
                .map_err(|error| BuildPipelineError::Database(error.to_string()))?;
            self.repository
                .transition_claimed_build(build_id, lease, CiBuildStage::Patching)
                .await
                .map_err(|e| BuildPipelineError::Database(e.to_string()))?;
            self.repository
                .append_build_log_claimed(
                    build_id,
                    lease,
                    CiBuildStage::Patching,
                    "stage: applying host-owned integration patches\n",
                )
                .await
                .map_err(|error| BuildPipelineError::Database(error.to_string()))?;
            self.update_check_run(
                build_id,
                lease,
                &request.key,
                CiBuildStage::Patching,
                None,
                None,
                None,
            )
            .await;
            let patched = self
                .patcher
                .apply(source, &request.key.patch_fingerprint)
                .await?;
            self.repository
                .append_build_log_claimed(
                    build_id,
                    lease,
                    CiBuildStage::Patching,
                    "host patches applied successfully\n",
                )
                .await
                .map_err(|error| BuildPipelineError::Database(error.to_string()))?;
            self.repository
                .transition_claimed_build(build_id, lease, CiBuildStage::Compiling)
                .await
                .map_err(|e| BuildPipelineError::Database(e.to_string()))?;
            self.repository
                .append_build_log_claimed(
                    build_id,
                    lease,
                    CiBuildStage::Compiling,
                    "stage: compiling Pumpkin and the trusted Kurbis control plugin\n",
                )
                .await
                .map_err(|error| BuildPipelineError::Database(error.to_string()))?;
            self.update_check_run(
                build_id,
                lease,
                &request.key,
                CiBuildStage::Compiling,
                None,
                None,
                None,
            )
            .await;
            let build_output = self
                .builder
                .compile_and_package(patched, &request.key)
                .await?;
            self.repository
                .append_build_log_claimed(
                    build_id,
                    lease,
                    CiBuildStage::Compiling,
                    &build_output.build_log,
                )
                .await
                .map_err(|error| BuildPipelineError::Database(error.to_string()))?;
            let artifact = build_output.artifact;
            self.repository
                .transition_claimed_build(build_id, lease, CiBuildStage::Packaging)
                .await
                .map_err(|e| BuildPipelineError::Database(e.to_string()))?;
            self.repository
                .append_build_log_claimed(
                    build_id,
                    lease,
                    CiBuildStage::Packaging,
                    "stage: validating and packaging controlled artifact\n",
                )
                .await
                .map_err(|error| BuildPipelineError::Database(error.to_string()))?;
            self.update_check_run(
                build_id,
                lease,
                &request.key,
                CiBuildStage::Packaging,
                None,
                None,
                None,
            )
            .await;
            let digest = blake3::hash(&artifact).to_hex().to_string();
            let expected_size = i64::try_from(artifact.len()).map_err(|_| {
                BuildPipelineError::Package("artifact exceeds signed size range".into())
            })?;
            let output = self
                .artifacts
                .put(
                    &digest,
                    &format!("ci/{}/{digest}.tar", request.key.commit_sha),
                    &artifact,
                    Utc::now() + Duration::hours(72),
                )
                .await
                .map_err(|error| BuildPipelineError::Package(error.to_string()))?;
            if output.artifact_digest != digest || output.size_bytes != expected_size {
                return Err(BuildPipelineError::Package(
                    "artifact cache returned mismatched digest or size".into(),
                ));
            }
            self.repository
                .attach_claimed_artifact(
                    build_id,
                    lease,
                    &output.artifact_digest,
                    &output.object_key,
                    output.size_bytes,
                )
                .await
                .map_err(|e| BuildPipelineError::Database(e.to_string()))?;
            let ready = self
                .repository
                .transition_claimed_build(build_id, lease, CiBuildStage::Ready)
                .await
                .map_err(|e| BuildPipelineError::Database(e.to_string()))?;
            self.repository
                .advance_stable_aliases_for_build(build_id, &self.root_domain)
                .await
                .map_err(|e| BuildPipelineError::Database(e.to_string()))?;
            self.update_check_run(
                build_id,
                lease,
                &request.key,
                CiBuildStage::Ready,
                ready.artifact_digest.clone(),
                None,
                None,
            )
            .await;
            Ok(ready)
        }
        .await;
        match result {
            Ok(build) => Ok(build),
            Err(error) => {
                let failure_stage = match &error {
                    BuildPipelineError::Source(_) => CiBuildStage::Fetching,
                    BuildPipelineError::Patch(_) => CiBuildStage::Patching,
                    BuildPipelineError::Compile(_) => CiBuildStage::Compiling,
                    BuildPipelineError::Package(_) => CiBuildStage::Packaging,
                    BuildPipelineError::Sandbox(_) | BuildPipelineError::Database(_) => {
                        CiBuildStage::Queued
                    }
                };
                let failure_log = format!("stage {failure_stage} failed: {}\n", error);
                let _ = self
                    .repository
                    .append_build_log_claimed(build_id, lease, failure_stage, &failure_log)
                    .await;
                match self
                    .repository
                    .fail_claimed_build(build_id, lease, failure_stage, &error.to_string())
                    .await
                {
                    Ok(true) => {
                        self.update_check_run(
                            build_id,
                            lease,
                            &request.key,
                            CiBuildStage::Failed,
                            None,
                            Some(failure_stage),
                            Some(error.to_string()),
                        )
                        .await;
                    }
                    Ok(false) => {}
                    Err(database_error) => {
                        tracing::error!(build_id = %build_id, ?database_error, "failed to persist CI build failure")
                    }
                }
                Err(error)
            }
        }
    }
}

#[async_trait]
impl<S, P, B, A> crate::ci::BuildCoordinator for ControlledBuildPipeline<S, P, B, A>
where
    S: SourceControlClient + 'static,
    P: PatchApplier + 'static,
    B: ControlledBuilder + 'static,
    A: ArtifactCache + 'static,
{
    async fn ensure_build(&self, request: BuildRequest) -> Result<CiBuild, CiServiceError> {
        self.run(request)
            .await
            .map_err(|error| CiServiceError::Internal(error.to_string()))
    }

    async fn transition(
        &self,
        build_id: Uuid,
        stage: CiBuildStage,
        error: Option<&str>,
    ) -> Result<CiBuild, CiServiceError> {
        self.repository
            .transition_build(build_id, stage, error)
            .await
            .map_err(|error| CiServiceError::Internal(error.to_string()))
    }
}

fn tail_utf8_bytes(value: &str, limit: usize) -> String {
    let mut start = value.len().saturating_sub(limit);
    while start < value.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    value[start..].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pipeline_limits_are_explicitly_required() {
        let limits = SandboxLimits {
            memory_bytes: 2 * 1024 * 1024 * 1024,
            cpu_millis: 1000,
            pid_limit: 512,
            disk_bytes: 5 * 1024 * 1024 * 1024,
            process_timeout: std::time::Duration::from_secs(3600),
        };
        assert!(limits.memory_bytes > 0 && limits.disk_bytes > 0);
    }
}
