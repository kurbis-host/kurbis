use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

macro_rules! lifecycle_bail {
    (not_found: $($message:tt)*) => {
        return Err(::anyhow::Error::new(
            $crate::control_plane::lifecycle::LifecycleFailure::NotFound(format!($($message)*)),
        ))
    };
    (forbidden: $($message:tt)*) => {
        return Err(::anyhow::Error::new(
            $crate::control_plane::lifecycle::LifecycleFailure::Forbidden(format!($($message)*)),
        ))
    };
    (invalid: $($message:tt)*) => {
        return Err(::anyhow::Error::new(
            $crate::control_plane::lifecycle::LifecycleFailure::Invalid(format!($($message)*)),
        ))
    };
    (unavailable: $($message:tt)*) => {
        return Err(::anyhow::Error::new(
            $crate::control_plane::lifecycle::LifecycleFailure::Unavailable(format!($($message)*)),
        ))
    };
    ($($message:tt)*) => {
        return Err(::anyhow::Error::new(
            $crate::control_plane::lifecycle::LifecycleFailure::Conflict(format!($($message)*)),
        ))
    };
}
pub(crate) use lifecycle_bail as bail;

use crate::contracts::{
    lifecycle::ResetMode,
    worker::{OperationPayload, OperationStage},
};
use crate::model::{DesiredState, ObservedState, OperationKind, OperationStatus, ServerRecord};

mod commands;
mod operations;
mod placement;

use commands::authorize_actor;
pub use commands::{LifecycleActor, LifecycleCommand, LifecycleCommandError, LifecycleFailure};

pub use operations::{
    begin_snapshot_retry, claim_operation, complete_delete, complete_reset, complete_restore,
    complete_snapshot, complete_start, complete_stop, deactivate_placement_after_stop,
    defer_snapshot_operation, fail_operation, recover_running_operations, retry_operation,
    update_operation_payload, validate_delete,
};
use placement::{
    enqueue_delete, lock_placement, lock_server, lock_server_optional, place_and_enqueue_start,
    place_and_enqueue_worker_operation, select_worker,
};
pub use placement::{
    enqueue_operation, healthy_worker_template, heartbeat_worker, mark_runtime_lost,
    release_placement_resources,
};

#[derive(Clone)]
pub struct Lifecycle {
    pool: PgPool,
}

impl Lifecycle {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn request_start(&self, server_id: Uuid, reason: &str) -> Result<ObservedState> {
        self.admit_start(LifecycleActor::Owner, server_id, reason, false)
            .await?
            .ok_or_else(|| {
                LifecycleFailure::Conflict(
                    "unconditional start admission returned no state".to_owned(),
                )
                .into()
            })
    }

