use std::sync::Arc;

use anyhow::Result;
use kurbis_hoster::{
    ci::{
        ArtifactCache, BuildCoordinator, CI_RUNTIME_IDLE_SECONDS, CiBuildIdentity, CiPoller,
        CiPollerConfig, CiPreviewRepository, CiPreviewRuntimeRepository, GitHubCheckRunClient,
        GitHubExactSource, GitHubSourceControl, ObjectArtifactCache, POLL_INTERVAL_SECONDS,
        bubblewrap_pipeline, docker_pipeline,
    },
    config::{CiBuilderBackend, CiConfig, ControlPlaneConfig, ObjectStoreConfig},
    control_plane::{
        api::{AppState, router, spawn_queue_drain},
        lifecycle::Lifecycle,
        maintenance::{
            WorkerMaintenance, cleanup_stale_ci_preview_deletions, spawn_worker_maintenance,
        },
    },
    db, init_tracing,
};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("kurbis_hoster=info,tower_http=info");
    let config = ControlPlaneConfig::from_env()?;
    let object_store = ObjectStoreConfig::from_env()?.build().await?;
    let pool = db::connect(&config.common.database_url).await?;
    db::migrate(&pool).await?;
    let lifecycle = Lifecycle::new(pool.clone());
    spawn_queue_drain(lifecycle.clone());
    spawn_worker_maintenance(WorkerMaintenance::new(pool.clone(), object_store.clone()));
    let mut ci_build_coordinator: Option<Arc<dyn BuildCoordinator>> = None;
    let mut ci_build_identity: Option<CiBuildIdentity> = None;
    if let Some(ci_config) = CiConfig::from_env()? {
        let ci_repository = CiPreviewRepository::new(pool.clone());
        let watched = ci_repository
            .register_repository(&ci_config.repository, &ci_config.default_branch)
            .await?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()?;
        let github_polling =
            GitHubSourceControl::new(client.clone(), ci_config.github_pat.clone())?;
        let source = GitHubExactSource::new(
            std::sync::Arc::new(github_polling),
            ci_config.github_pat.clone(),
        )?;
        let public_status_base_url = format!(
            "{}/ci/previews",
            config.public_base_url.trim_end_matches('/')
        );
        let check_runs = std::sync::Arc::new(
            GitHubCheckRunClient::new(client, ci_config.github_pat)?
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
            };
        ci_build_coordinator = Some(pipeline.clone());
        ci_build_identity = Some(CiBuildIdentity {
            repository: ci_config.repository.clone(),
            patch_fingerprint: ci_config.patch_fingerprint.clone(),
            toolchain_version: effective_toolchain_version.clone(),
            target_architecture: ci_config.target_architecture.clone(),
        });
        let expiry_repository = ci_repository.clone();
        let runtime_repository = CiPreviewRuntimeRepository::new(pool.clone());
        let artifact_cache = ObjectArtifactCache::new(pool.clone(), object_store.clone());
        let ci_runtime_object_store = object_store.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let now = chrono::Utc::now();
                if let Err(error) = expiry_repository.expire(now).await {
                    tracing::error!(?error, "failed to expire CI preview metadata");
                }
                // Claiming these rows is the control-plane-to-worker lifecycle
                // signal. Assigned workers perform graceful idle stops or
                // generation-fenced world deletion during their next reconcile.
                if let Err(error) = runtime_repository.claim_expired(now, 128).await {
                    tracing::error!(?error, "failed to queue expired CI runtime cleanup");
                }
                if let Err(error) = cleanup_stale_ci_preview_deletions(
                    &runtime_repository,
                    ci_runtime_object_store.clone(),
                    128,
                )
                .await
                {
                    tracing::error!(
                        ?error,
                        "failed to reclaim expired CI worlds from stale workers"
                    );
                }
                // Rehome checkpointed runtimes before the idle claimant can
                // transition a stale worker's placement to `stopping`.
                if let Err(error) = runtime_repository.reconcile_stale_workers(128).await {
                    tracing::error!(?error, "failed to reconcile stale CI preview workers");
                }
                if let Err(error) = runtime_repository
                    .claim_idle(chrono::Duration::seconds(CI_RUNTIME_IDLE_SECONDS), 128)
                    .await
                {
                    tracing::error!(?error, "failed to queue idle CI runtime shutdown");
                }
                if let Err(error) = runtime_repository.request_due_world_snapshots(128).await {
                    tracing::error!(?error, "failed to request periodic CI world snapshots");
                }
                if let Err(error) = artifact_cache.delete_expired(now).await {
                    tracing::error!(?error, "failed to delete expired CI artifacts");
                }
            }
        });
        let ci_poller = CiPoller::new(ci_repository, std::sync::Arc::new(source))
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
            tokio::spawn(async move {
                loop {
                    if let Err(error) = webhook_poller.drain_github_webhooks_once().await {
                        tracing::error!(
                            ?error,
                            "failed to process queued GitHub PR webhook deliveries"
                        );
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            });
        }
        tokio::spawn(ci_poller.run_forever(
            watched.id,
            std::time::Duration::from_secs(POLL_INTERVAL_SECONDS),
        ));
    }
    let listen = config.listen;
    let state = Arc::new(AppState {
        pool,
        ci_build_coordinator,
        ci_build_identity,
        lifecycle,
        config,
        object_store,
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
    });
    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!(%listen, "control plane listening");
    axum::serve(listener, router(state)).await?;
    Ok(())
}
