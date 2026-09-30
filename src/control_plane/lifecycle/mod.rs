use anyhow::{Context, Result, bail};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::model::{DesiredState, ObservedState, OperationKind, OperationStatus};

mod operations;
mod placement;

pub use operations::{
    claim_operation, complete_delete, complete_restore, complete_snapshot, complete_start,
    complete_stop, fail_operation, recover_running_operations, retry_operation,
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
        self.admit_start(server_id, reason, false)
            .await?
            .context("unconditional start admission returned no state")
    }

    async fn admit_start(
        &self,
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
            None => bail!("server not found"),
        };
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
            bail!("server owner is suspended");
        }

        if server.desired_state == DesiredState::Deleted
            || server.observed_state == ObservedState::Deleted
        {
            bail!("server is deleted");
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
            select_worker(&mut tx, &server).await?
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
        let mut tx = self.pool.begin().await?;
        let server = lock_server(&mut tx, server_id).await?;
        if server.desired_state == DesiredState::Deleted
            || server.observed_state == ObservedState::Deleted
        {
            tx.commit().await?;
            return Ok(ObservedState::Deleted);
        }
        sqlx::query(
            "UPDATE servers SET desired_state = 'stopped', updated_at = now() WHERE id = $1",
        )
        .bind(server_id)
        .execute(&mut *tx)
        .await?;

        if server.observed_state == ObservedState::Restoring {
            let restoring_snapshot: bool = sqlx::query_scalar(
                "SELECT EXISTS(
                    SELECT 1 FROM operations
                    WHERE server_id = $1 AND kind = 'restore' AND status IN ('pending','running')
                 )",
            )
            .bind(server_id)
            .fetch_one(&mut *tx)
            .await?;
            if restoring_snapshot {
                tx.commit().await?;
                return Ok(ObservedState::Restoring);
            }
        }

        match server.observed_state {
            ObservedState::Sleeping | ObservedState::Deleted => {
                tx.commit().await?;
                Ok(server.observed_state)
            }
            ObservedState::Queued => {
                sqlx::query("DELETE FROM start_requests WHERE server_id = $1")
                    .bind(server_id)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query(
                    "UPDATE operations SET status = 'cancelled', updated_at = now()
                     WHERE server_id = $1 AND status = 'pending'",
                )
                .bind(server_id)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE servers SET observed_state = 'sleeping', updated_at = now() WHERE id = $1",
                )
                .bind(server_id)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(ObservedState::Sleeping)
            }
            ObservedState::Stopping | ObservedState::Snapshotting => {
                tx.commit().await?;
                Ok(server.observed_state)
            }
            _ => {
                let placement = lock_placement(&mut tx, server_id)
                    .await?
                    .context("server has no placement to stop")?;

                let live_start: Option<(Uuid, OperationStatus)> = sqlx::query_as(
                    "SELECT id, status FROM operations
                     WHERE server_id = $1 AND kind IN ('start','restart') AND status IN ('pending','running')
                     ORDER BY created_at DESC LIMIT 1",
                )
                .bind(server_id)
                .fetch_optional(&mut *tx)
                .await?;

                if let Some((id, OperationStatus::Pending)) = live_start {
                    sqlx::query("UPDATE operations SET status = 'cancelled', updated_at = now() WHERE id = $1")
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                    release_placement_resources(&mut tx, &placement).await?;
                    sqlx::query(
                        "UPDATE servers SET observed_state = 'sleeping', updated_at = now() WHERE id = $1",
                    )
                    .bind(server_id)
                    .execute(&mut *tx)
                    .await?;
                    tx.commit().await?;
                    return Ok(ObservedState::Sleeping);
                }

                if live_start.is_none() {
                    enqueue_operation(
                        &mut tx,
                        server_id,
                        Some(placement.worker_id),
                        OperationKind::Stop,
                        server.generation,
                        json!({ "reason": reason }),
                    )
                    .await?;
                    sqlx::query(
                        "UPDATE servers SET observed_state = 'stopping', updated_at = now() WHERE id = $1",
                    )
                    .bind(server_id)
                    .execute(&mut *tx)
                    .await?;
                }
                tx.commit().await?;
                Ok(ObservedState::Stopping)
            }
        }
    }

    pub async fn request_restart(&self, server_id: Uuid) -> Result<ObservedState> {
        let mut tx = self.pool.begin().await?;
        let server = lock_server(&mut tx, server_id).await?;
        if server.desired_state == DesiredState::Deleted
            || server.observed_state == ObservedState::Deleted
        {
            bail!("server is deleted");
        }
        if server.observed_state == ObservedState::Sleeping {
            tx.commit().await?;
            return self.request_start(server_id, "restart").await;
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
        let placement = lock_placement(&mut tx, server_id)
            .await?
            .context("server has no placement to restart")?;
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
            json!({"reason":"manual_restart"}),
        )
        .await?;
        tx.commit().await?;
        Ok(ObservedState::Stopping)
    }

    pub async fn request_snapshot(&self, server_id: Uuid) -> Result<ObservedState> {
        let mut tx = self.pool.begin().await?;
        let server = lock_server(&mut tx, server_id).await?;
        if server.observed_state != ObservedState::Online
            || server.desired_state != DesiredState::Running
        {
            bail!("manual snapshots require an online running server");
        }
        let placement = lock_placement(&mut tx, server_id)
            .await?
            .context("online server has no placement")?;
        if !placement.active || placement.generation != server.generation {
            bail!("online server placement is not active for the current generation");
        }
        let live_snapshot: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM operations
                WHERE server_id = $1 AND kind = 'snapshot' AND status IN ('pending','running')
             )",
        )
        .bind(server_id)
        .fetch_one(&mut *tx)
        .await?;
        if live_snapshot {
            bail!("manual snapshot already in progress");
        }
        enqueue_operation(
            &mut tx,
            server_id,
            Some(placement.worker_id),
            OperationKind::Snapshot,
            server.generation,
            json!({"reason":"manual"}),
        )
        .await?;
        tx.commit().await?;
        Ok(ObservedState::Online)
    }

    pub async fn request_restore(
        &self,
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
        if server.desired_state == DesiredState::Deleted
            || server.observed_state == ObservedState::Deleted
        {
            bail!("server is deleted");
        }
        if server.observed_state != ObservedState::Sleeping {
            bail!("snapshot restore requires a sleeping server");
        }
        if target_generation < 1 {
            bail!("invalid snapshot generation");
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
            bail!("snapshot not found");
        }
        if let Some(placement) = lock_placement(&mut tx, server_id).await?
            && placement.active
        {
            bail!("server still has an active fenced placement");
        }
        let worker = select_worker(&mut tx, &server)
            .await?
            .context("no worker capacity available for snapshot restore")?;
        place_and_enqueue_worker_operation(
            &mut tx,
            &server,
            &worker,
            DesiredState::Stopped,
            ObservedState::Restoring,
            OperationKind::Restore,
            json!({"target_generation": target_generation}),
        )
        .await?;
        tx.commit().await?;
        Ok(ObservedState::Restoring)
    }

    pub async fn request_delete(&self, server_id: Uuid) -> Result<ObservedState> {
        let mut tx = self.pool.begin().await?;
        let server = lock_server(&mut tx, server_id).await?;
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
                json!({"reason":"delete"}),
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
            match self.admit_start(server_id, "queued", true).await? {
                Some(ObservedState::Restoring) => started += 1,
                Some(ObservedState::Queued) => break,
                _ => {}
            }
        }
        Ok(started)
    }
}
