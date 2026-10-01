use anyhow::{Context, Result, bail};
use serde_json::json;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::{
    enqueue_delete, enqueue_operation, lock_placement, lock_server, release_placement_resources,
};
use crate::model::{DesiredState, ObservedState, OperationKind, OperationRecord, OperationStatus};

pub async fn claim_operation(pool: &PgPool, worker_id: Uuid) -> Result<Option<OperationRecord>> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM workers WHERE id = $1 FOR UPDATE")
        .bind(worker_id)
        .fetch_optional(&mut *tx)
        .await?
        .context("worker is not registered")?;
    let op = sqlx::query_as::<_, OperationRecord>(
        "SELECT o.id, o.server_id, o.worker_id, o.kind, o.status, o.generation, o.attempt,
                o.payload, o.last_error, o.created_at, o.updated_at
         FROM operations o JOIN servers s ON s.id = o.server_id
         WHERE o.status = 'pending' AND o.next_attempt_at <= now()
           AND o.worker_id = $1 AND o.generation = s.generation
         ORDER BY created_at
         FOR UPDATE SKIP LOCKED
         LIMIT 1",
    )
    .bind(worker_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(mut op) = op else {
        tx.commit().await?;
        return Ok(None);
    };
    sqlx::query(
        "UPDATE operations
         SET status = 'running', attempt = attempt + 1,
             claimed_at = now(), updated_at = now()
         WHERE id = $1 AND worker_id = $2 AND status = 'pending'",
    )
    .bind(op.id)
    .bind(worker_id)
    .execute(&mut *tx)
    .await?;
    op.status = OperationStatus::Running;
    op.attempt += 1;
    tx.commit().await?;
    Ok(Some(op))
}