    async fn admit_start(
        &self,
        actor: LifecycleActor,
        server_id: Uuid,
        reason: &str,
        queued_only: bool,
    ) -> Result<Option<ObservedState>> {
        let mut tx = self.pool.begin().await?;
        // Serialize admission across coordinators, including manual starts and
        // gateway joins, so newly arriving requests cannot bypass the queue.
        sqlx::query("SELECT pg_advisory_xact_lock(724513, 1)")
            .execute(&mut *tx)
            .await?;
        let server = match lock_server_optional(&mut tx, server_id).await? {
            Some(server) => server,
            None if queued_only => {
                tx.commit().await?;
                return Ok(None);
            }
            None => bail!(not_found: "server not found"),
        };
        authorize_actor(&mut tx, &server, actor.clone(), false).await?;
        if has_live_reset(&mut tx, server_id).await? {
            bail!("server reset is in progress");
        }
        if queued_only {
            let queued: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM start_requests WHERE server_id = $1)",
            )
            .bind(server_id)
            .fetch_one(&mut *tx)
            .await?;
            if !queued {
                tx.commit().await?;
                return Ok(None);
            }
        }

        let owner_active: bool =
            sqlx::query_scalar("SELECT account_status = 'active' FROM users WHERE id = $1")
                .bind(server.owner_id)
                .fetch_one(&mut *tx)
                .await?;
        if !owner_active {
            if queued_only {
                sqlx::query("DELETE FROM start_requests WHERE server_id = $1")
                    .bind(server_id)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("UPDATE servers SET desired_state = 'stopped', observed_state = 'sleeping', updated_at = now() WHERE id = $1 AND observed_state = 'queued'")
                    .bind(server_id).execute(&mut *tx).await?;
                tx.commit().await?;
                return Ok(None);
            }
            bail!(forbidden: "server owner is suspended");
        }

        if server.desired_state == DesiredState::Deleted
            || server.observed_state == ObservedState::Deleted
        {
            bail!(not_found: "server is deleted");
        }
        if matches!(
            server.observed_state,
            ObservedState::Online
                | ObservedState::Starting
                | ObservedState::Restoring
                | ObservedState::Stopping
                | ObservedState::Snapshotting
        ) {
            sqlx::query(
                "UPDATE servers SET desired_state = 'running', updated_at = now() WHERE id = $1",
            )
            .bind(server_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(Some(server.observed_state));
        }

        if let Some(placement) = lock_placement(&mut tx, server_id).await?
            && placement.active
        {
            bail!(
                "server still has an active fenced placement; confirm the previous runtime is inactive before starting another copy"
            );
        }

        let queue_head: Option<Uuid> = sqlx::query_scalar(
            "SELECT server_id FROM start_requests ORDER BY requested_at, server_id LIMIT 1",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let worker = if queue_head.is_none_or(|head| head == server_id) {
            select_worker(&mut tx, server.id, server.current_snapshot_generation).await?
        } else {
            None
        };
        let Some(worker) = worker else {
            sqlx::query(
                "INSERT INTO start_requests(server_id, reason) VALUES($1, $2)
                 ON CONFLICT(server_id) DO UPDATE SET reason = EXCLUDED.reason",
            )
            .bind(server_id)
            .bind(reason)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE servers
                 SET desired_state = 'running', observed_state = 'queued', last_error = NULL, updated_at = now()
                 WHERE id = $1",
            )
            .bind(server_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(Some(ObservedState::Queued));
        };

        place_and_enqueue_start(&mut tx, &server, &worker, reason).await?;
        tx.commit().await?;
        Ok(Some(ObservedState::Restoring))
    }

    pub async fn request_stop(&self, server_id: Uuid, reason: &str) -> Result<ObservedState> {
        self.request_stop_for_actor(LifecycleActor::Owner, server_id, reason)
            .await
    }

    pub async fn request_stop_for_actor(
        &self,
        actor: LifecycleActor,
        server_id: Uuid,
        reason: &str,
    ) -> Result<ObservedState> {
        let mut tx = self.pool.begin().await?;
        let server = lock_server(&mut tx, server_id).await?;
        authorize_actor(&mut tx, &server, actor, true).await?;
        let state = request_stop_in_transaction(&mut tx, &server, reason).await?;
        tx.commit().await?;
        Ok(state)
    }

    pub async fn request_restart(&self, server_id: Uuid) -> Result<ObservedState> {
        self.request_restart_for_actor(LifecycleActor::Owner, server_id)
            .await
    }

    pub async fn request_restart_for_actor(
        &self,
        actor: LifecycleActor,
        server_id: Uuid,
    ) -> Result<ObservedState> {
        let mut tx = self.pool.begin().await?;
        let server = lock_server(&mut tx, server_id).await?;
        authorize_actor(&mut tx, &server, actor.clone(), true).await?;
        if has_live_reset(&mut tx, server_id).await? {
            bail!("server reset is in progress");
        }
        if has_live_snapshot(&mut tx, server_id).await? {
            bail!("server snapshot is in progress");
        }
        if server.desired_state == DesiredState::Deleted
            || server.observed_state == ObservedState::Deleted
        {
            bail!(not_found: "server is deleted");
        }
        if server.observed_state == ObservedState::Sleeping {
            tx.commit().await?;
            return self
                .admit_start(actor, server_id, "restart", false)
                .await?
                .ok_or_else(|| {
                    LifecycleFailure::Conflict(
                        "restart start admission returned no state".to_owned(),
                    )
                    .into()
                });
        }
        if server.observed_state == ObservedState::Queued {
            tx.commit().await?;
            return Ok(ObservedState::Queued);
        }
        if matches!(
            server.observed_state,
            ObservedState::Stopping | ObservedState::Snapshotting
        ) {
            sqlx::query(
                "UPDATE servers SET desired_state = 'running', updated_at = now() WHERE id = $1",
            )
            .bind(server_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(server.observed_state);
        }
        let placement = lock_placement(&mut tx, server_id).await?.ok_or_else(|| {
            LifecycleFailure::Conflict("server has no placement to restart".to_owned())
        })?;
        sqlx::query("UPDATE servers SET desired_state = 'running', observed_state = 'stopping', updated_at = now() WHERE id = $1")
            .bind(server_id)
            .execute(&mut *tx)
            .await?;
        enqueue_operation(
            &mut tx,
            server_id,
            Some(placement.worker_id),
            OperationKind::Restart,
            server.generation,
            operation_payload(OperationPayload {
                reason: Some("manual_restart".to_owned()),
                ..OperationPayload::default()
            })?,
        )
        .await?;
        tx.commit().await?;
        Ok(ObservedState::Stopping)
    }

    pub async fn request_snapshot(&self, server_id: Uuid) -> Result<ObservedState> {
        self.request_snapshot_with_reason(server_id, crate::model::SnapshotReason::Manual)
            .await
    }

    pub async fn request_snapshot_with_reason(
        &self,
        server_id: Uuid,
        reason: crate::model::SnapshotReason,
    ) -> Result<ObservedState> {
        self.request_snapshot_for_actor(LifecycleActor::Owner, server_id, reason)
            .await
    }

    pub async fn request_snapshot_for_actor(
        &self,
        actor: LifecycleActor,
        server_id: Uuid,
        reason: crate::model::SnapshotReason,
    ) -> Result<ObservedState> {
        let mut tx = self.pool.begin().await?;
        let server = lock_server(&mut tx, server_id).await?;
        authorize_actor(&mut tx, &server, actor, true).await?;
        let state = request_snapshot_in_transaction(&mut tx, &server, reason).await?;
        tx.commit().await?;
        Ok(state)
    }

    pub async fn request_world_export(&self, server_id: Uuid, requested_by: Uuid) -> Result<Uuid> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(724514, hashtext($1))")
            .bind(server_id.to_string())
            .execute(&mut *tx)
            .await?;
        let server = lock_server(&mut tx, server_id).await?;
        if server.desired_state == DesiredState::Deleted
            || server.observed_state == ObservedState::Deleted
        {
            bail!(not_found: "server is deleted");
        }
        let has_live_operation: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM operations
             WHERE server_id = $1 AND status IN ('pending', 'running'))",
        )
        .bind(server_id)
        .fetch_one(&mut *tx)
        .await?;
        if has_live_operation {
            bail!("another server operation is in progress");
        }

        let export_id = Uuid::new_v4();
        match server.observed_state {
            ObservedState::Online if server.desired_state == DesiredState::Running => {
                let placement = lock_placement(&mut tx, server_id).await?.ok_or_else(|| {
                    LifecycleFailure::Conflict("online server has no placement".to_owned())
                })?;
                if !placement.active || placement.generation != server.generation {
                    bail!("online server placement is not active for the current generation");
                }
                sqlx::query(
                    "INSERT INTO world_exports(id, server_id, requested_by, status, stage, worker_id)
                     VALUES($1, $2, $3, 'pending', 'waiting_snapshot', $4)",
                )
                .bind(export_id)
                .bind(server_id)
                .bind(requested_by)
                .bind(placement.worker_id)
                .execute(&mut *tx)
                .await?;
                enqueue_operation(
                    &mut tx,
                    server_id,
                    Some(placement.worker_id),
                    OperationKind::Snapshot,
                    server.generation,
                    operation_payload(OperationPayload {
                        reason: Some("export".to_owned()),
                        ..OperationPayload::default()
                    })?,
                )
                .await?;
                sqlx::query(
                    "UPDATE servers SET observed_state = 'snapshotting', updated_at = now()
                     WHERE id = $1",
                )
                .bind(server_id)
                .execute(&mut *tx)
                .await?;
            }
            ObservedState::Sleeping if server.desired_state == DesiredState::Stopped => {
                let generation = server.current_snapshot_generation.ok_or_else(|| {
                    LifecycleFailure::Conflict(
                        "uninitialized worlds must be started before exporting".to_owned(),
                    )
                })?;
                let complete: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM snapshots
                     WHERE server_id = $1 AND generation = $2 AND complete)",
                )
                .bind(server_id)
                .bind(generation)
                .fetch_one(&mut *tx)
                .await?;
                if !complete {
                    bail!("server has no authoritative completed snapshot to export");
                }
                sqlx::query(
                    "INSERT INTO world_exports(
                         id, server_id, requested_by, status, stage, source_generation
                     ) VALUES($1, $2, $3, 'pending', 'ready', $4)",
                )
                .bind(export_id)
                .bind(server_id)
                .bind(requested_by)
                .bind(generation)
                .execute(&mut *tx)
                .await?;
            }
            _ => bail!("world export requires an online or sleeping server"),
        }
        tx.commit().await?;
        Ok(export_id)
    }

    pub async fn request_restore(
        &self,
        server_id: Uuid,
        target_generation: i64,
    ) -> Result<ObservedState> {
        self.request_restore_for_actor(LifecycleActor::Owner, server_id, target_generation)
            .await
    }

    pub async fn request_restore_for_actor(
        &self,
        actor: LifecycleActor,
        server_id: Uuid,
        target_generation: i64,
    ) -> Result<ObservedState> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(724513, 1)")
            .execute(&mut *tx)
            .await?;
        // Publish/prune must observe the restore pin before releasing its source.
        // Acquire before the server row, matching snapshot publication lock order.
        sqlx::query("SELECT pg_advisory_xact_lock(724514, hashtext($1))")
            .bind(server_id.to_string())
            .execute(&mut *tx)
            .await?;
        let server = lock_server(&mut tx, server_id).await?;
        authorize_actor(&mut tx, &server, actor, true).await?;
        if has_live_reset(&mut tx, server_id).await? {
            bail!("server reset is in progress");
        }
        if server.desired_state == DesiredState::Deleted
            || server.observed_state == ObservedState::Deleted
        {
            bail!(not_found: "server is deleted");
        }
        if target_generation < 1 {
            bail!(invalid: "invalid snapshot generation");
        }
        let retained: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM snapshots
                WHERE server_id = $1 AND generation = $2 AND complete
                  AND manifest_key <> '' AND checksum <> ''
             )",
        )
        .bind(server_id)
        .bind(target_generation)
        .fetch_one(&mut *tx)
        .await?;
        if !retained {
            bail!(not_found: "snapshot not found");
        }

        match server.observed_state {
            ObservedState::Sleeping => {
                if let Some(placement) = lock_placement(&mut tx, server_id).await?
                    && placement.active
                {
                    bail!("server still has an active fenced placement");
                }
                let worker = select_worker(&mut tx, server_id, Some(target_generation))
                    .await?
                    .ok_or_else(|| {
                        LifecycleFailure::Unavailable(
                            "no worker capacity available for snapshot restore".to_owned(),
                        )
                    })?;
                place_and_enqueue_worker_operation(
                    &mut tx,
                    &server,
                    &worker,
                    DesiredState::Stopped,
                    ObservedState::Restoring,
                    OperationKind::Restore,
                    operation_payload(OperationPayload {
                        target_generation: Some(target_generation),
                        ..OperationPayload::default()
                    })?,
                )
                .await?;
                tx.commit().await?;
                Ok(ObservedState::Restoring)
            }
            ObservedState::Online if server.desired_state == DesiredState::Running => {
                if has_live_snapshot(&mut tx, server_id).await? {
                    bail!("server snapshot is in progress");
                }
                let live_operation: bool = sqlx::query_scalar(
                    "SELECT EXISTS(
                        SELECT 1 FROM operations
                        WHERE server_id = $1 AND status IN ('pending', 'running')
                    )",
                )
                .bind(server_id)
                .fetch_one(&mut *tx)
                .await?;
                if live_operation {
                    bail!("another server operation is in progress");
                }
                let placement = lock_placement(&mut tx, server_id).await?.ok_or_else(|| {
                    LifecycleFailure::Conflict("online server has no placement".to_owned())
                })?;
                if !placement.active || placement.generation != server.generation {
                    bail!("online server placement is not active for the current generation");
                }
                sqlx::query(
                    "UPDATE servers
                     SET desired_state = 'stopped', observed_state = 'stopping',
                         last_error = NULL, updated_at = now()
                     WHERE id = $1",
                )
                .bind(server_id)
                .execute(&mut *tx)
                .await?;
                enqueue_operation(
                    &mut tx,
                    server_id,
                    Some(placement.worker_id),
                    OperationKind::Stop,
                    server.generation,
                    operation_payload(OperationPayload {
                        reason: Some("restore".to_owned()),
                        restore_after_stop_generation: Some(target_generation),
                        ..OperationPayload::default()
                    })?,
                )
                .await?;
                tx.commit().await?;
                Ok(ObservedState::Stopping)
            }
            _ => bail!("snapshot restore requires a sleeping or online server"),
        }
    }

    pub async fn request_delete(&self, server_id: Uuid) -> Result<ObservedState> {
        self.request_delete_for_actor(LifecycleActor::Owner, server_id)
            .await
    }

    pub async fn request_delete_for_actor(
        &self,
        actor: LifecycleActor,
        server_id: Uuid,
    ) -> Result<ObservedState> {
        let mut tx = self.pool.begin().await?;
        let server = lock_server(&mut tx, server_id).await?;
        authorize_actor(&mut tx, &server, actor, true).await?;
        if has_live_reset(&mut tx, server_id).await? {
            bail!("server reset is in progress");
        }
        if has_live_snapshot(&mut tx, server_id).await? {
            bail!("server snapshot is in progress");
        }
        sqlx::query(
            "UPDATE servers SET desired_state = 'deleted', last_error = NULL, updated_at = now()
             WHERE id = $1",
        )
        .bind(server_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM start_requests WHERE server_id = $1")
            .bind(server_id)
            .execute(&mut *tx)
            .await?;

        let live_operation: Option<(Uuid, OperationKind, OperationStatus)> = sqlx::query_as(
            "SELECT id, kind, status FROM operations
             WHERE server_id = $1 AND status IN ('pending','running')
               AND kind IN ('start','stop','restart','restore','delete')
             ORDER BY created_at DESC LIMIT 1
             FOR UPDATE",
        )
        .bind(server_id)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some((operation_id, OperationKind::Start, OperationStatus::Pending)) = live_operation
        {
            sqlx::query(
                "UPDATE operations SET status = 'cancelled', updated_at = now() WHERE id = $1",
            )
            .bind(operation_id)
            .execute(&mut *tx)
            .await?;
            if let Some(placement) = lock_placement(&mut tx, server_id).await?
                && !placement.active
            {
                release_placement_resources(&mut tx, &placement).await?;
            }
            enqueue_delete(&mut tx, server_id, server.generation).await?;
            sqlx::query(
                "UPDATE servers SET observed_state = 'deleted', updated_at = now() WHERE id = $1",
            )
            .bind(server_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(ObservedState::Deleted);
        }

        if live_operation.is_some() {
            tx.commit().await?;
            return Ok(match server.observed_state {
                ObservedState::Sleeping | ObservedState::Queued | ObservedState::Failed => {
                    ObservedState::Stopping
                }
                state => state,
            });
        }

        if let Some(placement) = lock_placement(&mut tx, server_id).await?
            && placement.active
        {
            enqueue_operation(
                &mut tx,
                server_id,
                Some(placement.worker_id),
                OperationKind::Stop,
                server.generation,
                operation_payload(OperationPayload {
                    reason: Some("delete".to_owned()),
                    ..OperationPayload::default()
                })?,
            )
            .await?;
            sqlx::query(
                "UPDATE servers SET observed_state = 'stopping', updated_at = now() WHERE id = $1",
            )
            .bind(server_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(ObservedState::Stopping);
        }

        enqueue_delete(&mut tx, server_id, server.generation).await?;
        sqlx::query(
            "UPDATE servers SET observed_state = 'deleted', updated_at = now() WHERE id = $1",
        )
        .bind(server_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ObservedState::Deleted)
    }

    pub async fn drain_queue_once(&self) -> Result<usize> {
        let server_ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT server_id FROM start_requests ORDER BY requested_at, server_id LIMIT 32",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut started = 0;
        for server_id in server_ids {
            match self
                .admit_start(LifecycleActor::Owner, server_id, "queued", true)
                .await?
            {
                Some(ObservedState::Restoring) => started += 1,
                Some(ObservedState::Queued) => break,
                _ => {}
            }
        }
        Ok(started)
    }

    pub async fn request_reset(
        &self,
        server_id: Uuid,
        mode: ResetMode,
        full_reset_settings: serde_json::Value,
    ) -> Result<ObservedState> {
        self.request_reset_for_actor(LifecycleActor::Owner, server_id, mode, full_reset_settings)
            .await
    }

    pub async fn request_reset_for_actor(
        &self,
        actor: LifecycleActor,
        server_id: Uuid,
        mode: ResetMode,
        full_reset_settings: serde_json::Value,
    ) -> Result<ObservedState> {
        let mut tx = self.pool.begin().await?;
        let server = lock_server(&mut tx, server_id).await?;
        authorize_actor(&mut tx, &server, actor, true).await?;
        if server.desired_state == DesiredState::Deleted
            || server.observed_state == ObservedState::Deleted
        {
            bail!(not_found: "server is deleted");
        }
        let live_operation: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM operations
             WHERE server_id = $1 AND status IN ('pending', 'running'))",
        )
        .bind(server_id)
        .fetch_one(&mut *tx)
        .await?;
        if live_operation {
            bail!("another server operation is in progress");
        }
        let was_running = server.observed_state == ObservedState::Online
            && server.desired_state == DesiredState::Running;
        if !was_running && server.observed_state != ObservedState::Sleeping {
            bail!("reset requires an online or sleeping server");
        }
        let worker_id = if was_running {
            let placement = lock_placement(&mut tx, server_id).await?.ok_or_else(|| {
                LifecycleFailure::Conflict("online server has no worker placement".to_owned())
            })?;
            if !placement.active || placement.generation != server.generation {
                bail!("online server placement is not active for the current generation");
            }
            let healthy: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM workers WHERE id = $1 AND NOT draining
                  AND heartbeat_at > now() - interval '30 seconds')",
            )
            .bind(placement.worker_id)
            .fetch_one(&mut *tx)
            .await?;
            if !healthy {
                bail!(
                    "assigned worker is unavailable; reset cannot safely stop the running server"
                );
            }
            Some(placement.worker_id)
        } else {
            let healthy_worker: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM workers WHERE NOT draining
                  AND heartbeat_at > now() - interval '30 seconds'
                 ORDER BY id LIMIT 1",
            )
            .fetch_optional(&mut *tx)
            .await?;
            Some(healthy_worker.ok_or_else(|| {
                LifecycleFailure::Unavailable(
                    "no worker is available to reset this server".to_owned(),
                )
            })?)
        };

        if mode == ResetMode::Full {
            sqlx::query("UPDATE servers SET settings = $2 WHERE id = $1")
                .bind(server_id)
                .bind(full_reset_settings)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM whitelist_entries WHERE server_id = $1")
                .bind(server_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO server_whitelist_state(server_id, revision, applied_revision)
                 VALUES($1, 1, 0) ON CONFLICT(server_id) DO UPDATE
                   SET revision = server_whitelist_state.revision + 1, updated_at = now()",
            )
            .bind(server_id)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE servers SET generation = generation + 1, desired_state = $2,
             observed_state = 'resetting', last_error = NULL, updated_at = now()
             WHERE id = $1",
        )
        .bind(server_id)
        .bind(if was_running {
            DesiredState::Running
        } else {
            DesiredState::Stopped
        })
        .execute(&mut *tx)
        .await?;
        if was_running {
            let placement_updated = sqlx::query(
                "UPDATE placements SET generation = $2, updated_at = now()
                 WHERE server_id = $1 AND worker_id = $3 AND generation = $4 AND active",
            )
            .bind(server_id)
            .bind(server.generation + 1)
            .bind(worker_id)
            .bind(server.generation)
            .execute(&mut *tx)
            .await?;
            if placement_updated.rows_affected() != 1 {
                bail!("running reset lost its active placement while advancing generation");
            }
        }
        enqueue_operation(
            &mut tx,
            server_id,
            worker_id,
            OperationKind::Reset,
            server.generation + 1,
            operation_payload(OperationPayload {
                mode: Some(mode),
                was_running: Some(was_running),
                ..OperationPayload::default()
            })?,
        )
        .await?;
        tx.commit().await?;
        Ok(ObservedState::Resetting)
    }
}

