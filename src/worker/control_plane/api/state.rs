use std::net::IpAddr;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    contracts::{
        lifecycle::{CommandResponse, ServerCommandRequest},
        worker::{
            WorkerAssignment, WorkerRecoveryReport, WorkerServerContext, WorkerServerState,
            WorkerSnapshotCacheHint, WorkerSnapshotReservation,
        },
    },
    model::{ObservedState, SnapshotReason, SnapshotRecord},
    pumpkin_control::WaitingPlayer,
    runtime_stats::RuntimeStats,
};

use super::super::{EmptyRequest, WorkerControl};

impl WorkerControl {
    pub async fn acquire_lease(&self, worker_id: Uuid, signal_address: IpAddr) -> Result<()> {
        let lease_id = Uuid::new_v4();
        let response: LeaseResponse = self
            .post_json(
                "/internal/worker/control/lease",
                &AcquireLeaseRequest {
                    lease_id,
                    signal_address,
                },
            )
            .await?;
        ensure!(
            response.worker_id == worker_id,
            "control plane assigned another worker identity"
        );
        self.set_lease_id(lease_id);
        Ok(())
    }

    pub async fn worker_servers(&self) -> Result<Vec<WorkerServerState>> {
        self.get_json("/internal/worker/control/servers").await
    }

    pub async fn server_context(
        &self,
        server_id: Uuid,
        assignment: WorkerAssignment,
    ) -> Result<WorkerServerContext> {
        self.get_json_with_assignment(
            &format!("/internal/worker/control/servers/{server_id}/context"),
            assignment,
        )
        .await
    }

    pub async fn sync_runtime_stats(&self, server_id: Uuid, stats: &RuntimeStats) -> Result<()> {
        stats.validate().map_err(anyhow::Error::msg)?;
        self.post_no_content(
            &format!("/internal/worker/control/servers/{server_id}/runtime-stats"),
            &RuntimeStatsSyncRequest {
                generation: stats.generation,
                stats,
            },
        )
        .await
    }

    pub async fn replace_players(
        &self,
        server_id: Uuid,
        generation: i64,
        players: &[WaitingPlayer],
    ) -> Result<()> {
        self.post_no_content(
            &format!("/internal/worker/control/servers/{server_id}/players"),
            &PlayersSyncRequest {
                generation,
                players,
            },
        )
        .await
    }

    pub async fn clear_players(&self, server_id: Uuid, generation: i64) -> Result<()> {
        self.replace_players(server_id, generation, &[]).await
    }

    pub async fn append_logs(
        &self,
        server_id: Uuid,
        generation: i64,
        lines: &[String],
    ) -> Result<()> {
        self.post_no_content(
            &format!("/internal/worker/control/servers/{server_id}/logs"),
            &LogSyncRequest { generation, lines },
        )
        .await
    }

    pub async fn mark_whitelist_applied(
        &self,
        server_id: Uuid,
        generation: i64,
        revision: i64,
    ) -> Result<()> {
        self.post_no_content(
            &format!("/internal/worker/control/servers/{server_id}/whitelist-applied"),
            &RevisionSyncRequest {
                generation,
                revision,
            },
        )
        .await
    }

    pub async fn mark_operators_applied(
        &self,
        server_id: Uuid,
        generation: i64,
        revision: i64,
    ) -> Result<()> {
        self.post_no_content(
            &format!("/internal/worker/control/servers/{server_id}/operators-applied"),
            &RevisionSyncRequest {
                generation,
                revision,
            },
        )
        .await
    }

