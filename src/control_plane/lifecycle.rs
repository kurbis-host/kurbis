use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::json;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::model::{
    DesiredState, ObservedState, OperationKind, OperationStatus, PlacementRecord,
    SERVER_CPU_MILLIS, SERVER_MEMORY_BYTES, SERVER_STORAGE_BYTES, ServerRecord, WorkerRecord,
};

mod operations;
pub use operations::{
    claim_operation, complete_start, complete_stop, fail_operation, recover_running_operations,
    retry_operation,
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
               AND kind IN ('start','stop','restart','delete')
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

async fn lock_server(tx: &mut Transaction<'_, Postgres>, server_id: Uuid) -> Result<ServerRecord> {
    lock_server_optional(tx, server_id)
        .await?
        .context("server not found")
}

async fn lock_server_optional(
    tx: &mut Transaction<'_, Postgres>,
    server_id: Uuid,
) -> Result<Option<ServerRecord>> {
    Ok(sqlx::query_as::<_, ServerRecord>(
        "SELECT id, owner_id, subdomain, desired_state, observed_state, settings, storage_bytes,
                generation, current_snapshot_generation, last_error, created_at, updated_at
         FROM servers WHERE id = $1 FOR UPDATE",
    )
    .bind(server_id)
    .fetch_optional(&mut **tx)
    .await?)
}

async fn lock_placement(
    tx: &mut Transaction<'_, Postgres>,
    server_id: Uuid,
) -> Result<Option<PlacementRecord>> {
    Ok(sqlx::query_as::<_, PlacementRecord>(
        "SELECT server_id, worker_id, generation, lease_token, java_port, bedrock_port,
                active, updated_at
         FROM placements WHERE server_id = $1 FOR UPDATE",
    )
    .bind(server_id)
    .fetch_optional(&mut **tx)
    .await?)
}

async fn select_worker(
    tx: &mut Transaction<'_, Postgres>,
    server: &ServerRecord,
) -> Result<Option<WorkerRecord>> {
    Ok(sqlx::query_as::<_, WorkerRecord>(
        "SELECT w.id, w.name, w.address, w.total_memory_bytes, w.total_disk_bytes,
                w.total_cpu_millis, w.reserved_memory_bytes, w.reserved_disk_bytes,
                w.reserved_cpu_millis, w.heartbeat_at, w.draining,
                w.port_min, w.port_max
         FROM workers w
         LEFT JOIN worker_snapshot_cache c
           ON c.worker_id = w.id AND c.server_id = $1 AND c.generation = $2
         WHERE NOT w.draining
           AND w.heartbeat_at > now() - interval '30 seconds'
           AND w.total_memory_bytes - w.reserved_memory_bytes >= $3
           AND w.total_disk_bytes - w.reserved_disk_bytes >= $4
           AND w.total_cpu_millis - w.reserved_cpu_millis >= $5
           AND EXISTS (
               SELECT 1 FROM generate_series(w.port_min, w.port_max - 1) AS candidate(port)
               WHERE NOT EXISTS (
                   SELECT 1 FROM port_leases l
                   WHERE l.worker_id = w.id AND l.port IN (candidate.port, candidate.port + 1)
               )
           )
         ORDER BY (c.worker_id IS NOT NULL) DESC,
                  (w.total_memory_bytes - w.reserved_memory_bytes - $3) ASC,
                  (w.total_disk_bytes - w.reserved_disk_bytes - $4) ASC,
                  w.id
         FOR UPDATE OF w SKIP LOCKED
         LIMIT 1",
    )
    .bind(server.id)
    .bind(server.current_snapshot_generation.unwrap_or(0))
    .bind(SERVER_MEMORY_BYTES)
    .bind(SERVER_STORAGE_BYTES)
    .bind(SERVER_CPU_MILLIS)
    .fetch_optional(&mut **tx)
    .await?)
}

async fn place_and_enqueue_start(
    tx: &mut Transaction<'_, Postgres>,
    server: &ServerRecord,
    worker: &WorkerRecord,
    reason: &str,
) -> Result<()> {
    let base_port: Option<i32> = sqlx::query_scalar(
        "SELECT p
         FROM generate_series($2, $3 - 1) AS p
         WHERE NOT EXISTS (
             SELECT 1 FROM port_leases l
             WHERE l.worker_id = $1 AND l.port IN (p, p + 1)
         )
         ORDER BY p
         LIMIT 1",
    )
    .bind(worker.id)
    .bind(worker.port_min)
    .bind(worker.port_max)
    .fetch_optional(&mut **tx)
    .await?;
    let base_port = base_port.context("selected worker lost its free port pair")?;

    if let Some(old) = lock_placement(tx, server.id).await? {
        sqlx::query("DELETE FROM port_leases WHERE server_id = $1")
            .bind(server.id)
            .execute(&mut **tx)
            .await?;
        if old.active {
            bail!("refusing to move an active server");
        }
    }

    let generation = server.generation + 1;
    let lease_token = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO placements(server_id, worker_id, generation, lease_token, java_port, bedrock_port, active)
         VALUES($1, $2, $3, $4, $5, $6, false)
         ON CONFLICT(server_id) DO UPDATE SET
           worker_id = EXCLUDED.worker_id,
           generation = EXCLUDED.generation,
           lease_token = EXCLUDED.lease_token,
           java_port = EXCLUDED.java_port,
           bedrock_port = EXCLUDED.bedrock_port,
           active = false,
           updated_at = now()",
    )
    .bind(server.id)
    .bind(worker.id)
    .bind(generation)
    .bind(lease_token)
    .bind(base_port)
    .bind(base_port + 1)
    .execute(&mut **tx)
    .await?;

    for (port, purpose) in [(base_port, "java"), (base_port + 1, "bedrock")] {
        sqlx::query(
            "INSERT INTO port_leases(worker_id, port, server_id, purpose) VALUES($1,$2,$3,$4)",
        )
        .bind(worker.id)
        .bind(port)
        .bind(server.id)
        .bind(purpose)
        .execute(&mut **tx)
        .await?;
    }

    sqlx::query(
        "UPDATE workers
         SET reserved_memory_bytes = reserved_memory_bytes + $2,
             reserved_disk_bytes = reserved_disk_bytes + $3,
             reserved_cpu_millis = reserved_cpu_millis + $4
         WHERE id = $1",
    )
    .bind(worker.id)
    .bind(SERVER_MEMORY_BYTES)
    .bind(SERVER_STORAGE_BYTES)
    .bind(SERVER_CPU_MILLIS)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "UPDATE servers SET desired_state = 'running', observed_state = 'restoring',
            generation = $2, last_error = NULL, updated_at = now()
         WHERE id = $1",
    )
    .bind(server.id)
    .bind(generation)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM start_requests WHERE server_id = $1")
        .bind(server.id)
        .execute(&mut **tx)
        .await?;

    enqueue_operation(
        tx,
        server.id,
        Some(worker.id),
        OperationKind::Start,
        generation,
        json!({ "reason": reason, "lease_token": lease_token }),
    )
    .await?;
    Ok(())
}

