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
    let op = sqlx::query_as::<_, OperationRecord>(
        "SELECT id, server_id, worker_id, kind, status, generation, attempt, payload,
                last_error, created_at, updated_at
         FROM operations
         WHERE status = 'pending' AND next_attempt_at <= now()
           AND (worker_id = $1 OR (worker_id IS NULL AND kind = 'delete'))
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
         SET worker_id = COALESCE(worker_id, $2), status = 'running', attempt = attempt + 1,
             claimed_at = now(), updated_at = now()
         WHERE id = $1",
    )
    .bind(op.id)
    .bind(worker_id)
    .execute(&mut *tx)
    .await?;
    op.worker_id = Some(worker_id);
    op.status = OperationStatus::Running;
    op.attempt += 1;
    tx.commit().await?;
    Ok(Some(op))
}

pub async fn recover_running_operations(pool: &PgPool, worker_id: Uuid) -> Result<u64> {
    let result = sqlx::query(
        "UPDATE operations
         SET status = 'pending', claimed_at = NULL, next_attempt_at = now(), updated_at = now()
         WHERE worker_id = $1 AND status = 'running'",
    )
    .bind(worker_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn retry_operation(pool: &PgPool, op: &OperationRecord, error: &str) -> Result<bool> {
    const MAX_ATTEMPTS: i32 = 5;
    if op.attempt >= MAX_ATTEMPTS {
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

pub async fn fail_operation(pool: &PgPool, op: &OperationRecord, error: &str) -> Result<()> {
    let mut tx = pool.begin().await?;
    let server = lock_server(&mut tx, op.server_id).await?;
    if server.generation != op.generation {
        bail!("stale failed operation generation");
    }
    if !lock_claim(&mut tx, op, OperationStatus::Failed).await? {
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
    if op.kind == OperationKind::Start
        && let Some(placement) = placement
    {
        validate_placement(op, &placement)?;
        // Failed readiness does not prove the container is absent. Keep the
        // reservation and routing fence until worker cleanup confirms removal.
        sqlx::query("UPDATE placements SET active = true, updated_at = now() WHERE server_id = $1")
            .bind(op.server_id)
            .execute(&mut *tx)
            .await?;
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
    if !lock_claim(&mut tx, op, OperationStatus::Succeeded).await? {
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
    if !lock_claim(&mut tx, op, OperationStatus::Succeeded).await? {
        tx.commit().await?;
        return Ok(());
    }
    let placement = lock_placement(&mut tx, op.server_id)
        .await?
        .context("stopped server lost placement")?;
    validate_placement(op, &placement)?;
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
        "UPDATE servers SET observed_state = $4, storage_bytes = $2,
            current_snapshot_generation = $3, last_error = NULL, updated_at = now()
         WHERE id = $1",
    )
    .bind(op.server_id)
    .bind(storage_bytes)
    .bind(snapshot_generation)
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

// Caller locks the server first, preserving lifecycle lock order. The
// persisted claim, rather than a worker-supplied DTO, authorizes each mutation.
async fn lock_claim(
    tx: &mut Transaction<'_, Postgres>,
    claim: &OperationRecord,
    terminal: OperationStatus,
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
    if stored.status == terminal {
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
