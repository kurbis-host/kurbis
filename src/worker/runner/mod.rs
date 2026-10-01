use std::{collections::HashMap, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio::time::{Instant, sleep};
use uuid::Uuid;

mod exports;
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
    model::{
        OperationKind, OperationRecord, PlacementRecord, SERVER_STORAGE_BYTES, SnapshotReason,
    },
    worker::{
        control_plane::WorkerControl,
        docker::DockerRuntime,
        idle::IdleTimer,
        lease::WorkerLease,
        panel_state::{
            clear_players, sync_operators, sync_players, sync_whitelist, write_operator_file,
            write_whitelist_file,
        },
        runtime::{ServerRuntime, native::NativeRuntime},
        server_access::ServerAccess,
        snapshot::{SnapshotManager, WorkerObjectClient},
        snapshot_retry::SnapshotSchedule,
        storage::StorageManager,
    },
};

pub struct WorkerRunner {
    worker_lease: Option<WorkerLease>,
    config: WorkerConfig,
    pool: PgPool,
    control_plane: WorkerControl,
    runtime: Box<dyn ServerRuntime>,
    server_access: ServerAccess,
    snapshots: SnapshotManager,
    storage: StorageManager,
    idle_since: HashMap<Uuid, IdleTimer>,
    last_snapshot: HashMap<Uuid, SnapshotSchedule>,
    last_log_read: HashMap<Uuid, DateTime<Utc>>,
    last_local_gc: Instant,
    world_export_task: Option<tokio::task::JoinHandle<()>>,
}

impl WorkerRunner {
    pub async fn new(config: WorkerConfig, pool: PgPool) -> Result<Self> {
        let database_worker_id: Option<Uuid> =
            sqlx::query_scalar("SELECT worker_auth.current_worker_id()")
                .fetch_one(&pool)
                .await?;
        anyhow::ensure!(
            database_worker_id == Some(config.id),
            "WORKER_ID does not match the authenticated PostgreSQL principal"
        );
        let registered_signal_address: String = sqlx::query_scalar(
            "SELECT signal_address FROM workers
             WHERE id = worker_auth.current_worker_id() AND id = $1",
        )
        .bind(config.id)
        .fetch_one(&pool)
        .await?;
        anyhow::ensure!(
            crate::model::is_private_signal_address(&registered_signal_address)
                && registered_signal_address.parse::<std::net::IpAddr>()? == config.signal_address,
            "WORKER_SIGNAL_ADDRESS does not match the registered worker configuration"
        );
        let public_address: std::net::IpAddr = config
            .address
            .parse()
            .context("WORKER_ADDRESS must be an IP address for Bedrock ICE")?;
        let signal_address = config.signal_address;
        let worker_lease = WorkerLease::acquire(&pool, config.id).await?;
        let control_plane = WorkerControl::new(
            &config.control_plane_url,
            &config.api_token,
            config.allow_insecure_local_control_plane_http,
        )?;
        let recovered = control_plane.recover().await?;
        if recovered.operations > 0 || recovered.exports > 0 {
            tracing::warn!(
                worker_id = %config.id,
                operations = recovered.operations,
                exports = recovered.exports,
                "recovered interrupted worker work"
            );
        }
        tokio::fs::create_dir_all(config.root.join("servers")).await?;
        let object_client = WorkerObjectClient::new(
            &config.control_plane_url,
            &config.api_token,
            config.allow_insecure_local_control_plane_http,
        )?;
        let snapshots = SnapshotManager::new_worker(pool.clone(), object_client);
        let runtime: Box<dyn ServerRuntime> = match &config.runtime {
            RuntimeConfig::Docker { image } => Box::new(DockerRuntime::new(
                image.clone(),
                config.host_control_token.clone(),
                public_address,
                signal_address,
            )),
            RuntimeConfig::Native { binary } => Box::new(
                NativeRuntime::new(
                    binary.clone(),
                    config.root.clone(),
                    config.id,
                    config.host_control_token.clone(),
                    public_address,
                    signal_address,
                )
                .await?,
            ),
        };
        let storage = StorageManager::new(config.root.clone(), config.storage_mode);
        let server_access = ServerAccess::new(&config)?;
        Ok(Self {
            worker_lease: Some(worker_lease),
            control_plane,
            config,
            pool,
            runtime,
            server_access,
            snapshots,
            storage,
            idle_since: HashMap::new(),
            last_snapshot: HashMap::new(),
            last_log_read: HashMap::new(),
            last_local_gc: Instant::now(),
            world_export_task: None,
        })
    }

    pub async fn run(mut self) -> Result<()> {
        self.gc_stale_local_storage().await?;
        self.heartbeat().await?;
        let control_plane = self.control_plane.clone();
        let lease = self
            .worker_lease
            .take()
            .context("worker lease already consumed")?;
        tokio::select! {
            result = lease.monitor() => result,
            result = maintain_heartbeat(control_plane) => result,
            result = self.run_operations() => result,
        }
    }
}