pub async fn enqueue_operation(
    tx: &mut Transaction<'_, Postgres>,
    server_id: Uuid,
    worker_id: Option<Uuid>,
    kind: OperationKind,
    generation: i64,
    payload: serde_json::Value,
) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO operations(id, server_id, worker_id, kind, status, generation, payload)
         VALUES($1,$2,$3,$4,'pending',$5,$6)",
    )
    .bind(id)
    .bind(server_id)
    .bind(worker_id)
    .bind(kind)
    .bind(generation)
    .bind(payload)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

async fn enqueue_delete(
    tx: &mut Transaction<'_, Postgres>,
    server_id: Uuid,
    generation: i64,
) -> Result<Uuid> {
    enqueue_operation(
        tx,
        server_id,
        None,
        OperationKind::Delete,
        generation,
        json!({"reason":"delete"}),
    )
    .await
}

pub async fn release_placement_resources(
    tx: &mut Transaction<'_, Postgres>,
    placement: &PlacementRecord,
) -> Result<()> {
    sqlx::query(
        "UPDATE workers
         SET reserved_memory_bytes = GREATEST(0, reserved_memory_bytes - $2),
             reserved_disk_bytes = GREATEST(0, reserved_disk_bytes - $3),
             reserved_cpu_millis = GREATEST(0, reserved_cpu_millis - $4)
         WHERE id = $1",
    )
    .bind(placement.worker_id)
    .bind(SERVER_MEMORY_BYTES)
    .bind(SERVER_STORAGE_BYTES)
    .bind(SERVER_CPU_MILLIS)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE placements SET active = false, updated_at = now() WHERE server_id = $1")
        .bind(placement.server_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM port_leases WHERE server_id = $1")
        .bind(placement.server_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn mark_runtime_lost(
    pool: &PgPool,
    worker_id: Uuid,
    server_id: Uuid,
    error: &str,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    let server = lock_server(&mut tx, server_id).await?;
    let Some(placement) = lock_placement(&mut tx, server_id).await? else {
        tx.commit().await?;
        return Ok(());
    };
    if placement.worker_id != worker_id || !placement.active {
        tx.commit().await?;
        return Ok(());
    }
    release_placement_resources(&mut tx, &placement).await?;
    let deleting = server.desired_state == DesiredState::Deleted;
    sqlx::query(
        "UPDATE servers
         SET desired_state = $4, observed_state = $5, last_error = $2, updated_at = now()
         WHERE id = $1 AND generation = $3",
    )
    .bind(server_id)
    .bind(error)
    .bind(server.generation)
    .bind(if deleting {
        DesiredState::Deleted
    } else {
        DesiredState::Stopped
    })
    .bind(if deleting {
        ObservedState::Deleted
    } else {
        ObservedState::Failed
    })
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE operations
         SET status = 'failed', last_error = $2, updated_at = now()
         WHERE server_id = $1 AND status IN ('pending', 'running')",
    )
    .bind(server_id)
    .bind(error)
    .execute(&mut *tx)
    .await?;
    if deleting {
        enqueue_delete(&mut tx, server_id, server.generation).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn heartbeat_worker(pool: &PgPool, worker: &WorkerRecord) -> Result<()> {
    sqlx::query(
        "INSERT INTO workers(id,name,address,total_memory_bytes,total_disk_bytes,total_cpu_millis,heartbeat_at,draining,port_min,port_max)
         VALUES($1,$2,$3,$4,$5,$6,now(),$7,$8,$9)
         ON CONFLICT(id) DO UPDATE SET
           name=EXCLUDED.name, address=EXCLUDED.address,
           total_memory_bytes=EXCLUDED.total_memory_bytes, total_disk_bytes=EXCLUDED.total_disk_bytes,
           total_cpu_millis=EXCLUDED.total_cpu_millis,
           heartbeat_at=now(), draining=EXCLUDED.draining,
           port_min=EXCLUDED.port_min, port_max=EXCLUDED.port_max",
    )
    .bind(worker.id)
    .bind(&worker.name)
    .bind(&worker.address)
    .bind(worker.total_memory_bytes)
    .bind(worker.total_disk_bytes)
    .bind(worker.total_cpu_millis)
    .bind(worker.draining)
    .bind(worker.port_min)
    .bind(worker.port_max)
    .execute(pool)
    .await?;
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "worker heartbeat mirrors the persisted worker capacity tuple"
)]
pub fn healthy_worker_template(
    id: Uuid,
    name: String,
    address: String,
    total_memory_bytes: i64,
    total_disk_bytes: i64,
    total_cpu_millis: i64,
    port_min: i32,
    port_max: i32,
) -> WorkerRecord {
    WorkerRecord {
        id,
        name,
        address,
        total_memory_bytes,
        total_disk_bytes,
        total_cpu_millis,
        reserved_memory_bytes: 0,
        reserved_disk_bytes: 0,
        reserved_cpu_millis: 0,
        heartbeat_at: Utc::now(),
        draining: false,
        port_min,
        port_max,
    }
}
