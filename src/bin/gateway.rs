use anyhow::Result;
use kurbis_hoster::{
    config::GatewayConfig,
    db,
    gateway::{bedrock, java, routes::RouteCache, waiting},
    init_tracing,
};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("kurbis_hoster=info");
    let config = GatewayConfig::from_env()?;
    let pool = db::connect(&config.common.database_url).await?;
    let routes = RouteCache::new(pool, config.root_domain.clone());
    let listener = routes.invalidation_listener().await?;
    let invalidation_routes = routes.clone();
    tokio::try_join!(
        java::run(config.clone(), routes.clone()),
        bedrock::run(config.clone()),
        waiting::run(config, routes),
        invalidation_routes.listen_for_invalidations(listener)
    )?;
    Ok(())
}