pub async fn deactivate_placement_after_stop(
    pool: &PgPool,
    worker_id: Uuid,
    op: &OperationRecord,
) -> Result<()> {
    let expected_state = match op.kind {
        OperationKind::Stop | OperationKind::Restart => ObservedState::Stopping,
        OperationKind::Snapshot => ObservedState::Snapshotting,
        OperationKind::Reset => ObservedState::Resetting,
        _ => bail!("operation kind cannot deactivate a server placement"),
    };
    if op.worker_id != Some(worker_id) {
        bail!("placement deactivation worker identity does not match the operation");
    }

    let mut tx = pool.begin().await?;
    let server = lock_server(&mut tx, op.server_id).await?;
    if server.generation != op.generation || server.observed_state != expected_state {
        bail!("placement deactivation no longer matches the current server generation and state");
    }
    lock_claim(&mut tx, op, None).await?;
    let placement = lock_placement(&mut tx, op.server_id)
        .await?
        .context("placement deactivation lost its server placement")?;
    validate_placement(op, &placement)?;
    if placement.active {
        sqlx::query(
            "UPDATE placements SET active = false, updated_at = now()
             WHERE server_id = $1 AND worker_id = $2 AND generation = $3 AND active",
        )
        .bind(op.server_id)
        .bind(worker_id)
        .bind(op.generation)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn recover_running_operations(pool: &PgPool, worker_id: Uuid) -> Result<u64> {
    let result = sqlx::query(
        "UPDATE operations o
         SET status = 'pending', claimed_at = NULL, next_attempt_at = now(), updated_at = now()
         FROM servers s
         WHERE s.id = o.server_id AND o.worker_id = $1 AND o.status = 'running'
           AND o.generation = s.generation",
    )
    .bind(worker_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn retry_operation(pool: &PgPool, op: &OperationRecord, error: &str) -> Result<bool> {
    const MAX_ATTEMPTS: i32 = 5;
    let resumable_snapshot: bool = if op.kind == OperationKind::Snapshot {
        sqlx::query_scalar(
            "SELECT COALESCE(payload->>'stage' = 'deferred', false)
             FROM operations WHERE id = $1",
        )
        .bind(op.id)
        .fetch_one(pool)
        .await?
    } else {
        false
    };
    if op.attempt >= MAX_ATTEMPTS && !resumable_snapshot {
        fail_operation(pool, op, error).await?;
        return Ok(false);
    }
    let delay_seconds = i64::from(1_i32 << op.attempt.min(6));
    let result = sqlx::query(
        "UPDATE operations
         SET status = 'pending', last_error = $2, claimed_at = NULL,
             next_attempt_at = now() + make_interval(secs => $3), updated_at = now()
         WHERE id = $1 AND status = 'running' AND attempt = $4 AND worker_id = $5 AND generation = $6 AND server_id = $7 AND kind = $8",
    )
    .bind(op.id)
    .bind(error)
    .bind(delay_seconds)
    .bind(op.attempt)
    .bind(op.worker_id)
    .bind(op.generation)
    .bind(op.server_id)
    .bind(op.kind)
    .execute(pool)
    .await?;
    if result.rows_affected() != 1 {
        bail!("stale operation claim cannot schedule a retry");
    }
    Ok(true)
}

/// Reopen a snapshot attempt after a deferred upload, fencing new admissions
/// until the worker has saved and stopped the runtime again.
pub async fn begin_snapshot_retry(pool: &PgPool, op: &OperationRecord) -> Result<()> {
    if op.kind != OperationKind::Snapshot {
        bail!("snapshot retry requires a snapshot operation");
    }
    let mut tx = pool.begin().await?;
    let server = lock_server(&mut tx, op.server_id).await?;
    if server.generation != op.generation || server.desired_state != DesiredState::Running {
        bail!("snapshot retry no longer matches the current running server generation");
    }
    if !lock_claim(&mut tx, op, None).await? {
        bail!("snapshot retry claim is no longer running");
    }
    let placement = lock_placement(&mut tx, op.server_id)
        .await?
        .context("snapshot retry lost placement")?;
    validate_placement(op, &placement)?;
    if server.observed_state == ObservedState::Snapshotting && !placement.active {
        tx.commit().await?;
        return Ok(());
    }
    if server.observed_state != ObservedState::Online {
        bail!("snapshot retry no longer matches the online server generation");
    }
    if !placement.active {
        bail!("snapshot retry requires an active placement");
    }
    sqlx::query(
        "UPDATE placements SET active = false, updated_at = now()
         WHERE server_id = $1 AND generation = $2",
    )
    .bind(op.server_id)
    .bind(op.generation)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE servers SET observed_state = 'snapshotting', updated_at = now()
         WHERE id = $1 AND generation = $2",
    )
    .bind(op.server_id)
    .bind(op.generation)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE operations SET payload = payload || '{\"stage\":\"requested\"}'::jsonb,
         updated_at = now() WHERE id = $1",
    )
    .bind(op.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Keep the existing runtime routable when snapshot storage is temporarily
/// unavailable, and persist a stage that can safely begin a fresh capture.
pub async fn defer_snapshot_operation(
    pool: &PgPool,
    op: &OperationRecord,
    error: &str,
) -> Result<()> {
    if op.kind != OperationKind::Snapshot {
        bail!("only a snapshot operation can be deferred");
    }
    let mut tx = pool.begin().await?;
    let server = lock_server(&mut tx, op.server_id).await?;
    if server.generation != op.generation || server.desired_state != DesiredState::Running {
        bail!("snapshot deferral no longer matches the current running server generation");
    }
    if !lock_claim(&mut tx, op, None).await? {
        bail!("snapshot deferral claim is no longer running");
    }
    let placement = lock_placement(&mut tx, op.server_id)
        .await?
        .context("snapshot deferral lost placement")?;
    validate_placement(op, &placement)?;
    if server.observed_state == ObservedState::Online && placement.active {
        tx.commit().await?;
        return Ok(());
    }
    if server.observed_state != ObservedState::Snapshotting {
        bail!("snapshot deferral no longer matches the snapshotting server generation");
    }
    if placement.active {
        bail!("snapshot deferral requires the stopped placement fence");
    }
    sqlx::query(
        "UPDATE operations SET payload = payload || '{\"stage\":\"deferred\"}'::jsonb,
         updated_at = now() WHERE id = $1",
    )
    .bind(op.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE placements SET active = true, updated_at = now()
         WHERE server_id = $1 AND generation = $2",
    )
    .bind(op.server_id)
    .bind(op.generation)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE servers SET observed_state = 'online', last_error = $2, updated_at = now()
         WHERE id = $1 AND generation = $3",
    )
    .bind(op.server_id)
    .bind(error)
    .bind(op.generation)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn fail_operation(pool: &PgPool, op: &OperationRecord, error: &str) -> Result<()> {
    let mut tx = pool.begin().await?;
    let server = lock_server(&mut tx, op.server_id).await?;
    if server.generation != op.generation {
        bail!("stale failed operation generation");
    }
    if !lock_claim(&mut tx, op, Some(OperationStatus::Failed)).await? {
        tx.commit().await?;
        return Ok(());
    }
    let placement = lock_placement(&mut tx, op.server_id).await?;
    sqlx::query(
        "UPDATE operations SET status = 'failed', last_error = $2, updated_at = now() WHERE id = $1",
    )
    .bind(op.id)
    .bind(error)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE servers SET observed_state = 'failed', last_error = $2, updated_at = now()
         WHERE id = $1 AND generation = $3",
    )
    .bind(op.server_id)
    .bind(error)
    .bind(op.generation)
    .execute(&mut *tx)
    .await?;
    if let Some(placement) = placement {
        if op.kind == OperationKind::Start {
            validate_placement(op, &placement)?;
            // Failed readiness does not prove the container is absent. Keep the
            // reservation and routing fence until worker cleanup confirms removal.
            sqlx::query(
                "UPDATE placements SET active = true, updated_at = now() WHERE server_id = $1",
            )
            .bind(op.server_id)
            .execute(&mut *tx)
            .await?;
        } else if op.kind == OperationKind::Restore {
            validate_placement(op, &placement)?;
            if placement.active {
                bail!("failed restore unexpectedly has an active placement");
            }
            release_placement_resources(&mut tx, &placement).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

pub async fn complete_start(pool: &PgPool, op: &OperationRecord, storage_bytes: i64) -> Result<()> {
    let mut tx = pool.begin().await?;
    let server = lock_server(&mut tx, op.server_id).await?;
    if server.generation != op.generation {
        bail!("stale start operation generation");
    }
    if op.kind != OperationKind::Start {
        bail!("start completion requires a start operation");
    }
    if !lock_claim(&mut tx, op, Some(OperationStatus::Succeeded)).await? {
        tx.commit().await?;
        return Ok(());
    }
    let placement = lock_placement(&mut tx, op.server_id)
        .await?
        .context("started server lost placement")?;
    validate_placement(op, &placement)?;
    sqlx::query("UPDATE placements SET active = true, updated_at = now() WHERE server_id = $1 AND generation = $2")
        .bind(op.server_id)
        .bind(op.generation)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE operations SET status = 'succeeded', updated_at = now() WHERE id = $1")
        .bind(op.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE servers SET observed_state = 'online', storage_bytes = $2, last_error = NULL, updated_at = now()
         WHERE id = $1",
    )
    .bind(op.server_id)
    .bind(storage_bytes)
    .execute(&mut *tx)
    .await?;

    let owner_active: bool =
        sqlx::query_scalar("SELECT account_status = 'active' FROM users WHERE id = $1")
            .bind(server.owner_id)
            .fetch_one(&mut *tx)
            .await?;
    if !owner_active && server.desired_state != DesiredState::Deleted {
        sqlx::query("UPDATE servers SET desired_state = 'stopped' WHERE id = $1")
            .bind(op.server_id)
            .execute(&mut *tx)
            .await?;
    }
    if server.desired_state != DesiredState::Running || !owner_active {
        enqueue_operation(
            &mut tx,
            op.server_id,
            Some(placement.worker_id),
            OperationKind::Stop,
            op.generation,
            json!({"reason": if server.desired_state == DesiredState::Deleted { "delete requested during start" } else { "stop requested during start" }}),
        )
        .await?;
        sqlx::query(
            "UPDATE servers SET observed_state = 'stopping', updated_at = now() WHERE id = $1",
        )
        .bind(op.server_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn complete_stop(
    pool: &PgPool,
    op: &OperationRecord,
    snapshot_generation: i64,
    storage_bytes: i64,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    let server = lock_server(&mut tx, op.server_id).await?;
    if server.generation != op.generation {
        bail!("stale stop operation generation");
    }
    if !matches!(op.kind, OperationKind::Stop | OperationKind::Restart) {
        bail!("stop completion requires a stop or restart operation");
    }
    if !lock_claim(&mut tx, op, Some(OperationStatus::Succeeded)).await? {
        tx.commit().await?;
        return Ok(());
    }
    let placement = lock_placement(&mut tx, op.server_id)
        .await?
        .context("stopped server lost placement")?;
    validate_placement(op, &placement)?;
    // Only the snapshot publisher owns the recovery pointer. A worker report
    // cannot invent a generation, accept a partial upload, or rewind it.
    let valid_final_snapshot: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM snapshots
         WHERE server_id = $1 AND generation = $2 AND complete
           AND reason = 'shutdown' AND size_bytes = $3
           AND manifest_key <> '' AND checksum <> '' AND created_at >= $4)",
    )
    .bind(op.server_id)
    .bind(snapshot_generation)
    .bind(storage_bytes)
    .bind(op.created_at)
    .fetch_one(&mut *tx)
    .await?;
    if server.current_snapshot_generation != Some(snapshot_generation) || !valid_final_snapshot {
        bail!("stop completion requires the current complete final snapshot for this shutdown");
    }
    release_placement_resources(&mut tx, &placement).await?;
    sqlx::query("UPDATE operations SET status = 'succeeded', updated_at = now() WHERE id = $1")
        .bind(op.id)
        .execute(&mut *tx)
        .await?;
    let next_state = match server.desired_state {
        DesiredState::Running => ObservedState::Queued,
        DesiredState::Stopped => ObservedState::Sleeping,
        DesiredState::Deleted => ObservedState::Deleted,
    };
    sqlx::query(
        "UPDATE servers SET observed_state = $2, last_error = NULL, updated_at = now()
         WHERE id = $1",
    )
    .bind(op.server_id)
    .bind(next_state)
    .execute(&mut *tx)
    .await?;
    if server.desired_state == DesiredState::Running {
        sqlx::query(
            "INSERT INTO start_requests(server_id, reason) VALUES($1, 'restart')
             ON CONFLICT(server_id) DO UPDATE SET reason = EXCLUDED.reason, requested_at = now()",
        )
        .bind(op.server_id)
        .execute(&mut *tx)
        .await?;
    } else if server.desired_state == DesiredState::Deleted {
        enqueue_delete(&mut tx, op.server_id, op.generation).await?;
    }
    if server.desired_state != DesiredState::Deleted {
        sqlx::query(
            "INSERT INTO worker_snapshot_cache(worker_id, server_id, generation)
             VALUES($1,$2,$3)
             ON CONFLICT(worker_id, server_id) DO UPDATE
               SET generation = EXCLUDED.generation, updated_at = now()",
        )
        .bind(placement.worker_id)
        .bind(op.server_id)
        .bind(snapshot_generation)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Persist a resumable operation stage only for the current running claim.
pub async fn update_operation_payload(
    pool: &PgPool,
    op: &OperationRecord,
    patch: serde_json::Value,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    let server = lock_server(&mut tx, op.server_id).await?;
    if server.generation != op.generation {
        bail!("stale operation generation");
    }
    lock_claim(&mut tx, op, None).await?;
    sqlx::query("UPDATE operations SET payload = payload || $2, updated_at = now() WHERE id = $1")
        .bind(op.id)
        .bind(patch)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn complete_snapshot(pool: &PgPool, op: &OperationRecord, generation: i64) -> Result<()> {
    let mut tx = pool.begin().await?;
    let server = lock_server(&mut tx, op.server_id).await?;
    if op.kind != OperationKind::Snapshot || server.generation != op.generation {
        bail!("snapshot completion requires the current snapshot operation");
    }
    if !lock_claim(&mut tx, op, Some(OperationStatus::Succeeded)).await? {
        tx.commit().await?;
        return Ok(());
    }
    if server.observed_state != ObservedState::Snapshotting
        || server.desired_state != DesiredState::Running
    {
        bail!("snapshot completion requires a running server in the snapshotting state");
    }
    let placement = lock_placement(&mut tx, op.server_id)
        .await?
        .context("snapshot lost placement")?;
    validate_placement(op, &placement)?;
    let reason = op
        .payload
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .context("snapshot operation is missing its reason")?;
    let valid: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM snapshots WHERE server_id = $1 AND generation = $2
         AND complete AND reason::text = $3 AND created_at >= $4
         AND manifest_key <> '' AND checksum <> '')",
    )
    .bind(op.server_id)
    .bind(generation)
    .bind(reason)
    .bind(op.created_at)
    .fetch_one(&mut *tx)
    .await?;
    if placement.active || server.current_snapshot_generation != Some(generation) || !valid {
        bail!("snapshot completion requires a published complete stopped-runtime snapshot");
    }
    sqlx::query(
        "UPDATE placements SET active = true, updated_at = now()
         WHERE server_id = $1 AND generation = $2",
    )
    .bind(op.server_id)
    .bind(op.generation)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE operations SET status = 'succeeded', updated_at = now() WHERE id = $1")
        .bind(op.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE servers SET observed_state = 'online', storage_bytes =
             (SELECT size_bytes FROM snapshots WHERE server_id = $1 AND generation = $2),
             last_error = NULL, updated_at = now()
         WHERE id = $1 AND generation = $3",
    )
    .bind(op.server_id)
    .bind(generation)
    .bind(op.generation)
    .execute(&mut *tx)
    .await?;
    if reason == "export" {
        sqlx::query(
            "UPDATE world_exports SET source_generation = $2, stage = 'ready', updated_at = now()
             WHERE server_id = $1 AND status = 'pending' AND source_generation IS NULL",
        )
        .bind(op.server_id)
        .bind(generation)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn complete_restore(
    pool: &PgPool,
    op: &OperationRecord,
    target_generation: i64,
    snapshot_generation: i64,
    storage_bytes: i64,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    let server = lock_server(&mut tx, op.server_id).await?;
    if op.kind != OperationKind::Restore || server.generation != op.generation {
        bail!("restore completion requires the current restore operation");
    }
    if !lock_claim(&mut tx, op, Some(OperationStatus::Succeeded)).await? {
        tx.commit().await?;
        return Ok(());
    }
    let placement = lock_placement(&mut tx, op.server_id)
        .await?
        .context("restore lost placement")?;
    validate_placement(op, &placement)?;
    if placement.active {
        bail!("restore placement unexpectedly became active");
    }
    let source_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM snapshots
            WHERE server_id = $1 AND generation = $2 AND complete
              AND manifest_key <> '' AND checksum <> ''
         )",
    )
    .bind(op.server_id)
    .bind(target_generation)
    .fetch_one(&mut *tx)
    .await?;
    let restored_snapshot_valid: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM snapshots
            WHERE server_id = $1 AND generation = $2 AND complete
              AND reason = 'restore' AND size_bytes = $3
              AND manifest_key <> '' AND checksum <> '' AND created_at >= $4
         )",
    )
    .bind(op.server_id)
    .bind(snapshot_generation)
    .bind(storage_bytes)
    .bind(op.created_at)
    .fetch_one(&mut *tx)
    .await?;
    if !source_exists
        || snapshot_generation <= target_generation
        || server.current_snapshot_generation != Some(snapshot_generation)
        || !restored_snapshot_valid
    {
        bail!("restore completion requires a new monotonic published snapshot");
    }

    release_placement_resources(&mut tx, &placement).await?;
    sqlx::query("UPDATE operations SET status = 'succeeded', updated_at = now() WHERE id = $1")
        .bind(op.id)
        .execute(&mut *tx)
        .await?;
    let next_state = match server.desired_state {
        DesiredState::Running => ObservedState::Queued,
        DesiredState::Stopped => ObservedState::Sleeping,
        DesiredState::Deleted => ObservedState::Deleted,
    };
    sqlx::query(
        "UPDATE servers SET observed_state = $2, last_error = NULL, updated_at = now()
         WHERE id = $1",
    )
    .bind(op.server_id)
    .bind(next_state)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO worker_snapshot_cache(worker_id, server_id, generation)
         VALUES($1,$2,$3)
         ON CONFLICT(worker_id, server_id) DO UPDATE
           SET generation = EXCLUDED.generation, updated_at = now()",
    )
    .bind(placement.worker_id)
    .bind(op.server_id)
    .bind(snapshot_generation)
    .execute(&mut *tx)
    .await?;
    match server.desired_state {
        DesiredState::Running => {
            sqlx::query(
                "INSERT INTO start_requests(server_id, reason) VALUES($1, 'restore_complete')
                 ON CONFLICT(server_id) DO UPDATE
                   SET reason = EXCLUDED.reason, requested_at = now()",
            )
            .bind(op.server_id)
            .execute(&mut *tx)
            .await?;
        }
        DesiredState::Deleted => {
            enqueue_delete(&mut tx, op.server_id, op.generation).await?;
        }
        DesiredState::Stopped => {}
    }
    tx.commit().await?;
    Ok(())
}

pub async fn complete_reset(
    pool: &PgPool,
    op: &OperationRecord,
    snapshot_generation: i64,
    storage_bytes: i64,
    was_running: bool,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    let server = lock_server(&mut tx, op.server_id).await?;
    if op.kind != OperationKind::Reset || server.generation != op.generation {
        bail!("reset completion requires the current reset operation");
    }
    if !lock_claim(&mut tx, op, Some(OperationStatus::Succeeded)).await? {
        tx.commit().await?;
        return Ok(());
    }
    let valid_snapshot: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM snapshots WHERE server_id = $1 AND generation = $2
         AND complete AND reason = 'reset' AND size_bytes = $3
         AND manifest_key <> '' AND checksum <> '' AND created_at >= $4)",
    )
    .bind(op.server_id)
    .bind(snapshot_generation)
    .bind(storage_bytes)
    .bind(op.created_at)
    .fetch_one(&mut *tx)
    .await?;
    if server.current_snapshot_generation != Some(snapshot_generation) || !valid_snapshot {
        bail!("reset completion requires the authoritative replacement snapshot");
    }
    let placement = lock_placement(&mut tx, op.server_id).await?;
    if was_running {
        let placement = placement.context("running reset lost its placement")?;
        validate_placement(op, &placement)?;
        sqlx::query("UPDATE placements SET active = true, updated_at = now() WHERE server_id = $1")
            .bind(op.server_id)
            .execute(&mut *tx)
            .await?;
    } else if placement.is_some_and(|placement| placement.active) {
        bail!("sleeping reset left an active placement");
    }

    sqlx::query("UPDATE operations SET status = 'succeeded', updated_at = now() WHERE id = $1")
        .bind(op.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE servers SET observed_state = $2, storage_bytes = $3, last_error = NULL, updated_at = now()
         WHERE id = $1",
    )
    .bind(op.server_id)
    .bind(if was_running {
        ObservedState::Online
    } else {
        ObservedState::Sleeping
    })
    .bind(storage_bytes)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM worker_snapshot_cache WHERE server_id = $1")
        .bind(op.server_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO worker_snapshot_cache(worker_id, server_id, generation)
         VALUES($1, $2, $3) ON CONFLICT(worker_id, server_id) DO UPDATE
           SET generation = EXCLUDED.generation, updated_at = now()",
    )
    .bind(
        op.worker_id
            .context("reset operation has no worker claim")?,
    )
    .bind(op.server_id)
    .bind(snapshot_generation)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

// Physical cleanup is idempotent and runs without holding database row locks.
// Revalidate both before cleanup and before deleting the durable records.
async fn lock_delete(tx: &mut Transaction<'_, Postgres>, op: &OperationRecord) -> Result<()> {
    let server = lock_server(tx, op.server_id).await?;
    if op.kind != OperationKind::Delete
        || server.generation != op.generation
        || server.desired_state != DesiredState::Deleted
        || server.observed_state != ObservedState::Deleted
    {
        bail!("delete requires the current operation and a deleted server");
    }
    lock_claim(tx, op, None).await?;
    if lock_placement(tx, op.server_id)
        .await?
        .is_some_and(|p| p.active)
    {
        bail!("refusing to delete server data while placement is active");
    }
    Ok(())
}

pub async fn validate_delete(pool: &PgPool, op: &OperationRecord) -> Result<()> {
    let mut tx = pool.begin().await?;
    lock_delete(&mut tx, op).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn complete_delete(pool: &PgPool, op: &OperationRecord) -> Result<()> {
    let mut tx = pool.begin().await?;
    lock_delete(&mut tx, op).await?;
    sqlx::query("DELETE FROM servers WHERE id = $1")
        .bind(op.server_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

// Caller locks the server first, preserving lifecycle lock order. The
// persisted claim, rather than a worker-supplied DTO, authorizes each mutation.
async fn lock_claim(
    tx: &mut Transaction<'_, Postgres>,
    claim: &OperationRecord,
    terminal: Option<OperationStatus>,
) -> Result<bool> {
    let stored = sqlx::query_as::<_, OperationRecord>(
        "SELECT id, server_id, worker_id, kind, status, generation, attempt, payload,
                last_error, created_at, updated_at FROM operations WHERE id = $1 FOR UPDATE",
    )
    .bind(claim.id)
    .fetch_optional(&mut **tx)
    .await?
    .context("operation not found")?;
    if stored.server_id != claim.server_id
        || stored.worker_id != claim.worker_id
        || stored.generation != claim.generation
        || stored.kind != claim.kind
        || stored.attempt != claim.attempt
    {
        bail!("stale operation claim identity or attempt");
    }
    if Some(stored.status) == terminal {
        return Ok(false);
    }
    if stored.status != OperationStatus::Running {
        bail!("operation claim is no longer running");
    }
    Ok(true)
}

fn validate_placement(
    op: &OperationRecord,
    placement: &crate::model::PlacementRecord,
) -> Result<()> {
    if Some(placement.worker_id) != op.worker_id || placement.generation != op.generation {
        bail!("operation claim does not match placement");
    }
    Ok(())
}
