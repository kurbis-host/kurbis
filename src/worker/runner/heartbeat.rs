use anyhow::Result;
use sqlx::PgPool;
use tokio::time::{Duration, sleep};

use crate::{
    config::WorkerConfig,
    control_plane::lifecycle::{healthy_worker_template, heartbeat_worker},
};

pub(super) async fn maintain_heartbeat(pool: PgPool, config: WorkerConfig) -> Result<()> {
    loop {
        send_heartbeat(&pool, &config).await?;
        sleep(Duration::from_secs(10)).await;
    }
}

pub(super) async fn send_heartbeat(pool: &PgPool, config: &WorkerConfig) -> Result<()> {
    let worker = healthy_worker_template(
        config.id,
        config.name.clone(),
        config.address.clone(),
        config.total_memory_bytes,
        config.total_disk_bytes,
        config.total_cpu_millis,
        config.port_min,
        config.port_max,
    );
    heartbeat_worker(pool, &worker).await
}
