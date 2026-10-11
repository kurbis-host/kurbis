use std::sync::Arc;

use anyhow::{Error, Result};
use kurbis_hoster::{
    ci::{
        BuildCoordinator, CiBuildIdentity, CiPoller, CiPollerConfig, CiPreviewRepository,
        GitHubCheckRunClient, GitHubExactSource, GitHubRateLimitConfig, GitHubRateLimitGuard,
        GitHubSourceControl, ObjectArtifactCache, POLL_INTERVAL_SECONDS, PollError,
        bubblewrap_pipeline, docker_pipeline, remote_bubblewrap_pipeline_ready,
    },
    config::{CiBuilderBackend, CiConfig, ControlPlaneConfig, ObjectStoreConfig},
    control_plane::supervision::{Supervisor, SupervisorConfig, SupervisorRegistry, WorkerError},
    control_plane::{
        api::{AppState, router},
        lifecycle::Lifecycle,
        maintenance::{CiPreviewMaintenance, WorkerMaintenance},
    },
    db, init_tracing,
};

fn classify_ci_poll_failure(error: Error) -> WorkerError {
    let Some(poll_error) = error.downcast_ref::<PollError>() else {
        return WorkerError::retryable(error);
    };
    match poll_error {
        PollError::InvalidResponse(_) => WorkerError::fatal(error),
        PollError::Http { status, .. }
            if *status == reqwest::StatusCode::UNAUTHORIZED
                || *status == reqwest::StatusCode::FORBIDDEN
                || *status == reqwest::StatusCode::NOT_FOUND =>
        {
            WorkerError::fatal(error)
        }
        PollError::Http { .. } | PollError::RateLimited { .. } | PollError::Transport(_) => {
            WorkerError::retryable(error)
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("kurbis_hoster=info,tower_http=info");
    let config = ControlPlaneConfig::from_env()?;
    let object_store = ObjectStoreConfig::from_env()?.build().await?;
    let pool = db::connect(&config.common.database_url).await?;
    db::migrate(&pool).await?;
    let lifecycle = Lifecycle::new(pool.clone());
    let supervision = SupervisorRegistry::new();
    let mut workers = Supervisor::new(supervision.clone());
    let queue_lifecycle = lifecycle.clone();
    workers.spawn(
        "start-queue",
        SupervisorConfig::required(std::time::Duration::from_secs(2)),
        move || {
            let lifecycle = queue_lifecycle.clone();
            async move {
                lifecycle
                    .drain_queue_once()
                    .await
                    .map(|_| ())
                    .map_err(WorkerError::retryable)
            }
        },
    );
    let maintenance = WorkerMaintenance::new(pool.clone(), object_store.clone());
    workers.spawn(
        "worker-maintenance",
        SupervisorConfig::required(std::time::Duration::from_secs(30)),
        move || {
            let maintenance = maintenance.clone();
            async move {
                match maintenance.run_once().await {
                    Ok(report) => {
                        if report.operations_assigned > 0
                            || report.exports_assigned > 0
                            || report.exports_requeued > 0
                            || report.exports_expired > 0
                            || report.garbage_collection.chunks_deleted > 0
                            || report.garbage_collection.manifests_deleted > 0
                            || report.garbage_collection.exports_deleted > 0
                            || report.garbage_collection.staging_deleted > 0
                            || report.ci_world_snapshot_gc_completed > 0
                            || report.github_webhook_processed_gc_deleted > 0
                        {
                            tracing::info!(?report, "worker maintenance completed");
                        }
                        Ok(())
                    }
                    Err(error) => Err(WorkerError::retryable(error)),
                }
            }
        },
    );
    let mut ci_build_coordinator: Option<Arc<dyn BuildCoordinator>> = None;
    let mut ci_build_identity: Option<CiBuildIdentity> = None;
    let mut ci_main_only = false;
    let mut ci_default_branch: Option<String> = None;
    if let Some(ci_config) = CiConfig::from_env()? {
        let ci_repository = CiPreviewRepository::new(pool.clone());
        let watched = ci_repository
            .register_repository(&ci_config.repository, &ci_config.default_branch)
            .await?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()?;
        let github_rate_limit = Arc::new(
            GitHubRateLimitGuard::new(GitHubRateLimitConfig {
                minimum_interval: ci_config.github_min_request_interval,
                max_backoff: ci_config.github_rate_limit_max_backoff,
                jitter: ci_config.github_rate_limit_jitter,
            })
            .with_store(pool.clone()),
        );
        let github_polling =
            GitHubSourceControl::new(client.clone(), ci_config.github_pat.clone())?
                .with_api_base_url(ci_config.github_api_base_url.clone())
                .with_rate_limit_guard(github_rate_limit.clone());
        let source = GitHubExactSource::with_api_base_url(
            std::sync::Arc::new(github_polling),
            ci_config.github_pat.clone(),
            &ci_config.github_api_base_url,
        )?;
        let public_status_base_url = format!(
            "{}/ci/previews",
            config.public_base_url.trim_end_matches('/')
        );
        let check_runs = std::sync::Arc::new(
            GitHubCheckRunClient::new(client, ci_config.github_pat)?
                .with_api_base_url(ci_config.github_api_base_url.clone())
                .with_rate_limit_guard(github_rate_limit)
                .with_details_base_url(public_status_base_url.clone())?,
        );
        let artifacts =
            std::sync::Arc::new(ObjectArtifactCache::new(pool.clone(), object_store.clone()));
        let (pipeline, effective_toolchain_version): (Arc<dyn BuildCoordinator>, String) =
            match &ci_config.builder_backend {
                CiBuilderBackend::Docker => {
                    let pipeline = docker_pipeline(
                        CiPreviewRepository::new(pool.clone()),
                        Arc::new(source.clone()),
                        artifacts.clone(),
                        ci_config.builder_image.clone(),
                        ci_config.builder_docker_host.clone(),
                        ci_config.toolchain_version.clone(),
                        ci_config.build_concurrency,
                        ci_config.plugin_source.clone(),
                        Some(check_runs.clone()),
                    )?
                    .with_public_status_base_url(public_status_base_url.clone());
                    (Arc::new(pipeline), ci_config.toolchain_version.clone())
                }
                CiBuilderBackend::Bubblewrap {
                    executable,
                    rootfs,
                    toolchain,
                    workspace_root,
                    source_preparer,
                } => {
                    let (pipeline, identity) = bubblewrap_pipeline(
                        CiPreviewRepository::new(pool.clone()),
                        Arc::new(source.clone()),
                        artifacts.clone(),
                        executable.clone(),
                        rootfs.clone(),
                        toolchain.clone(),
                        workspace_root.clone(),
                        source_preparer.clone(),
                        ci_config.toolchain_version.clone(),
                        ci_config.plugin_source.clone(),
                        ci_config.build_concurrency,
                        Some(check_runs.clone()),
                    )?;
                    (
                        Arc::new(
                            pipeline.with_public_status_base_url(public_status_base_url.clone()),
                        ),
                        identity,
                    )
                }
                CiBuilderBackend::BubblewrapSsh {
                    target,
                    runner,
                    executable,
                    rootfs,
                    toolchain,
                    workspace_root,
                    source_preparer,
                } => {
                    let (pipeline, identity) = remote_bubblewrap_pipeline_ready(
                        CiPreviewRepository::new(pool.clone()),
                        Arc::new(source.clone()),
                        artifacts.clone(),
                        target.clone(),
                        runner.clone(),
                        executable.clone(),
                        rootfs.clone(),
                        toolchain.clone(),
                        workspace_root.clone(),
                        source_preparer.clone(),
                        ci_config.toolchain_version.clone(),
                        ci_config.plugin_source.clone(),
                        ci_config.build_concurrency,
                        Some(check_runs.clone()),
                    )
                    .await?;
                    (
                        Arc::new(
                            pipeline.with_public_status_base_url(public_status_base_url.clone()),
                        ),
                        identity,
                    )
                }
            };
        ci_build_coordinator = Some(pipeline.clone());
        ci_main_only = true;
        ci_default_branch = Some(ci_config.default_branch.clone());
        ci_build_identity = Some(CiBuildIdentity {
            repository: ci_config.repository.clone(),
            patch_fingerprint: ci_config.patch_fingerprint.clone(),
            toolchain_version: effective_toolchain_version.clone(),
            target_architecture: ci_config.target_architecture.clone(),
        });
        let ci_maintenance = CiPreviewMaintenance::new(pool.clone(), object_store.clone());
        workers.spawn(
            "ci-preview-maintenance",
            SupervisorConfig::required(std::time::Duration::from_secs(60)),
            move || {
                let maintenance = ci_maintenance.clone();
                async move { maintenance.run_once().await.map_err(WorkerError::retryable) }
            },
        );
        let ci_poller = CiPoller::new(ci_repository, std::sync::Arc::new(source))
            .default_branch_only()
            .with_check_runs(check_runs)
            .with_config(CiPollerConfig {
                root_domain: kurbis_hoster::ci::CI_ROOT_DOMAIN.into(),
                patch_fingerprint: ci_config.patch_fingerprint.clone(),
                toolchain_version: effective_toolchain_version,
                target_architecture: ci_config.target_architecture.clone(),
                public_status_base_url: Some(public_status_base_url),
            })
            .with_builder(pipeline);
        // Polling is the active CI event source. Keep the signed webhook
        // implementation available for an explicit opt-in, but do not start
        // its delivery consumer merely because a PAT is configured.
        if config.ci_github_webhook_enabled {
            let webhook_poller = ci_poller.clone();
            workers.spawn(
                "ci-github-webhook",
                SupervisorConfig::optional(std::time::Duration::from_secs(2)),
                move || {
                    let webhook_poller = webhook_poller.clone();
                    async move {
                        webhook_poller
                            .drain_github_webhooks_once()
                            .await
                            .map_err(WorkerError::retryable)
                    }
                },
            );
        }
        let repository_id = watched.id;
        workers.spawn(
            format!("ci-poller:{repository_id}"),
            SupervisorConfig::required(std::time::Duration::from_secs(POLL_INTERVAL_SECONDS)),
            move || {
                let poller = ci_poller.clone();
                async move {
                    poller
                        .poll_with_retry(repository_id, 3)
                        .await
                        .map_err(classify_ci_poll_failure)
                }
            },
        );
    }
    let listen = config.listen;
    let state = Arc::new(AppState {
        pool,
        ci_build_coordinator,
        ci_build_identity,
        ci_main_only,
        ci_default_branch,
        lifecycle,
        config,
        object_store,
        supervision,
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
    });
    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!(%listen, "control plane listening");
    let server = axum::serve(listener, router(state)).into_future();
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result?,
        result = workers.run() => result?,
    }
    Ok(())
}