async fn has_live_reset(tx: &mut Transaction<'_, Postgres>, server_id: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM operations
         WHERE server_id = $1 AND kind = 'reset' AND status IN ('pending', 'running'))",
    )
    .bind(server_id)
    .fetch_one(&mut **tx)
    .await?)
}

async fn request_stop_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    server: &ServerRecord,
    reason: &str,
) -> Result<ObservedState> {
    let server_id = server.id;
    if has_live_reset(tx, server_id).await? {
        bail!("server reset is in progress");
    }
    if has_live_snapshot(tx, server_id).await? {
        bail!("server snapshot is in progress");
    }
    if server.desired_state == DesiredState::Deleted
        || server.observed_state == ObservedState::Deleted
    {
        return Ok(ObservedState::Deleted);
    }
    sqlx::query("UPDATE servers SET desired_state = 'stopped', updated_at = now() WHERE id = $1")
        .bind(server_id)
        .execute(&mut **tx)
        .await?;

    if server.observed_state == ObservedState::Restoring {
        let restoring_snapshot: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM operations
                WHERE server_id = $1 AND kind = 'restore' AND status IN ('pending','running')
             )",
        )
        .bind(server_id)
        .fetch_one(&mut **tx)
        .await?;
        if restoring_snapshot {
            return Ok(ObservedState::Restoring);
        }
    }

    match server.observed_state {
        ObservedState::Sleeping | ObservedState::Deleted => Ok(server.observed_state),
        ObservedState::Queued => {
            sqlx::query("DELETE FROM start_requests WHERE server_id = $1")
                .bind(server_id)
                .execute(&mut **tx)
                .await?;
            sqlx::query(
                "UPDATE operations SET status = 'cancelled', updated_at = now()
                 WHERE server_id = $1 AND status = 'pending'",
            )
            .bind(server_id)
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                "UPDATE servers SET observed_state = 'sleeping', updated_at = now() WHERE id = $1",
            )
            .bind(server_id)
            .execute(&mut **tx)
            .await?;
            Ok(ObservedState::Sleeping)
        }
        ObservedState::Stopping | ObservedState::Snapshotting => Ok(server.observed_state),
        _ => {
            let live_start: Option<(Uuid, OperationStatus)> = sqlx::query_as(
                "SELECT id, status FROM operations
                 WHERE server_id = $1 AND kind IN ('start','restart')
                   AND status IN ('pending','running')
                 ORDER BY created_at DESC LIMIT 1 FOR UPDATE",
            )
            .bind(server_id)
            .fetch_optional(&mut **tx)
            .await?;
            let placement = lock_placement(tx, server_id).await?.ok_or_else(|| {
                LifecycleFailure::Conflict("server has no placement to stop".to_owned())
            })?;

            if let Some((id, OperationStatus::Pending)) = live_start {
                sqlx::query(
                    "UPDATE operations SET status = 'cancelled', updated_at = now() WHERE id = $1",
                )
                .bind(id)
                .execute(&mut **tx)
                .await?;
                release_placement_resources(tx, &placement).await?;
                sqlx::query(
                    "UPDATE servers SET observed_state = 'sleeping', updated_at = now() WHERE id = $1",
                )
                .bind(server_id)
                .execute(&mut **tx)
                .await?;
                return Ok(ObservedState::Sleeping);
            }

            if live_start.is_none() {
                enqueue_operation(
                    tx,
                    server_id,
                    Some(placement.worker_id),
                    OperationKind::Stop,
                    server.generation,
                    operation_payload(OperationPayload {
                        reason: Some(reason.to_owned()),
                        ..OperationPayload::default()
                    })?,
                )
                .await?;
                sqlx::query(
                    "UPDATE servers SET observed_state = 'stopping', updated_at = now() WHERE id = $1",
                )
                .bind(server_id)
                .execute(&mut **tx)
                .await?;
            }
            Ok(ObservedState::Stopping)
        }
    }
}

