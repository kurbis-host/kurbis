use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::json;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::model::{
    DesiredState, ObservedState, OperationKind, PlacementRecord, SERVER_CPU_MILLIS,
    SERVER_MEMORY_BYTES, SERVER_STORAGE_BYTES, ServerRecord, WorkerRecord,
};

pub(super) async fn lock_server(
    tx: &mut Transaction<'_, Postgres>,
    server_id: Uuid,
) -> Result<ServerRecord> {
    lock_server_optional(tx, server_id)
        .await?
        .context("server not found")
}

pub(super) async fn lock_server_optional(
    tx: &mut Transaction<'_, Postgres>,
    server_id: Uuid,
) -> Result<Option<ServerRecord>> {
    Ok(sqlx::query_as::<_, ServerRecord>(
        "SELECT id, owner_id, subdomain, is_default, desired_state, observed_state, settings, storage_bytes,
                generation, current_snapshot_generation, last_error, created_at, updated_at
         FROM servers WHERE id = $1 FOR UPDATE",
    )
    .bind(server_id)
    .fetch_optional(&mut **tx)
    .await?)
}

pub(super) async fn lock_placement(
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

pub(super) async fn select_worker(
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

pub(super) async fn place_and_enqueue_start(
    tx: &mut Transaction<'_, Postgres>,
    server: &ServerRecord,
    worker: &WorkerRecord,
    reason: &str,
) -> Result<()> {
    place_and_enqueue_worker_operation(
        tx,
        server,
        worker,
        DesiredState::Running,
        ObservedState::Restoring,
        OperationKind::Start,
        json!({"reason": reason}),
    )
    .await?;
    sqlx::query("DELETE FROM start_requests WHERE server_id = $1")
        .bind(server.id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub(super) async fn place_and_enqueue_worker_operation(
    tx: &mut Transaction<'_, Postgres>,
    server: &ServerRecord,
    worker: &WorkerRecord,
    desired_state: DesiredState,
    observed_state: ObservedState,
    kind: OperationKind,
    mut payload: serde_json::Value,
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
        "UPDATE servers SET desired_state = $2, observed_state = $3,
            generation = $4, last_error = NULL, updated_at = now()
         WHERE id = $1",
    )
    .bind(server.id)
    .bind(desired_state)
    .bind(observed_state)
    .bind(generation)
    .execute(&mut **tx)
    .await?;
    let payload_object = payload
        .as_object_mut()
        .context("worker operation payload must be a JSON object")?;
    payload_object.insert("lease_token".into(), json!(lease_token));
    enqueue_operation(tx, server.id, Some(worker.id), kind, generation, payload).await?;
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

pub(super) async fn enqueue_delete(
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
