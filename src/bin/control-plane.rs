use std::sync::Arc;

use anyhow::Result;
use kurbis_hoster::{
    ci::{
        ArtifactCache, BuildCoordinator, CI_RUNTIME_IDLE_SECONDS, CiPoller, CiPollerConfig,
        CiPreviewRepository, CiPreviewRuntimeRepository, GitHubCheckRunClient, GitHubExactSource,
        GitHubSourceControl, ObjectArtifactCache, POLL_INTERVAL_SECONDS, docker_pipeline,
    },
    config::{CiConfig, ControlPlaneConfig, ObjectStoreConfig},
    control_plane::{
        api::{AppState, router, spawn_queue_drain},
        lifecycle::Lifecycle,
        maintenance::{WorkerMaintenance, spawn_worker_maintenance},
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
        let check_runs =
            std::sync::Arc::new(GitHubCheckRunClient::new(client, ci_config.github_pat)?);
        let artifacts =
            std::sync::Arc::new(ObjectArtifactCache::new(pool.clone(), object_store.clone()));
        let pipeline = std::sync::Arc::new(docker_pipeline(
            CiPreviewRepository::new(pool.clone()),
            std::sync::Arc::new(source.clone()),
            artifacts.clone(),
            ci_config.builder_image.clone(),
            ci_config.builder_docker_host.clone(),
            ci_config.toolchain_version.clone(),
            ci_config.plugin_source.clone(),
            Some(check_runs.clone()),
        )?);
        ci_build_coordinator = Some(pipeline.clone());
        let expiry_repository = ci_repository.clone();
        let runtime_repository = CiPreviewRuntimeRepository::new(pool.clone());
        let artifact_cache = ObjectArtifactCache::new(pool.clone(), object_store.clone());
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
                if let Err(error) = runtime_repository
                    .claim_idle(chrono::Duration::seconds(CI_RUNTIME_IDLE_SECONDS), 128)
                    .await
                {
                    tracing::error!(?error, "failed to queue idle CI runtime shutdown");
                }
                if let Err(error) = artifact_cache.delete_expired(now).await {
                    tracing::error!(?error, "failed to delete expired CI artifacts");
                }
            }
        });
        tokio::spawn(
            CiPoller::new(ci_repository, std::sync::Arc::new(source))
                .with_check_runs(check_runs)
                .with_config(CiPollerConfig {
                    root_domain: kurbis_hoster::ci::CI_ROOT_DOMAIN.into(),
                    patch_fingerprint: ci_config.patch_fingerprint.clone(),
                    toolchain_version: ci_config.toolchain_version.clone(),
                    target_architecture: ci_config.target_architecture.clone(),
                })
                .with_builder(pipeline)
                .run_forever(
                    watched.id,
                    std::time::Duration::from_secs(POLL_INTERVAL_SECONDS),
                ),
        );
    }
    let listen = config.listen;
    let state = Arc::new(AppState {
        pool,
        ci_build_coordinator,
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