async fn request_snapshot_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    server: &ServerRecord,
    reason: crate::model::SnapshotReason,
) -> Result<ObservedState> {
    let server_id = server.id;
    if has_live_reset(tx, server_id).await? {
        bail!("server reset is in progress");
    }
    if has_live_snapshot(tx, server_id).await? {
        bail!("a server snapshot is already in progress");
    }
    if server.observed_state != ObservedState::Online
        || server.desired_state != DesiredState::Running
    {
        bail!("snapshots require an online running server");
    }
    let placement = lock_placement(tx, server_id)
        .await?
        .ok_or_else(|| LifecycleFailure::Conflict("online server has no placement".to_owned()))?;
    if !placement.active || placement.generation != server.generation {
        bail!("online server placement is not active for the current generation");
    }
    let reason = serde_json::to_value(reason)?
        .as_str()
        .context("snapshot reason must serialize as a string")?
        .to_owned();
    enqueue_operation(
        tx,
        server_id,
        Some(placement.worker_id),
        OperationKind::Snapshot,
        server.generation,
        operation_payload(OperationPayload {
            reason: Some(reason),
            ..OperationPayload::default()
        })?,
    )
    .await?;
    sqlx::query(
        "UPDATE servers SET observed_state = 'snapshotting', updated_at = now() WHERE id = $1",
    )
    .bind(server_id)
    .execute(&mut **tx)
    .await?;
    Ok(ObservedState::Snapshotting)
}

pub(super) fn operation_payload(mut payload: OperationPayload) -> Result<Value> {
    payload.stage = OperationStage::Requested;
    serde_json::to_value(payload).context("lifecycle operation payload must be serializable")
}

async fn has_live_snapshot(tx: &mut Transaction<'_, Postgres>, server_id: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM operations
         WHERE server_id = $1 AND kind = 'snapshot' AND status IN ('pending', 'running'))",
    )
    .bind(server_id)
    .fetch_one(&mut **tx)
    .await?)
}
