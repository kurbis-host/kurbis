use std::{sync::Arc, time::Duration};

use chrono::Duration as ChronoDuration;

use anyhow::{Context, Result};
use tokio::time::sleep;
use uuid::Uuid;

use super::{
    BuildKey, CheckRunClient, CheckRunDetails, CheckRunError, CiBuildRetention, CiBuildStage,
    CiHostname, CiPreviewRepository, CiRefKind,
    source_control::{PollError, SourceControlClient, SourceRefKind, retry_delay},
};

#[derive(Debug, Clone)]
pub struct CiPollerConfig {
    pub root_domain: String,
    pub patch_fingerprint: String,
    pub toolchain_version: String,
    pub target_architecture: String,
    pub public_status_base_url: Option<String>,
}
impl Default for CiPollerConfig {
    fn default() -> Self {
        Self {
            root_domain: super::domain::CI_ROOT_DOMAIN.into(),
            patch_fingerprint: "checked-in-host-control".into(),
            toolchain_version: "native".into(),
            target_architecture: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
            public_status_base_url: None,
        }
    }
}

/// Durable source-control poller. A short local gate and a durable database
/// lease serialize provider polling without holding a transaction across HTTP;
/// build records and immutable aliases are deduplicated by their database keys.
pub struct CiPoller<C> {
    repository: CiPreviewRepository,
    source: Arc<C>,
    config: CiPollerConfig,
    check_runs: Option<Arc<dyn CheckRunClient>>,
    builder: Option<Arc<dyn crate::ci::BuildCoordinator>>,
    source_poll_gate: Arc<tokio::sync::Mutex<()>>,
}
impl<C> Clone for CiPoller<C>
where
    C: Clone,
{
    fn clone(&self) -> Self {
        Self {
            repository: self.repository.clone(),
            source: Arc::new((*self.source).clone()),
            config: self.config.clone(),
            check_runs: self.check_runs.clone(),
            builder: self.builder.clone(),
            source_poll_gate: self.source_poll_gate.clone(),
        }
    }
}
impl<C: SourceControlClient + 'static> CiPoller<C> {
    pub fn new(repository: CiPreviewRepository, source: Arc<C>) -> Self {
        Self {
            repository,
            source,
            config: CiPollerConfig::default(),
            check_runs: None,
            builder: None,
            source_poll_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
    pub fn with_config(mut self, config: CiPollerConfig) -> Self {
        self.config = config;
        self
    }

    pub fn with_check_runs(mut self, client: Arc<dyn CheckRunClient>) -> Self {
        self.check_runs = Some(client);
        self
    }
    pub fn with_builder(mut self, builder: Arc<dyn crate::ci::BuildCoordinator>) -> Self {
        self.builder = Some(builder);
        self
    }
    pub async fn poll_once(&self, repository_id: Uuid) -> Result<()> {
        // `run_forever` ticks independently of long build work. Skip a tick
        // instead of queuing another provider poll while this instance is
        // already fetching and persisting the repository snapshot. The durable
        // lease in the repository additionally fences other replicas.
        let Ok(source_poll_guard) = self.source_poll_gate.clone().try_lock_owned() else {
            return Ok(());
        };
        let poll = self
            .repository
            .poll_repository(repository_id, self.source.as_ref())
            .await?;
        drop(source_poll_guard);
        let Some(poll) = poll else {
            return Ok(());
        };
        let repository_name = self.repository_name(repository_id).await?;
        let default_branch = self.repository.default_branch(repository_id).await?;
        let eager_refs = poll.eager_refs;
        // Branch deletion is actionable only after a complete branch poll has
        // durably marked refs deleted. Pull-request close is intentionally not
        // mapped to preview teardown.
        self.repository
            .expire_deleted_branch_aliases(repository_id)
            .await?;
        for source_ref in poll.refs {
            if source_ref.deleted
                || (source_ref.kind == SourceRefKind::PullRequest && source_ref.closed)
            {
                // Closed historical PRs stay in ci_refs for lifecycle
                // accounting, but must not seed queued builds or preview
                // aliases merely because GitHub returns state=all.
                continue;
            }
            let is_main_branch =
                source_ref.kind == SourceRefKind::Branch && source_ref.name == default_branch;
            let key = BuildKey::new(
                &repository_name,
                &source_ref.commit_sha,
                &self.config.patch_fingerprint,
                &self.config.toolchain_version,
                &self.config.target_architecture,
            )?;
            let retention = if is_main_branch {
                CiBuildRetention::Permanent
            } else {
                CiBuildRetention::Preview
            };
            let (build, _) = self
                .repository
                .ensure_build_with_retention(&key, retention)
                .await?;
            let commit_alias = CiHostname::for_ref(
                CiRefKind::Commit,
                &source_ref.commit_sha,
                &self.config.root_domain,
            )?;

            if is_main_branch {
                // The default Pumpkin branch is a stable build input, not a
                // preview. It is built on the first source poll after startup,
                // reusing the exact PR pipeline and its artifact validation while
                // never creating a preview alias or inheriting its TTL.
                if let Some(builder) = self.builder.as_ref() {
                    builder
                        .ensure_build(crate::ci::BuildRequest {
                            key,
                            source_repository: source_ref.source_repository.clone(),
                            source_archive: Vec::new(),
                            retention,
                        })
                        .await
                        .map_err(|error| anyhow::anyhow!("main Pumpkin build failed: {error}"))?;
                }
                continue;
            }

            let kind = source_ref.kind.into();
            self.repository
                .ensure_preview(
                    &repository_name,
                    &source_ref.commit_sha,
                    CiRefKind::Commit,
                    None,
                    &commit_alias,
                    build.id,
                )
                .await?;
            let stable_alias = match source_ref.kind {
                SourceRefKind::PullRequest => CiHostname::for_ref(
                    CiRefKind::PullRequest,
                    &source_ref.name,
                    &self.config.root_domain,
                )?,
                SourceRefKind::Branch => CiHostname::for_ref(
                    CiRefKind::Branch,
                    &source_ref.name,
                    &self.config.root_domain,
                )?,
            };
            self.repository
                .ensure_preview(
                    &repository_name,
                    &source_ref.commit_sha,
                    kind,
                    Some(&source_ref.name),
                    &stable_alias,
                    build.id,
                )
                .await?;
            // GitHub Check Runs are a PR-head status surface. Branch and commit
            // aliases are deliberately build-only and remain lazy until an
            // authenticated join asks for their exact observed head.
            let should_track_check_run = source_ref.kind == SourceRefKind::PullRequest
                && !(source_ref.closed && build.stage == CiBuildStage::Queued);
            if should_track_check_run
                && let Some(check_runs) = self.check_runs.as_ref()
                && self.repository.check_run_id(build.id).await?.is_none()
            {
                self.ensure_check_run(
                    check_runs.as_ref(),
                    build.id,
                    &source_ref,
                    &repository_name,
                    vec![commit_alias.value.clone(), stable_alias.value.clone()],
                )
                .await?;
            }
            let eager_pr = source_ref.kind == SourceRefKind::PullRequest
                && !source_ref.closed
                && eager_refs.iter().any(|eager| {
                    eager.name == source_ref.name && eager.commit_sha == source_ref.commit_sha
                });
            if eager_pr {
                self.ensure_pr_head_build(
                    &source_ref,
                    &repository_name,
                    &source_ref.commit_sha,
                    &commit_alias,
                    &stable_alias,
                )
                .await?;
            }
        }
        if let Some(builder) = self.builder.as_ref() {
            // Queue claims are fenced in PostgreSQL and recovered after a
            // process restart. Provider polling never holds a transaction while
            // the builder performs network or compiler work.
            self.drain_pull_request_queue(&repository_name, builder.as_ref())
                .await?;
        }
        self.reconcile_check_runs(Some(&repository_name)).await?;
        Ok(())
    }

    /// Process queued signed PR lifecycle events without expanding a head into
    /// its ancestors. Each delivery represents exactly one requested PR head.
    pub async fn drain_github_webhooks_once(&self) -> Result<()> {
        for delivery in self.repository.claim_github_pr_deliveries(16).await? {
            let delivery_id = delivery.delivery_id;
            let claim_token = delivery.claim_token;
            let repository = self.repository.clone();
            let renewal = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    match repository
                        .renew_github_pr_delivery_claim(delivery_id, claim_token)
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(error) => {
                            tracing::warn!(delivery_id = %delivery_id, ?error, "failed to renew GitHub PR delivery lease")
                        }
                    }
                }
            });
            let result = self.process_github_pr_delivery(&delivery).await;
            renewal.abort();
            match result {
                Ok(()) => {
                    self.repository
                        .complete_github_pr_delivery(delivery_id, claim_token)
                        .await?;
                }
                Err(error) => {
                    self.repository
                        .retry_github_pr_delivery(delivery_id, claim_token, &error.to_string())
                        .await?;
                    tracing::warn!(delivery_id = %delivery_id, ?error, "GitHub PR delivery will retry");
                }
            }
        }
        self.reconcile_check_runs(None).await?;
        Ok(())
    }

    async fn process_github_pr_delivery(
        &self,
        delivery: &crate::ci::ClaimedCiGitHubPrDelivery,
    ) -> Result<()> {
        let commit_alias = CiHostname::for_ref(
            CiRefKind::Commit,
            &delivery.head_sha,
            &self.config.root_domain,
        )?;
        let stable_alias = CiHostname::for_ref(
            CiRefKind::PullRequest,
            &format!("pr-{}", delivery.pull_request_number),
            &self.config.root_domain,
        )?;
        let source_ref = crate::ci::SourceRef {
            kind: SourceRefKind::PullRequest,
            name: format!("pr-{}", delivery.pull_request_number),
            number: Some(delivery.pull_request_number),
            commit_sha: delivery.head_sha.clone(),
            source_repository: Some(delivery.source_repository.clone()),
            closed: false,
            deleted: false,
            updated_at: Some(chrono::Utc::now()),
        };
        self.ensure_pr_head_build(
            &source_ref,
            &delivery.repository,
            &delivery.head_sha,
            &commit_alias,
            &stable_alias,
        )
        .await
    }

    async fn ensure_pr_head_build(
        &self,
        source_ref: &crate::ci::SourceRef,
        repository: &str,
        commit_sha: &str,
        commit_alias: &CiHostname,
        stable_alias: &CiHostname,
    ) -> Result<()> {
        let key = BuildKey::new(
            repository,
            commit_sha,
            &self.config.patch_fingerprint,
            &self.config.toolchain_version,
            &self.config.target_architecture,
        )?;
        let (build, _) = self.repository.ensure_build(&key).await?;
        self.repository
            .ensure_preview(
                repository,
                commit_sha,
                CiRefKind::Commit,
                None,
                commit_alias,
                build.id,
            )
            .await?;
        if let Some(check_runs) = self.check_runs.as_ref()
            && self.repository.check_run_id(build.id).await?.is_none()
        {
            let mut head = source_ref.clone();
            head.commit_sha = commit_sha.to_owned();
            self.ensure_check_run(
                check_runs.as_ref(),
                build.id,
                &head,
                repository,
                vec![commit_alias.value.clone(), stable_alias.value.clone()],
            )
            .await?;
        }
        let pull_request_number: i64 = source_ref
            .number
            .map(i64::try_from)
            .transpose()?
            .or_else(|| {
                crate::ci::domain::parse_pr_number(&source_ref.name)
                    .ok()
                    .and_then(|value| i64::try_from(value).ok())
            })
            .context("pull request number is required for CI queue admission")?;
        self.repository
            .enqueue_pull_request_build(
                repository,
                pull_request_number,
                &key,
                source_ref.source_repository.as_deref(),
            )
            .await?;
        if let Some(builder) = self.builder.as_ref() {
            self.drain_pull_request_queue(repository, builder.as_ref())
                .await?;
        }
        Ok(())
    }

    async fn drain_pull_request_queue(
        &self,
        repository: &str,
        builder: &dyn crate::ci::BuildCoordinator,
    ) -> Result<()> {
        loop {
            let claims = self
                .repository
                .claim_pull_request_builds(repository, 32, ChronoDuration::minutes(30))
                .await?;
            if claims.is_empty() {
                return Ok(());
            }
            for claim in claims {
                let heartbeat_repository = self.repository.clone();
                let queue_id = claim.id;
                let claim_token = claim.claim_token;
                let heartbeat = tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        match heartbeat_repository
                            .renew_pull_request_build_claim(
                                queue_id,
                                claim_token,
                                ChronoDuration::minutes(30),
                            )
                            .await
                        {
                            Ok(true) => {}
                            Ok(false) => break,
                            Err(error) => {
                                tracing::warn!(%queue_id, ?error, "CI build queue lease heartbeat failed")
                            }
                        }
                    }
                });
                let result = builder.ensure_build(claim.request.clone()).await;
                heartbeat.abort();
                match result {
                    Ok(_) => {
                        if !self
                            .repository
                            .complete_pull_request_build(
                                claim.id,
                                claim.claim_token,
                                true,
                                None,
                                None,
                            )
                            .await?
                        {
                            tracing::debug!(queue_id = %claim.id, "CI build queue claim expired before success was recorded");
                        }
                    }
                    Err(error) => {
                        self.repository
                            .complete_pull_request_build_from_build_error(
                                &claim,
                                &error.to_string(),
                            )
                            .await?;
                        return Err(anyhow::anyhow!("queued CI PR build failed: {error}"));
                    }
                }
            }
        }
    }

    async fn reconcile_check_runs(&self, repository: Option<&str>) -> Result<()> {
        let Some(check_runs) = self.check_runs.as_ref() else {
            return Ok(());
        };
        let candidates = match repository {
            Some(repository) => {
                self.repository
                    .check_run_reconciliation_candidates_for_repository(128, repository)
                    .await?
            }
            None => {
                self.repository
                    .check_run_reconciliation_candidates(128)
                    .await?
            }
        };
        for candidate in candidates {
            let (build_log, expires_at, preview_hostnames) = if candidate.tombstone {
                (String::new(), None, Vec::new())
            } else {
                let (build_log, expires_at, preview_hostnames) = self
                    .repository
                    .build_diagnostics(candidate.build_id, &self.config.root_domain)
                    .await?;
                (build_log, expires_at, preview_hostnames)
            };
            let details_url = self
                .config
                .public_status_base_url
                .as_ref()
                .and_then(|base| {
                    CiHostname::for_ref(
                        CiRefKind::Commit,
                        &candidate.key.commit_sha,
                        &self.config.root_domain,
                    )
                    .ok()
                    .map(|alias| format!("{base}/{}", alias.value))
                });
            let details = CheckRunDetails {
                commit_sha: candidate.key.commit_sha.clone(),
                stage: candidate.stage,
                repository: candidate.key.repository.clone(),
                artifact_digest: candidate.artifact_digest,
                failure_stage: candidate.failure_stage,
                failure_message: candidate.failure_message,
                build_log: build_log
                    .chars()
                    .rev()
                    .take(6 * 1024)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect(),
                preview_hostnames,
                expires_at: expires_at.map(|expires_at| expires_at.to_rfc3339()),
                details_url,
            };
            match check_runs
                .update(
                    &candidate.key.repository,
                    crate::ci::CheckRunId(candidate.check_run_id),
                    &details,
                )
                .await
            {
                Ok(()) => {
                    self.repository
                        .mark_check_run_stage_synced(
                            candidate.build_id,
                            candidate.stage,
                            candidate.tombstone,
                        )
                        .await?;
                }
                Err(error) => tracing::warn!(
                    build_id = %candidate.build_id,
                    stage = %candidate.stage,
                    ?error,
                    "failed to reconcile GitHub CI Check Run"
                ),
            }
        }
        Ok(())
    }

    async fn ensure_check_run(
        &self,
        check_runs: &dyn CheckRunClient,
        build_id: Uuid,
        source_ref: &crate::ci::SourceRef,
        repository: &str,
        preview_hostnames: Vec<String>,
    ) -> Result<()> {
        if source_ref.kind != SourceRefKind::PullRequest {
            return Ok(());
        }
        let claim = Uuid::new_v4();
        if !self
            .repository
            .claim_check_run(build_id, claim, ChronoDuration::minutes(2))
            .await?
        {
            return Ok(());
        }
        let details_url = self
            .config
            .public_status_base_url
            .as_ref()
            .and_then(|base| {
                CiHostname::for_ref(
                    CiRefKind::Commit,
                    &source_ref.commit_sha,
                    &self.config.root_domain,
                )
                .ok()
                .map(|alias| format!("{base}/{}", alias.value))
            });
        let details = CheckRunDetails {
            commit_sha: source_ref.commit_sha.clone(),
            stage: CiBuildStage::Queued,
            repository: repository.to_owned(),
            artifact_digest: None,
            failure_stage: None,
            failure_message: None,
            build_log: String::new(),
            preview_hostnames,
            expires_at: None,
            details_url,
        };
        match check_runs.get_or_create(repository, &details).await {
            Ok(check_run) => {
                self.repository
                    .complete_check_run_claim(
                        build_id,
                        claim,
                        check_run.0,
                        None,
                        CiBuildStage::Queued,
                    )
                    .await?;
                Ok(())
            }
            Err(error) => {
                let rate_limited = matches!(error, CheckRunError::RateLimited { .. });
                let permission_denied = matches!(
                    error,
                    CheckRunError::Http { status, .. } if status == reqwest::StatusCode::FORBIDDEN
                );
                if rate_limited {
                    // Preserve the short claim lease across quota pauses so a
                    // polling storm cannot recreate the same Check Run request
                    // before GitHub's reset window has elapsed.
                    tracing::warn!(
                        build_id = %build_id,
                        ?error,
                        "GitHub Check Run rate limited; continuing CI build without external status"
                    );
                    return Ok(());
                }
                self.repository
                    .release_check_run_claim(build_id, claim)
                    .await?;
                if permission_denied {
                    tracing::warn!(
                        build_id = %build_id,
                        repository,
                        "GitHub Check Run permission denied; continuing CI poll without external status"
                    );
                    return Ok(());
                }
                Err(anyhow::anyhow!(
                    "create or recover GitHub check run: {error}"
                ))
            }
        }
    }

    async fn repository_name(&self, repository_id: Uuid) -> Result<String> {
        self.repository.repository_name(repository_id).await
    }
    pub async fn poll_with_retry(&self, repository_id: Uuid, max_attempts: u32) -> Result<()> {
        let attempts = max_attempts.max(1);
        let mut attempt = 0;
        loop {
            match self.poll_once(repository_id).await {
                Ok(()) => return Ok(()),
                Err(error) if attempt + 1 < attempts => {
                    let delay = error
                        .downcast_ref::<PollError>()
                        .map(|poll_error| retry_delay(poll_error, attempt))
                        .unwrap_or_else(|| {
                            retry_delay(
                                &PollError::Http {
                                    status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
                                    message: error.to_string(),
                                },
                                attempt,
                            )
                        });
                    sleep(delay).await;
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }
    /// Poll the watched repository at a fixed cadence without detaching work.
    /// Callers that need process-level lifecycle ownership should run this
    /// future from their supervisor and retain its join handle.
    pub async fn run_forever(self, repository_id: Uuid, interval: Duration) -> Result<()>
    where
        C: Clone,
    {
        let interval = interval.max(Duration::from_secs(1));
        let mut poll_tick = tokio::time::interval(interval);
        loop {
            poll_tick.tick().await;
            self.poll_with_retry(repository_id, 3).await?;
        }
    }
}