    pub async fn reserve_snapshot(
        &self,
        server_id: Uuid,
        operation_id: Uuid,
        generation: i64,
        attempt: i32,
        reason: SnapshotReason,
    ) -> Result<WorkerSnapshotReservation> {
        self.post_json(
            &format!("/internal/worker/control/servers/{server_id}/snapshots/reserve"),
            &ReserveSnapshotRequest {
                operation_id,
                generation,
                attempt,
                reason,
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn publish_snapshot_metadata(
        &self,
        server_id: Uuid,
        operation_id: Uuid,
        generation: i64,
        attempt: i32,
        snapshot_generation: i64,
        manifest_key: &str,
        size_bytes: i64,
        checksum: &str,
    ) -> Result<()> {
        self.post_no_content(
            &format!("/internal/worker/control/servers/{server_id}/snapshots/{snapshot_generation}/metadata"),
            &SnapshotMetadataRequest { operation_id, generation, attempt, manifest_key, size_bytes, checksum },
        ).await
    }

    pub async fn snapshot_metadata(
        &self,
        server_id: Uuid,
        assignment: WorkerAssignment,
        snapshot_generation: i64,
    ) -> Result<SnapshotRecord> {
        self.get_json_with_assignment(
            &format!(
                "/internal/worker/control/servers/{server_id}/snapshots/{snapshot_generation}"
            ),
            assignment,
        )
        .await
    }

    pub async fn current_snapshot_metadata(
        &self,
        server_id: Uuid,
        assignment: WorkerAssignment,
    ) -> Result<Option<SnapshotRecord>> {
        self.get_json_with_assignment(
            &format!("/internal/worker/control/servers/{server_id}/snapshots/current"),
            assignment,
        )
        .await
    }

    pub async fn report_snapshot_cache_hint(
        &self,
        server_id: Uuid,
        assignment: WorkerAssignment,
        snapshot_generation: i64,
    ) -> Result<()> {
        self.post_no_content(
            &format!("/internal/worker/control/servers/{server_id}/snapshots/cache-hint"),
            &WorkerSnapshotCacheHint {
                snapshot_generation,
                assignment,
            },
        )
        .await
    }

    pub async fn delete_snapshot_manifests(
        &self,
        server_id: Uuid,
        operation_id: Uuid,
        generation: i64,
        attempt: i32,
    ) -> Result<()> {
        self.post_no_content(
            &format!("/internal/worker/control/servers/{server_id}/snapshots/delete-manifests"),
            &SnapshotDeleteRequest {
                operation_id,
                generation,
                attempt,
            },
        )
        .await
    }

    pub async fn export_is_running(
        &self,
        export_id: Uuid,
        generation: i64,
        attempt: i32,
    ) -> Result<bool> {
        self.get_json_with_assignment(
            &format!("/internal/worker/control/exports/{export_id}/running"),
            WorkerAssignment::Export {
                export_id,
                generation,
                attempt,
            },
        )
        .await
    }

    pub async fn retained_server_ids(&self) -> Result<Vec<Uuid>> {
        self.get_json("/internal/worker/control/retained-servers")
            .await
    }

    pub async fn command_stop(
        &self,
        server_id: Uuid,
        generation: i64,
        reason: &str,
    ) -> Result<ObservedState> {
        let response: CommandResponse = self
            .post_json(
                &format!("/api/v1/servers/{server_id}/commands"),
                &ServerCommandRequest::Stop {
                    generation: Some(generation),
                    reason: Some(reason.to_owned()),
                },
            )
            .await?;
        Ok(response.observed_state)
    }

    pub async fn command_snapshot(
        &self,
        server_id: Uuid,
        generation: i64,
        reason: SnapshotReason,
    ) -> Result<ObservedState> {
        let response: CommandResponse = self
            .post_json(
                &format!("/api/v1/servers/{server_id}/commands"),
                &ServerCommandRequest::Snapshot {
                    generation: Some(generation),
                    reason: Some(reason),
                },
            )
            .await?;
        Ok(response.observed_state)
    }

    pub async fn recover(&self) -> Result<WorkerRecoveryReport> {
        self.post_json("/internal/worker/control/recover", &EmptyRequest {})
            .await
    }

    pub async fn heartbeat(&self) -> Result<()> {
        self.post_no_content(
            "/internal/worker/control/heartbeat",
            &HeartbeatRequest {
                runtime_architecture: runtime_architecture(),
                supports_ci_previews: self.supports_ci_previews(),
            },
        )
        .await
    }
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct HeartbeatRequest {
    runtime_architecture: &'static str,
    supports_ci_previews: bool,
}

fn runtime_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64-linux",
        "aarch64" => "aarch64-linux",
        _ => "unknown",
    }
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct AcquireLeaseRequest {
    lease_id: Uuid,
    signal_address: IpAddr,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseResponse {
    worker_id: Uuid,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeStatsSyncRequest<'a> {
    generation: i64,
    stats: &'a RuntimeStats,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct PlayersSyncRequest<'a> {
    generation: i64,
    players: &'a [WaitingPlayer],
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct LogSyncRequest<'a> {
    generation: i64,
    lines: &'a [String],
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct RevisionSyncRequest {
    generation: i64,
    revision: i64,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ReserveSnapshotRequest {
    operation_id: Uuid,
    generation: i64,
    attempt: i32,
    reason: SnapshotReason,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct SnapshotMetadataRequest<'a> {
    operation_id: Uuid,
    generation: i64,
    attempt: i32,
    manifest_key: &'a str,
    size_bytes: i64,
    checksum: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct SnapshotDeleteRequest {
    operation_id: Uuid,
    generation: i64,
    attempt: i32,
}
