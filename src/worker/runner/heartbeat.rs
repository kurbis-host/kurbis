use anyhow::Result;
use tokio::time::{Duration, sleep};

use crate::worker::control_plane::WorkerControl;

pub(super) async fn maintain_heartbeat(control_plane: WorkerControl) -> Result<()> {
    loop {
        send_heartbeat(&control_plane).await?;
        sleep(Duration::from_secs(10)).await;
    }
}

pub(super) async fn send_heartbeat(control_plane: &WorkerControl) -> Result<()> {
    control_plane.heartbeat().await
}
