use std::sync::Arc;

use anyhow::Result;
use kurbis_hoster::{
    config::{ControlPlaneConfig, ObjectStoreConfig},
    control_plane::{
        api::{AppState, router, spawn_queue_drain},
        lifecycle::Lifecycle,
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
    let listen = config.listen;
    let state = Arc::new(AppState {
        pool,
        lifecycle,
        config,
        object_store,
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()?,
    });
    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!(%listen, "control plane listening");
    axum::serve(listener, router(state)).await?;
    Ok(())
}
