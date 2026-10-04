use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    contracts::worker::{OperationPayload, OperationProgress, OperationStage},
    control_plane::lifecycle::{
        begin_snapshot_retry as lifecycle_begin_snapshot_retry, complete_delete, complete_reset,
        complete_restore, complete_snapshot, complete_start, complete_stop,
        deactivate_placement_after_stop, defer_snapshot_operation, mark_runtime_lost,
        retry_operation as retry_worker_operation, update_operation_payload, validate_delete,
    },
    model::{OperationKind, OperationRecord, OperationStatus},
};

use super::{ApiError, AppState, worker_auth};

const MAX_WORKER_ERROR_BYTES: usize = 1_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HeartbeatRequest {
    #[serde(default = "unknown_worker_architecture")]
    runtime_architecture: String,
    #[serde(default)]
    supports_ci_previews: bool,
}

fn unknown_worker_architecture() -> String {
    "unknown".to_owned()
}
#[derive(Debug, Serialize)]
pub(super) struct RecoveryReport {
    operations: u64,
    exports: u64,
}

pub(super) async fn recover(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<RecoveryReport>, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let mut tx = state.pool.begin().await?;
    let operations = sqlx::query(
        "UPDATE operations SET status = 'pending', claimed_at = NULL,
             next_attempt_at = now(), updated_at = now()
         WHERE worker_id = $1 AND status = 'running'",
    )
    .bind(worker_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let exports = sqlx::query(
        "UPDATE world_exports SET status = 'pending', worker_id = NULL,
             stage = CASE WHEN source_generation IS NULL THEN 'waiting_snapshot' ELSE 'ready' END,
             updated_at = now()
         WHERE status = 'running' AND worker_id = $1",
    )
    .bind(worker_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    // Console commands are not retried after a worker interruption: the
    // command may have executed before its acknowledgement was lost.
    sqlx::query(
        "UPDATE console_commands SET status = 'failed',
             error = 'execution_outcome_unknown_after_worker_recovery',
             completed_at = now(), updated_at = now()
         WHERE status = 'running' AND worker_id = $1",
    )
    .bind(worker_id)
    .execute(&mut *tx)
    .await?;
    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    tx.commit().await?;
    Ok(Json(RecoveryReport {
        operations,
        exports,
    }))
}

pub(super) async fn heartbeat(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<HeartbeatRequest>,
) -> Result<StatusCode, ApiError> {
    if !matches!(
        request.runtime_architecture.as_str(),
        "unknown" | "x86_64-linux" | "aarch64-linux"
    ) {
        return Err(ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_worker_architecture",
            "worker reported an unsupported runtime architecture",
        ));
    }
    let mut authorization = state.pool.begin().await?;
    let worker_id = worker_auth::lock_authenticated_worker_id(&mut authorization, &headers).await?;
    let lease_updated = sqlx::query(
        "UPDATE worker_credentials SET lease_expires_at = now() + interval '30 seconds'
         WHERE worker_id = $1 AND lease_expires_at > now()",
    )
    .bind(worker_id)
    .execute(&mut *authorization)
    .await?;
    if lease_updated.rows_affected() != 1 {
        return Err(worker_auth::invalid_worker_token());
    }
    let result = sqlx::query(
        "UPDATE workers SET heartbeat_at = now(), runtime_architecture = $2,
             supports_ci_previews = $3 WHERE id = $1",
    )
    .bind(worker_id)
    .bind(&request.runtime_architecture)
    .bind(request.supports_ci_previews)
    .execute(&mut *authorization)
    .await?;
    if result.rows_affected() != 1 {
        return Err(worker_auth::invalid_worker_token());
    }
    authorization.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn claim_operation(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Option<OperationRecord>>, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("kurbis-worker-claim:{worker_id}"))
        .execute(&mut *tx)
        .await?;

    let running_server = lock_server_with_worker_operation(&mut tx, worker_id, true).await?;
    let mut operation = if let Some((server_id, generation)) = running_server {
        sqlx::query_as::<_, OperationRecord>(
            "SELECT id, server_id, worker_id, kind, status, generation, attempt, payload,
                        last_error, created_at, updated_at
                 FROM operations
                 WHERE server_id = $1 AND worker_id = $2 AND generation = $3
                   AND status = 'running'
                 ORDER BY claimed_at, id
                 FOR UPDATE LIMIT 1",
        )
        .bind(server_id)
        .bind(worker_id)
        .bind(generation)
        .fetch_optional(&mut *tx)
        .await?
    } else {
        None
    };
    if operation.is_none()
        && let Some((server_id, generation)) =
            lock_server_with_worker_operation(&mut tx, worker_id, false).await?
    {
        operation = sqlx::query_as::<_, OperationRecord>(
            "SELECT id, server_id, worker_id, kind, status, generation, attempt, payload,
                        last_error, created_at, updated_at
                 FROM operations
                 WHERE server_id = $1 AND worker_id = $2 AND generation = $3
                   AND status = 'pending' AND next_attempt_at <= now()
                 ORDER BY created_at, id
                 FOR UPDATE LIMIT 1",
        )
        .bind(server_id)
        .bind(worker_id)
        .bind(generation)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(operation) = operation.as_mut() {
            let attempt = operation
                .attempt
                .checked_add(1)
                .ok_or_else(stale_operation)?;
            sqlx::query(
                "UPDATE operations SET status = 'running', attempt = $2,
                         claimed_at = now(), updated_at = now()
                     WHERE id = $1 AND worker_id = $3 AND generation = $4 AND status = 'pending'",
            )
            .bind(operation.id)
            .bind(attempt)
            .bind(worker_id)
            .bind(generation)
            .execute(&mut *tx)
            .await?;
            operation.attempt = attempt;
            operation.status = OperationStatus::Running;
        }
    }

    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    tx.commit().await?;
    Ok(Json(operation))
}

async fn lock_server_with_worker_operation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    worker_id: Uuid,
    running: bool,
) -> Result<Option<(Uuid, i64)>, ApiError> {
    let server = if running {
        sqlx::query_as::<_, (Uuid, i64)>(
            "SELECT s.id, s.generation
             FROM servers s
             WHERE EXISTS (
                 SELECT 1 FROM operations o
                 WHERE o.server_id = s.id AND o.worker_id = $1
                   AND o.generation = s.generation AND o.status = 'running'
             )
             ORDER BY (
                 SELECT min(o.claimed_at) FROM operations o
                 WHERE o.server_id = s.id AND o.worker_id = $1
                   AND o.generation = s.generation AND o.status = 'running'
             ), s.id
             FOR UPDATE OF s SKIP LOCKED LIMIT 1",
        )
    } else {
        sqlx::query_as::<_, (Uuid, i64)>(
            "SELECT s.id, s.generation
             FROM servers s
             WHERE EXISTS (
                 SELECT 1 FROM operations o
                 WHERE o.server_id = s.id AND o.worker_id = $1
                   AND o.generation = s.generation AND o.status = 'pending'
                   AND o.next_attempt_at <= now()
             )
             ORDER BY (
                 SELECT min(o.created_at) FROM operations o
                 WHERE o.server_id = s.id AND o.worker_id = $1
                   AND o.generation = s.generation AND o.status = 'pending'
                   AND o.next_attempt_at <= now()
             ), s.id
             FOR UPDATE OF s SKIP LOCKED LIMIT 1",
        )
    };
    Ok(server.bind(worker_id).fetch_optional(&mut **tx).await?)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OperationFenceRequest {
    generation: i64,
    attempt: i32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OperationProgressRequest {
    generation: i64,
    attempt: i32,
    patch: OperationProgress,
}

pub(super) async fn operation_progress(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path(operation_id): Path<Uuid>,
    Json(request): Json<OperationProgressRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let operation = current_operation(
        &state,
        worker_id,
        operation_id,
        request.generation,
        request.attempt,
        false,
    )
    .await?;
    if !request.patch.valid_for(operation.kind) {
        return Err(invalid_progress());
    }
    update_operation_payload(&state.pool, &operation, request.patch)
        .await
        .map_err(super::lifecycle_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OperationErrorRequest {
    generation: i64,
    attempt: i32,
    error: String,
}

#[derive(Debug, Serialize)]
pub(super) struct RetryResponse {
    retry_scheduled: bool,
}

pub(super) async fn retry_operation(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path(operation_id): Path<Uuid>,
    Json(request): Json<OperationErrorRequest>,
) -> Result<Json<RetryResponse>, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let error = bounded_worker_error(&request.error)?;
    let operation = sqlx::query_as::<_, OperationRecord>(
        "SELECT id, server_id, worker_id, kind, status, generation, attempt, payload,
                last_error, created_at, updated_at
         FROM operations WHERE id = $1 AND worker_id = $2",
    )
    .bind(operation_id)
    .bind(worker_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(operation_not_found)?;
    if operation.generation != request.generation || operation.attempt != request.attempt {
        return Err(stale_operation());
    }
    if matches!(
        operation.status,
        OperationStatus::Pending | OperationStatus::Failed
    ) && operation.last_error.as_deref() == Some(error)
    {
        return Ok(Json(RetryResponse {
            retry_scheduled: operation.status == OperationStatus::Pending,
        }));
    }
    if operation.status != OperationStatus::Running {
        return Err(stale_operation());
    }
    let retry_scheduled = retry_worker_operation(&state.pool, &operation, error)
        .await
        .map_err(super::lifecycle_error)?;
    Ok(Json(RetryResponse { retry_scheduled }))
}

pub(super) async fn begin_snapshot_retry(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path(operation_id): Path<Uuid>,
    Json(request): Json<OperationFenceRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let operation = current_operation(
        &state,
        worker_id,
        operation_id,
        request.generation,
        request.attempt,
        false,
    )
    .await?;
    if operation.kind == OperationKind::Snapshot
        && decode_operation_payload(&operation)?.stage == OperationStage::Requested
        && snapshot_retry_state(
            &state.pool,
            &operation,
            worker_id,
            "snapshotting",
            false,
            None,
        )
        .await?
    {
        return Ok(StatusCode::NO_CONTENT);
    }
    lifecycle_begin_snapshot_retry(&state.pool, &operation)
        .await
        .map_err(super::lifecycle_error)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn defer_snapshot(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path(operation_id): Path<Uuid>,
    Json(request): Json<OperationErrorRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let operation = current_operation(
        &state,
        worker_id,
        operation_id,
        request.generation,
        request.attempt,
        false,
    )
    .await?;
    let error = bounded_worker_error(&request.error)?;
    if operation.kind == OperationKind::Snapshot
        && decode_operation_payload(&operation)?.stage == OperationStage::Deferred
        && snapshot_retry_state(
            &state.pool,
            &operation,
            worker_id,
            "online",
            true,
            Some(error),
        )
        .await?
    {
        return Ok(StatusCode::NO_CONTENT);
    }
    defer_snapshot_operation(&state.pool, &operation, error)
        .await
        .map_err(super::lifecycle_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CompleteOperationRequest {
    generation: i64,
    attempt: i32,
    completion: OperationCompletion,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum OperationCompletion {
    Start {
        storage_bytes: i64,
    },
    Snapshot {
        snapshot_generation: i64,
    },
    Stop {
        snapshot_generation: i64,
        storage_bytes: i64,
    },
    Restore {
        target_generation: i64,
        snapshot_generation: i64,
        storage_bytes: i64,
    },
    Reset {
        snapshot_generation: i64,
        storage_bytes: i64,
        was_running: bool,
    },
    Delete,
}

pub(super) async fn complete_operation(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path(operation_id): Path<Uuid>,
    Json(request): Json<CompleteOperationRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    if matches!(&request.completion, OperationCompletion::Delete) {
        let still_assigned: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM operations WHERE id = $1 AND worker_id = $2)",
        )
        .bind(operation_id)
        .bind(worker_id)
        .fetch_one(&state.pool)
        .await?;
        if !still_assigned {
            let operation_exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM operations WHERE id = $1)")
                    .bind(operation_id)
                    .fetch_one(&state.pool)
                    .await?;
            if operation_exists {
                return Err(operation_not_found());
            }
            return Ok(StatusCode::NO_CONTENT);
        }
    }
    let operation = current_operation(
        &state,
        worker_id,
        operation_id,
        request.generation,
        request.attempt,
        true,
    )
    .await?;
    let operation_payload = decode_operation_payload(&operation)?;

    let result = match (operation.kind, request.completion) {
        (OperationKind::Start, OperationCompletion::Start { storage_bytes }) => {
            complete_start(&state.pool, &operation, storage_bytes).await
        }
        (
            OperationKind::Snapshot,
            OperationCompletion::Snapshot {
                snapshot_generation,
            },
        ) => complete_snapshot(&state.pool, &operation, snapshot_generation).await,
        (
            OperationKind::Stop | OperationKind::Restart,
            OperationCompletion::Stop {
                snapshot_generation,
                storage_bytes,
            },
        ) => complete_stop(&state.pool, &operation, snapshot_generation, storage_bytes).await,
        (
            OperationKind::Restore,
            OperationCompletion::Restore {
                target_generation,
                snapshot_generation,
                storage_bytes,
            },
        ) => {
            if operation_payload.target_generation != Some(target_generation) {
                return Err(stale_operation());
            }
            complete_restore(
                &state.pool,
                &operation,
                target_generation,
                snapshot_generation,
                storage_bytes,
            )
            .await
        }
        (
            OperationKind::Reset,
            OperationCompletion::Reset {
                snapshot_generation,
                storage_bytes,
                was_running,
            },
        ) => {
            if operation_payload.was_running != Some(was_running) {
                return Err(stale_operation());
            }
            complete_reset(
                &state.pool,
                &operation,
                snapshot_generation,
                storage_bytes,
                was_running,
            )
            .await
        }
        (OperationKind::Delete, OperationCompletion::Delete) => {
            complete_delete(&state.pool, &operation).await
        }
        _ => return Err(invalid_completion()),
    };
    result.map_err(super::lifecycle_error)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn validate_delete_operation(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path(operation_id): Path<Uuid>,
    Json(request): Json<OperationFenceRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let operation = current_operation(
        &state,
        worker_id,
        operation_id,
        request.generation,
        request.attempt,
        false,
    )
    .await?;
    if operation.kind != OperationKind::Delete {
        return Err(invalid_completion());
    }
    validate_delete(&state.pool, &operation)
        .await
        .map_err(super::lifecycle_error)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn deactivate_placement(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path(operation_id): Path<Uuid>,
    Json(request): Json<OperationFenceRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let operation = current_operation(
        &state,
        worker_id,
        operation_id,
        request.generation,
        request.attempt,
        false,
    )
    .await?;
    if !matches!(
        operation.kind,
        OperationKind::Stop
            | OperationKind::Restart
            | OperationKind::Snapshot
            | OperationKind::Reset
    ) {
        return Err(invalid_completion());
    }
    deactivate_placement_after_stop(&state.pool, worker_id, &operation)
        .await
        .map_err(super::lifecycle_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeLostRequest {
    server_id: Uuid,
    generation: i64,
    error: String,
}

pub(super) async fn runtime_lost(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<RuntimeLostRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    if request.generation <= 0 {
        return Err(stale_operation());
    }
    mark_runtime_lost(
        &state.pool,
        worker_id,
        request.server_id,
        request.generation,
        bounded_worker_error(&request.error)?,
    )
    .await
    .map_err(super::lifecycle_error)?;
    Ok(StatusCode::NO_CONTENT)
}

mod storage;
pub(super) use storage::{
    claim_export, complete_export, fail_export, publish_snapshot, verify_snapshot_manifest,
};

async fn current_operation(
    state: &AppState,
    worker_id: Uuid,
    operation_id: Uuid,
    generation: i64,
    attempt: i32,
    allow_succeeded: bool,
) -> Result<OperationRecord, ApiError> {
    if generation <= 0 || attempt < 1 {
        return Err(stale_operation());
    }
    let operation = sqlx::query_as::<_, OperationRecord>(
        "SELECT id, server_id, worker_id, kind, status, generation, attempt, payload,
                last_error, created_at, updated_at
         FROM operations WHERE id = $1 AND worker_id = $2",
    )
    .bind(operation_id)
    .bind(worker_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(operation_not_found)?;
    if operation.generation != generation || operation.attempt != attempt {
        return Err(stale_operation());
    }
    let status_allowed = operation.status == OperationStatus::Running
        || (allow_succeeded && operation.status == OperationStatus::Succeeded);
    if !status_allowed {
        return Err(stale_operation());
    }
    Ok(operation)
}

async fn snapshot_retry_state(
    pool: &sqlx::PgPool,
    operation: &OperationRecord,
    worker_id: Uuid,
    observed_state: &str,
    placement_active: bool,
    server_error: Option<&str>,
) -> Result<bool, ApiError> {
    let state = sqlx::query(
        "SELECT s.observed_state::text AS observed_state, s.last_error, p.active
         FROM servers s JOIN placements p ON p.server_id = s.id
         WHERE s.id = $1 AND s.generation = $2
           AND p.worker_id = $3 AND p.generation = $2",
    )
    .bind(operation.server_id)
    .bind(operation.generation)
    .bind(worker_id)
    .fetch_optional(pool)
    .await?;
    let Some(state) = state else {
        return Ok(false);
    };
    let current_state: String = state.try_get("observed_state")?;
    let active: bool = state.try_get("active")?;
    let error: Option<String> = state.try_get("last_error")?;
    Ok(current_state == observed_state
        && active == placement_active
        && server_error.is_none_or(|expected| error.as_deref() == Some(expected)))
}

async fn lock_worker_operation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    worker_id: Uuid,
    operation_id: Uuid,
    server_id: Uuid,
    generation: i64,
    attempt: i32,
) -> Result<OperationRecord, ApiError> {
    let operation = sqlx::query_as::<_, OperationRecord>(
        "SELECT id, server_id, worker_id, kind, status, generation, attempt, payload,
                last_error, created_at, updated_at
         FROM operations WHERE id = $1 AND server_id = $2 AND worker_id = $3 FOR UPDATE",
    )
    .bind(operation_id)
    .bind(server_id)
    .bind(worker_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(operation_not_found)?;
    if operation.generation != generation
        || operation.attempt != attempt
        || operation.status != OperationStatus::Running
    {
        return Err(stale_operation());
    }
    Ok(operation)
}

fn decode_operation_payload(operation: &OperationRecord) -> Result<OperationPayload, ApiError> {
    serde_json::from_value(operation.payload.clone()).map_err(|_| {
        ApiError::with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_stored_operation_payload",
            "operation state is unavailable",
        )
    })
}

fn bounded_worker_error(error: &str) -> Result<&str, ApiError> {
    if error.is_empty() || error.len() > MAX_WORKER_ERROR_BYTES {
        return Err(ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_worker_error",
            "worker error detail is invalid",
        ));
    }
    Ok(error)
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn stale_operation() -> ApiError {
    ApiError::with_code(
        StatusCode::CONFLICT,
        "stale_worker_assignment",
        "worker assignment is stale or no longer active",
    )
}

fn operation_not_found() -> ApiError {
    ApiError::with_code(
        StatusCode::NOT_FOUND,
        "operation_not_found",
        "operation not found",
    )
}

fn server_not_found() -> ApiError {
    ApiError::with_code(
        StatusCode::NOT_FOUND,
        "server_not_found",
        "server not found",
    )
}

fn invalid_progress() -> ApiError {
    ApiError::with_code(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_operation_progress",
        "worker operation progress is invalid",
    )
}

fn invalid_completion() -> ApiError {
    ApiError::with_code(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_operation_completion",
        "worker completion does not match the assigned operation",
    )
}

fn invalid_snapshot() -> ApiError {
    ApiError::with_code(
        StatusCode::CONFLICT,
        "snapshot_not_publishable",
        "snapshot is not a complete current worker result",
    )
}

fn invalid_export() -> ApiError {
    ApiError::with_code(
        StatusCode::CONFLICT,
        "export_not_completable",
        "world export is no longer assigned to this worker",
    )
}

pub(super) fn export_not_found() -> ApiError {
    ApiError::with_code(
        StatusCode::NOT_FOUND,
        "export_not_found",
        "world export not found",
    )
}
