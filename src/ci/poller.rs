use std::{sync::Arc, time::Duration};

use chrono::Duration as ChronoDuration;

use anyhow::Result;
use tokio::time::sleep;
use uuid::Uuid;

use super::{
    BuildKey, CheckRunClient, CheckRunDetails, CiBuildStage, CiHostname, CiPreviewRepository,
    CiRefKind,
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

/// Durable source-control poller. A repository row lock is held by
/// `poll_repository` across the provider request and persistence, then build
/// records and immutable aliases are deduplicated by their database keys.
pub struct CiPoller<C> {
    repository: CiPreviewRepository,
    source: Arc<C>,
    config: CiPollerConfig,
    check_runs: Option<Arc<dyn CheckRunClient>>,
    builder: Option<Arc<dyn crate::ci::BuildCoordinator>>,
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
        let poll = self
            .repository
            .poll_repository(repository_id, self.source.as_ref())
            .await?;
        let repository_name = self.repository_name(repository_id).await?;
        // Branch deletion is actionable only after a complete branch poll has
        // durably marked refs deleted. Pull-request close is intentionally not
        // mapped to preview teardown.
        self.repository
            .expire_deleted_branch_aliases(repository_id)
            .await?;
        for source_ref in poll.refs {
            if source_ref.deleted {
                continue;
            }
            let kind = match source_ref.kind {
                SourceRefKind::PullRequest => CiRefKind::PullRequest,
                SourceRefKind::Branch => CiRefKind::Branch,
            };
            let key = BuildKey::new(
                &repository_name,
                &source_ref.commit_sha,
                &self.config.patch_fingerprint,
                &self.config.toolchain_version,
                &self.config.target_architecture,
            )?;
            let (build, _) = self.repository.ensure_build(&key).await?;
            let commit_alias = CiHostname::for_ref(
                CiRefKind::Commit,
                &source_ref.commit_sha,
                &self.config.root_domain,
            )?;
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
            let stable_preview = self
                .repository
                .ensure_preview(
                    &repository_name,
                    &source_ref.commit_sha,
                    kind,
                    Some(&source_ref.name),
                    &stable_alias,
                    build.id,
                )
                .await?;
            let should_track_check_run = !(source_ref.kind == SourceRefKind::PullRequest
                && source_ref.closed
                && build.stage == CiBuildStage::Queued);
            if should_track_check_run
                && let Some(check_runs) = self.check_runs.as_ref()
                && self.repository.check_run_id(build.id).await?.is_none()
            {
                self.ensure_check_run(
                    check_runs.as_ref(),
                    build.id,
                    &source_ref,
                    &repository_name,
                    &commit_alias,
                    &stable_alias,
                )
                .await?;
            }
            let eager_pr = source_ref.kind == SourceRefKind::PullRequest && !source_ref.closed;
            // A stable branch alias may still serve an older successful
            // commit while the newly observed head is building. Once a player
            // has admitted that alias (anything beyond the untouched lobby),
            // keep advancing the latest candidate and its build-only Check Run.
            // This also retries a build whose lease expired while its prior
            // poller replica was unavailable.
            let admitted_branch = source_ref.kind == SourceRefKind::Branch
                && stable_preview.state != crate::ci::CiPreviewState::Lobby;
            if eager_pr {
                self.ensure_pr_head_build(
                    &source_ref,
                    &repository_name,
                    &source_ref.commit_sha,
                    &commit_alias,
                    &stable_alias,
                )
                .await?;
            } else if admitted_branch && let Some(builder) = self.builder.as_ref() {
                builder
                    .ensure_build(crate::ci::BuildRequest {
                        key: key.clone(),
                        source_repository: source_ref.source_repository.clone(),
                        source_archive: Vec::new(),
                    })
                    .await
                    .map_err(|error| anyhow::anyhow!("CI build failed: {error}"))?;
            }
        }
        self.reconcile_check_runs().await?;
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
        self.reconcile_check_runs().await?;
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
                commit_alias,
                stable_alias,
            )
            .await?;
        }
        if let Some(builder) = self.builder.as_ref() {
            builder
                .ensure_build(crate::ci::BuildRequest {
                    key,
                    source_repository: source_ref.source_repository.clone(),
                    source_archive: Vec::new(),
                })
                .await
                .map_err(|error| anyhow::anyhow!("PR head build failed: {error}"))?;
        }
        Ok(())
    }

    async fn reconcile_check_runs(&self) -> Result<()> {
        let Some(check_runs) = self.check_runs.as_ref() else {
            return Ok(());
        };
        for candidate in self
            .repository
            .check_run_reconciliation_candidates(128)
            .await?
        {
            let (build_log, expires_at, preview_hostnames) = if candidate.tombstone {
                (String::new(), None, Vec::new())
            } else {
                let (build_log, expires_at, preview_hostnames) = self
                    .repository
                    .build_diagnostics(candidate.build_id, &self.config.root_domain)
                    .await?;
                (build_log, Some(expires_at), preview_hostnames)
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
        commit_alias: &CiHostname,
        stable_alias: &CiHostname,
    ) -> Result<()> {
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
            .map(|base| format!("{base}/{}", commit_alias.value));
        let details = CheckRunDetails {
            commit_sha: source_ref.commit_sha.clone(),
            stage: CiBuildStage::Queued,
            repository: repository.to_owned(),
            artifact_digest: None,
            failure_stage: None,
            failure_message: None,
            build_log: String::new(),
            preview_hostnames: vec![commit_alias.value.clone(), stable_alias.value.clone()],
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
                self.repository
                    .release_check_run_claim(build_id, claim)
                    .await?;
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
                    let poll_error = PollError::Http {
                        status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
                        message: error.to_string(),
                    };
                    sleep(retry_delay(&poll_error, attempt)).await;
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }
    pub async fn run_forever(self, repository_id: Uuid, interval: Duration) -> Result<()> {
        let interval = interval.max(Duration::from_secs(1));
        loop {
            if let Err(error) = self.poll_with_retry(repository_id, 3).await {
                tracing::error!(?error, %repository_id, "CI source-control poll failed");
            }
            sleep(interval).await;
        }
    }
}
