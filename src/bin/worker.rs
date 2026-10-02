use anyhow::Result;
use kurbis_hoster::{config::WorkerConfig, init_tracing, worker::runner::WorkerRunner};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("kurbis_hoster=info");
    let config = WorkerConfig::from_env()?;
    WorkerRunner::new(config).await?.run().await
}
