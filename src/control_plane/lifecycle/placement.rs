use anyhow::Result;
use chrono::Utc;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::{LifecycleFailure, bail};
use crate::contracts::worker::OperationPayload;
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
        .ok_or_else(|| LifecycleFailure::NotFound("server not found".to_owned()).into())
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
        "SELECT server_id, worker_id, generation, lease_token, java_port, bedrock_port, signal_port,
                active, updated_at
         FROM placements WHERE server_id = $1 FOR UPDATE",
    )
    .bind(server_id)
    .fetch_optional(&mut **tx)
    .await?)
}

pub(super) async fn select_worker(
    tx: &mut Transaction<'_, Postgres>,
    server_id: Uuid,
    snapshot_generation: Option<i64>,
) -> Result<Option<WorkerRecord>> {
    Ok(sqlx::query_as::<_, WorkerRecord>(
        "SELECT w.id, w.name, w.address, w.signal_address, w.total_memory_bytes, w.total_disk_bytes,
                w.total_cpu_millis, w.reserved_memory_bytes, w.reserved_disk_bytes,
                w.reserved_cpu_millis, w.heartbeat_at, w.draining,
                w.port_min, w.port_max
         FROM workers w
         WHERE NOT w.draining
           AND w.heartbeat_at > now() - interval '30 seconds'
           AND w.total_memory_bytes - w.reserved_memory_bytes >= $1
           AND w.total_disk_bytes - w.reserved_disk_bytes >= $2
           AND w.total_cpu_millis - w.reserved_cpu_millis >= $3
           AND EXISTS (
               SELECT 1 FROM generate_series(w.port_min, w.port_max - 2) AS candidate(port)
               WHERE NOT EXISTS (
                   SELECT 1 FROM port_leases l
                   WHERE l.worker_id = w.id
                     AND l.port IN (candidate.port, candidate.port + 1, candidate.port + 2)
               )
           )
           AND w.signal_address <> ''
         ORDER BY CASE WHEN EXISTS (
                      SELECT 1 FROM worker_snapshot_hints h
                      JOIN snapshots s ON s.server_id = h.server_id
                          AND s.generation = h.snapshot_generation AND s.complete
                      WHERE h.worker_id = w.id AND h.server_id = $4
                        AND h.snapshot_generation = $5
                  ) THEN 0 ELSE 1 END,
                  (w.total_memory_bytes - w.reserved_memory_bytes - $1) ASC,
                  (w.total_disk_bytes - w.reserved_disk_bytes - $2) ASC,
                  w.id
         FOR UPDATE OF w SKIP LOCKED
         LIMIT 1",
    )
    .bind(SERVER_MEMORY_BYTES)
    .bind(SERVER_STORAGE_BYTES)
    .bind(SERVER_CPU_MILLIS)
    .bind(server_id)
    .bind(snapshot_generation)
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
        super::operation_payload(OperationPayload {
            reason: Some(reason.to_owned()),
            ..OperationPayload::default()
        })?,
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
    payload: serde_json::Value,
) -> Result<()> {
    let base_port: Option<i32> = sqlx::query_scalar(
        "SELECT p
         FROM generate_series($2, $3 - 2) AS p
         WHERE NOT EXISTS (
             SELECT 1 FROM port_leases l
             WHERE l.worker_id = $1 AND l.port IN (p, p + 1, p + 2)
         )
         ORDER BY p
         LIMIT 1",
    )
    .bind(worker.id)
    .bind(worker.port_min)
    .bind(worker.port_max)
    .fetch_optional(&mut **tx)
    .await?;
    let base_port = base_port.ok_or_else(|| {
        LifecycleFailure::Unavailable("selected worker lost its free port triple".to_owned())
    })?;

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
        "INSERT INTO placements(server_id, worker_id, generation, lease_token, java_port, bedrock_port, signal_port, active)
         VALUES($1, $2, $3, $4, $5, $6, $7, false)
         ON CONFLICT(server_id) DO UPDATE SET
           worker_id = EXCLUDED.worker_id,
           generation = EXCLUDED.generation,
           lease_token = EXCLUDED.lease_token,
           java_port = EXCLUDED.java_port,
           bedrock_port = EXCLUDED.bedrock_port,
           signal_port = EXCLUDED.signal_port,
           active = false,
           updated_at = now()",
    )
    .bind(server.id)
    .bind(worker.id)
    .bind(generation)
    .bind(lease_token)
    .bind(base_port)
    .bind(base_port + 1)
    .bind(base_port + 2)
    .execute(&mut **tx)
    .await?;

    for (port, purpose) in [
        (base_port, "java"),
        (base_port + 1, "bedrock"),
        (base_port + 2, "signal"),
    ] {
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
    // Cleanup is always assigned by the control plane. An inactive placement
    // still identifies the worker that owns the server's local data; when no
    // placement remains, choose a healthy registered worker so workers never
    // claim an anonymous delete row.
    let previous_worker: Option<Uuid> =
        sqlx::query_scalar("SELECT worker_id FROM placements WHERE server_id = $1")
            .bind(server_id)
            .fetch_optional(&mut **tx)
            .await?;
    let worker_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM workers
         WHERE NOT draining AND heartbeat_at > now() - interval '30 seconds'
           AND ($1 IS NULL OR id <> $1)
         ORDER BY id LIMIT 1",
    )
    .bind(previous_worker)
    .fetch_optional(&mut **tx)
    .await?
    .or(previous_worker);
    enqueue_operation(
        tx,
        server_id,
        worker_id,
        OperationKind::Delete,
        generation,
        super::operation_payload(OperationPayload {
            reason: Some("delete".to_owned()),
            ..OperationPayload::default()
        })?,
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
    expected_generation: i64,
    error: &str,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    let server = lock_server(&mut tx, server_id).await?;
    let Some(placement) = lock_placement(&mut tx, server_id).await? else {
        tx.commit().await?;
        return Ok(());
    };
    if server.generation != expected_generation
        || placement.worker_id != worker_id
        || placement.generation != expected_generation
        || !placement.active
    {
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
        "INSERT INTO workers(id,name,address,signal_address,total_memory_bytes,total_disk_bytes,total_cpu_millis,heartbeat_at,draining,port_min,port_max)
         VALUES($1,$2,$3,$4,$5,$6,$7,now(),$8,$9,$10)
         ON CONFLICT(id) DO UPDATE SET
           name=EXCLUDED.name, address=EXCLUDED.address,
           signal_address=EXCLUDED.signal_address,
           total_memory_bytes=EXCLUDED.total_memory_bytes, total_disk_bytes=EXCLUDED.total_disk_bytes,
           total_cpu_millis=EXCLUDED.total_cpu_millis,
           heartbeat_at=now(), draining=EXCLUDED.draining,
           port_min=EXCLUDED.port_min, port_max=EXCLUDED.port_max",
    )
    .bind(worker.id)
    .bind(&worker.name)
    .bind(&worker.address)
    .bind(&worker.signal_address)
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
        signal_address: "127.0.0.1".to_owned(),
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
