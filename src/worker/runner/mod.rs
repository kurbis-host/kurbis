use std::{collections::HashMap, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio::time::{Instant, sleep};
use uuid::Uuid;

mod heartbeat;
mod maintenance;
mod operations;
mod storage;

#[cfg(test)]
mod tests;

use heartbeat::{maintain_heartbeat, send_heartbeat};
use storage::{ensure_storage_limit, persistent_size};

use crate::{
    config::{RuntimeConfig, WorkerConfig},
    control_plane::lifecycle::{
        Lifecycle, claim_operation, complete_delete, complete_restore, complete_snapshot,
        complete_start, complete_stop, mark_runtime_lost, recover_running_operations,
        retry_operation, update_operation_payload, validate_delete,
    },
    model::{
        OperationKind, OperationRecord, PlacementRecord, SERVER_STORAGE_BYTES, SnapshotReason,
    },
    worker::{
        docker::DockerRuntime,
        idle::IdleTimer,
        lease::WorkerLease,
        panel_state::{clear_players, sync_players, sync_whitelist, write_whitelist_file},
        runtime::{ServerRuntime, native::NativeRuntime},
        snapshot::SnapshotManager,
        snapshot_retry::SnapshotSchedule,
        storage::StorageManager,
    },
};

pub struct WorkerRunner {
    worker_lease: Option<WorkerLease>,
    config: WorkerConfig,
    pool: PgPool,
    lifecycle: Lifecycle,
    runtime: Box<dyn ServerRuntime>,
    snapshots: SnapshotManager,
    storage: StorageManager,
    idle_since: HashMap<Uuid, IdleTimer>,
    last_snapshot: HashMap<Uuid, SnapshotSchedule>,
    last_log_read: HashMap<Uuid, DateTime<Utc>>,
    last_local_gc: Instant,
    last_object_gc: Instant,
}

impl WorkerRunner {
    pub async fn new(config: WorkerConfig, pool: PgPool) -> Result<Self> {
        let worker_lease = WorkerLease::acquire(&pool, config.id).await?;
        let recovered = recover_running_operations(&pool, config.id).await?;
        if recovered > 0 {
            tracing::warn!(worker_id = %config.id, recovered, "recovered interrupted worker operations");
        }
        tokio::fs::create_dir_all(config.root.join("servers")).await?;
        let snapshots = SnapshotManager::new(pool.clone(), &config.object_store).await?;
        let runtime: Box<dyn ServerRuntime> = match &config.runtime {
            RuntimeConfig::Docker { image } => Box::new(DockerRuntime::new(
                image.clone(),
                config.host_control_token.clone(),
            )),
            RuntimeConfig::Native { binary } => Box::new(
                NativeRuntime::new(
                    binary.clone(),
                    config.root.clone(),
                    config.id,
                    config.host_control_token.clone(),
                )
                .await?,
            ),
        };
        let storage = StorageManager::new(config.root.clone(), config.storage_mode);
        Ok(Self {
            worker_lease: Some(worker_lease),
            lifecycle: Lifecycle::new(pool.clone()),
            config,
            pool,
            runtime,
            snapshots,
            storage,
            idle_since: HashMap::new(),
            last_snapshot: HashMap::new(),
            last_log_read: HashMap::new(),
            last_local_gc: Instant::now(),
            last_object_gc: Instant::now(),
        })
    }

    pub async fn run(mut self) -> Result<()> {
        self.gc_stale_local_storage().await?;
        self.heartbeat().await?;
        let pool = self.pool.clone();
        let heartbeat_config = self.config.clone();
        let lease = self
            .worker_lease
            .take()
            .context("worker lease already consumed")?;
        tokio::select! {
            result = lease.monitor() => result,
            result = maintain_heartbeat(pool, heartbeat_config) => result,
            result = self.run_operations() => result,
        }
    }
}
