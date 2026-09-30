use anyhow::Result;
use kurbis_hoster::{config::WorkerConfig, db, init_tracing, worker::runner::WorkerRunner};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("kurbis_hoster=info");
    let config = WorkerConfig::from_env()?;
    let pool = db::connect(&config.common.database_url).await?;
    db::migrate(&pool).await?;
    WorkerRunner::new(config, pool).await?.run().await
}
