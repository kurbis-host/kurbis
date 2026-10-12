use std::{collections::HashMap, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
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
    contracts::worker::WorkerServerContext,
    model::{
        DesiredState, ObservedState, OperationKind, OperationRecord, PlacementRecord,
        SERVER_STORAGE_BYTES, SnapshotReason,
    },
    worker::{
        ci_runtime::CiPreviewWorker,
        control_plane::{OperationRetryOutcome, WorkerControl},
        docker::DockerRuntime,
        idle::IdleTimer,
        panel_state::{
            clear_players, sync_operators, sync_players, sync_whitelist, write_operator_file,
            write_whitelist_file,
        },
        runtime::{ServerRuntime, native::NativeRuntime},
        runtime_api_relay::WorkerRuntimeApiRelay,
        server_access::ServerAccess,
        snapshot::{SnapshotManager, WorkerObjectClient},
        snapshot_retry::SnapshotSchedule,
        storage::StorageManager,
    },
};

pub struct WorkerRunner {
    config: WorkerConfig,
    control_plane: WorkerControl,
    runtime: Box<dyn ServerRuntime>,
    ci_runtime: Option<CiPreviewWorker>,
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
    pub async fn new(config: WorkerConfig) -> Result<Self> {
        let public_address: std::net::IpAddr = config
            .address
            .parse()
            .context("WORKER_ADDRESS must be an IP address for Bedrock ICE")?;
        let signal_address = config.signal_address;
        let control_plane = WorkerControl::new(
            &config.control_plane_url,
            &config.api_token,
            config.allow_insecure_local_control_plane_http,
        )?
        .with_ci_preview_support(matches!(&config.runtime, RuntimeConfig::Docker { .. }));
        tokio::fs::create_dir_all(config.root.join("servers")).await?;
        let object_client = WorkerObjectClient::from_control_plane(control_plane.clone());
        let snapshots = SnapshotManager::new_worker(object_client);
        let runtime_api_relay = match &config.runtime {
            RuntimeConfig::Docker { .. } => {
                let socket = crate::worker::sandbox::rootless_docker_socket().await?;
                Some(std::sync::Arc::new(
                    WorkerRuntimeApiRelay::start(
                        &socket,
                        &config.control_plane_url,
                        config.allow_insecure_local_control_plane_http,
                    )
                    .await?,
                ))
            }
            RuntimeConfig::Native { .. } => None,
        };
        let runtime: Box<dyn ServerRuntime> = match &config.runtime {
            RuntimeConfig::Docker { image } => {
                let runtime = DockerRuntime::new(
                    image.clone(),
                    config.host_control_token.clone(),
                    config.root.clone(),
                    public_address,
                    signal_address,
                    config
                        .gateway_address
                        .context("Docker workload ingress needs WORKER_GATEWAY_ADDRESS")?,
                    config
                        .gateway_bedrock_ip
                        .context("Docker workload ingress needs WORKER_GATEWAY_BEDROCK_IP")?,
                    config
                        .gateway_bedrock_port
                        .context("Docker workload ingress needs WORKER_GATEWAY_BEDROCK_PORT")?,
                    config.runtime_api_proxy_image.clone(),
                    config.runtime_api_proxy_upstream.clone(),
                    config.runtime_api_proxy_egress_network.clone(),
                    config.allow_insecure_local_control_plane_http,
                    runtime_api_relay.clone().expect("Docker runtime relay"),
                )?;
                runtime.verify_sandbox().await?;
                Box::new(runtime)
            }
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
        let ci_runtime = match &config.runtime {
            RuntimeConfig::Docker { image } => Some(CiPreviewWorker::new(
                control_plane.clone(),
                config.root.clone(),
                image.clone(),
                config.id,
                config.host_control_token.clone(),
                config
                    .address
                    .parse()
                    .context("WORKER_ADDRESS must be an IP address for CI preview routing")?,
                config.signal_address,
                config
                    .gateway_address
                    .context("CI preview ingress needs WORKER_GATEWAY_ADDRESS")?,
                config
                    .gateway_bedrock_ip
                    .context("CI preview ingress needs WORKER_GATEWAY_BEDROCK_IP")?,
                config
                    .gateway_bedrock_port
                    .context("CI preview ingress needs WORKER_GATEWAY_BEDROCK_PORT")?,
                config.storage_mode,
                config.runtime_api_proxy_image.clone(),
                config.runtime_api_proxy_upstream.clone(),
                config.runtime_api_proxy_egress_network.clone(),
                config.allow_insecure_local_control_plane_http,
                runtime_api_relay.clone().expect("Docker CI runtime relay"),
            )),
            RuntimeConfig::Native { .. } => None,
        };
        control_plane
            .acquire_lease(config.id, signal_address)
            .await?;
        let recovered = control_plane.recover().await?;
        if recovered.operations > 0 || recovered.exports > 0 {
            tracing::warn!(
                worker_id = %config.id,
                operations = recovered.operations,
                exports = recovered.exports,
                "recovered interrupted worker work"
            );
        }
        let storage = StorageManager::new(config.root.clone(), config.storage_mode);
        let server_access = ServerAccess::new(&config, control_plane.clone())?;
        Ok(Self {
            control_plane,
            config,
            runtime,
            ci_runtime,
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
        tokio::select! {
            result = maintain_heartbeat(control_plane) => result,
            result = self.run_operations() => result,
        }
    }
}
