use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use sqlx::Row;
use uuid::Uuid;

use crate::{
    contracts::worker::{WorkerAssignment, WorkerSnapshotCacheHint, WorkerSnapshotReservation},
    model::{OperationKind, OperationRecord, OperationStatus, SnapshotReason, SnapshotRecord},
    snapshot::{ObjectKind, WorkerObjectKey, physical_object_path},
};

use super::{
    ApiError, AppState, worker_auth,
    worker_control::verify_snapshot_manifest,
    worker_state::{ContextQuery, lock_assignment, validate_assignment},
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReserveSnapshotRequest {
    operation_id: Uuid,
    generation: i64,
    attempt: i32,
    reason: SnapshotReason,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SnapshotMetadataRequest {
    operation_id: Uuid,
    generation: i64,
    attempt: i32,
    manifest_key: String,
    size_bytes: i64,
    checksum: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteManifestsRequest {
    operation_id: Uuid,
    generation: i64,
    attempt: i32,
}

pub(super) async fn report_snapshot_cache_hint(
    State(state): State<std::sync::Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(server_id): Path<Uuid>,
    Json(request): Json<WorkerSnapshotCacheHint>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let mut tx = state.pool.begin().await?;
    if request.snapshot_generation <= 0
        || matches!(request.assignment, WorkerAssignment::Export { .. })
    {
        return Err(invalid_snapshot_assignment());
    }
    lock_assignment(&mut tx, worker_id, server_id, request.assignment).await?;
    let complete: Option<i64> = sqlx::query_scalar(
        "SELECT generation FROM snapshots
         WHERE server_id = $1 AND generation = $2 AND complete
           AND manifest_key <> '' AND checksum <> ''
         FOR SHARE",
    )
    .bind(server_id)
    .bind(request.snapshot_generation)
    .fetch_optional(&mut *tx)
    .await?;
    if complete.is_none() {
        return Err(invalid_snapshot_assignment());
    }
    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    sqlx::query(
        "INSERT INTO worker_snapshot_hints(worker_id, server_id, snapshot_generation)
         VALUES($1, $2, $3)
         ON CONFLICT(worker_id, server_id)
         DO UPDATE SET snapshot_generation = EXCLUDED.snapshot_generation, reported_at = now()",
    )
    .bind(worker_id)
    .bind(server_id)
    .bind(request.snapshot_generation)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn reserve_snapshot(
    State(state): State<std::sync::Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(server_id): Path<Uuid>,
    Json(request): Json<ReserveSnapshotRequest>,
) -> Result<Json<WorkerSnapshotReservation>, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let mut tx = state.pool.begin().await?;
    let server_generation: Option<i64> =
        sqlx::query_scalar("SELECT generation FROM servers WHERE id = $1 FOR UPDATE")
            .bind(server_id)
            .fetch_optional(&mut *tx)
            .await?;
    if server_generation != Some(request.generation)
        || request.generation <= 0
        || request.attempt < 1
    {
        return Err(invalid_snapshot_assignment());
    }
    let operation = current_operation(
        &mut tx,
        worker_id,
        server_id,
        request.operation_id,
        request.generation,
        request.attempt,
    )
    .await?;
    let expected_reason = snapshot_reason_for_operation(&operation)?;
    if request.reason != expected_reason {
        return Err(invalid_snapshot_assignment());
    }
    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    let generation: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(generation), 0) + 1 FROM snapshots WHERE server_id = $1",
    )
    .bind(server_id)
    .fetch_one(&mut *tx)
    .await?;
    let id = Uuid::new_v4();
    let created_at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete)
         VALUES($1,$2,$3,'',0,'',$4,false) RETURNING created_at",
    )
    .bind(id).bind(server_id).bind(generation).bind(request.reason).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(WorkerSnapshotReservation {
        id,
        server_id,
        generation,
        reason: request.reason,
        created_at,
    }))
}

pub(super) async fn publish_snapshot_metadata(
    State(state): State<std::sync::Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path((server_id, snapshot_generation)): Path<(Uuid, i64)>,
    Json(request): Json<SnapshotMetadataRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    if request.generation <= 0
        || request.attempt < 1
        || snapshot_generation <= 0
        || !(0..=crate::model::SERVER_STORAGE_BYTES).contains(&request.size_bytes)
        || !is_lower_hex(&request.checksum, 64)
    {
        return Err(invalid_snapshot_assignment());
    }
    let mut tx = state.pool.begin().await?;
    let server_generation: Option<i64> =
        sqlx::query_scalar("SELECT generation FROM servers WHERE id = $1 FOR UPDATE")
            .bind(server_id)
            .fetch_optional(&mut *tx)
            .await?;
    if server_generation != Some(request.generation) {
        return Err(invalid_snapshot_assignment());
    }
    let operation = current_operation(
        &mut tx,
        worker_id,
        server_id,
        request.operation_id,
        request.generation,
        request.attempt,
    )
    .await?;
    let snapshot = sqlx::query(
        "SELECT id, reason::text AS reason, created_at, complete FROM snapshots
         WHERE server_id = $1 AND generation = $2 FOR UPDATE",
    )
    .bind(server_id)
    .bind(snapshot_generation)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(invalid_snapshot_assignment)?;
    let snapshot_id: Uuid = snapshot.try_get("id")?;
    let snapshot_reason: String = snapshot.try_get("reason")?;
    let created_at: chrono::DateTime<chrono::Utc> = snapshot.try_get("created_at")?;
    let complete: bool = snapshot.try_get("complete")?;
    let expected_reason = snapshot_reason_for_operation(&operation)?;
    let key =
        WorkerObjectKey::parse(&request.manifest_key).map_err(|_| invalid_snapshot_assignment())?;
    if complete
        || created_at < operation.created_at
        || snapshot_reason != snapshot_reason_name(expected_reason)
        || !matches!(key.kind(), ObjectKind::Manifest { generation, checksum } if generation == snapshot_generation && checksum == request.checksum)
    {
        return Err(invalid_snapshot_assignment());
    }
    verify_snapshot_manifest(
        state.object_store.as_ref(),
        server_id,
        snapshot_generation,
        &request.manifest_key,
        &request.checksum,
        request.size_bytes,
    )
    .await?;
    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    let updated = sqlx::query(
        "UPDATE snapshots SET manifest_key = $2, size_bytes = $3, checksum = $4, complete = true
         WHERE id = $1 AND NOT complete",
    )
    .bind(snapshot_id)
    .bind(request.manifest_key)
    .bind(request.size_bytes)
    .bind(request.checksum)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(invalid_snapshot_assignment());
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn current_snapshot(
    State(state): State<std::sync::Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(server_id): Path<Uuid>,
    Query(query): Query<ContextQuery>,
) -> Result<Json<Option<SnapshotRecord>>, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let assignment = query.assignment()?;
    validate_assignment(&state.pool, worker_id, server_id, assignment).await?;
    let generation: Option<i64> =
        sqlx::query_scalar("SELECT current_snapshot_generation FROM servers WHERE id=$1")
            .bind(server_id)
            .fetch_optional(&state.pool)
            .await?
            .flatten();
    let Some(generation) = generation else {
        return Ok(Json(None));
    };
    snapshot_record(&state, server_id, generation)
        .await
        .map(|record| Json(Some(record)))
}

pub(super) async fn snapshot(
    State(state): State<std::sync::Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path((server_id, generation)): Path<(Uuid, i64)>,
    Query(query): Query<ContextQuery>,
) -> Result<Json<SnapshotRecord>, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let assignment = query.assignment()?;
    validate_assignment(&state.pool, worker_id, server_id, assignment).await?;
    snapshot_record(&state, server_id, generation)
        .await
        .map(Json)
}

pub(super) async fn delete_manifests(
    State(state): State<std::sync::Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(server_id): Path<Uuid>,
    Json(request): Json<DeleteManifestsRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let mut tx = state.pool.begin().await?;
    let operation = current_operation(
        &mut tx,
        worker_id,
        server_id,
        request.operation_id,
        request.generation,
        request.attempt,
    )
    .await?;
    if operation.kind != OperationKind::Delete {
        return Err(invalid_snapshot_assignment());
    }
    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT manifest_key FROM snapshots WHERE server_id=$1 AND manifest_key <> ''",
    )
    .bind(server_id)
    .fetch_all(&mut *tx)
    .await?;
    for key in keys {
        let path =
            physical_object_path(server_id, &key).map_err(|_| invalid_snapshot_assignment())?;
        match state.object_store.delete(&path).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
            Err(_) => {
                return Err(ApiError::with_code(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "snapshot_cleanup_failed",
                    "snapshot manifest cleanup failed",
                ));
            }
        }
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn export_running(
    State(state): State<std::sync::Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(export_id): Path<Uuid>,
    Query(query): Query<ContextQuery>,
) -> Result<Json<bool>, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let assignment = query.assignment()?;
    let WorkerAssignment::Export {
        export_id: assigned_id,
        generation,
        attempt,
    } = assignment
    else {
        return Err(invalid_snapshot_assignment());
    };
    if assigned_id != export_id {
        return Err(invalid_snapshot_assignment());
    }
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM world_exports WHERE id=$1 AND worker_id=$2 AND source_generation=$3 AND attempt=$4)",
    ).bind(export_id).bind(worker_id).bind(generation).bind(attempt).fetch_one(&state.pool).await?;
    if !exists {
        return Err(invalid_snapshot_assignment());
    }
    let running: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM world_exports WHERE id=$1 AND worker_id=$2 AND source_generation=$3 AND attempt=$4 AND status='running')",
    ).bind(export_id).bind(worker_id).bind(generation).bind(attempt).fetch_one(&state.pool).await?;
    Ok(Json(running))
}

async fn current_operation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    worker_id: Uuid,
    server_id: Uuid,
    operation_id: Uuid,
    generation: i64,
    attempt: i32,
) -> Result<OperationRecord, ApiError> {
    let operation = sqlx::query_as::<_, OperationRecord>(
        "SELECT id, server_id, worker_id, kind, status, generation, attempt, payload,
                last_error, created_at, updated_at FROM operations
         WHERE id=$1 AND server_id=$2 AND worker_id=$3 AND generation=$4 AND attempt=$5 AND status='running'
         FOR SHARE",
    ).bind(operation_id).bind(server_id).bind(worker_id).bind(generation).bind(attempt)
        .fetch_optional(&mut **tx).await?.ok_or_else(invalid_snapshot_assignment)?;
    if operation.status != OperationStatus::Running {
        return Err(invalid_snapshot_assignment());
    }
    Ok(operation)
}

async fn snapshot_record(
    state: &AppState,
    server_id: Uuid,
    generation: i64,
) -> Result<SnapshotRecord, ApiError> {
    sqlx::query_as::<_, SnapshotRecord>(
        "SELECT id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete, created_at
         FROM snapshots WHERE server_id=$1 AND generation=$2 AND complete",
    ).bind(server_id).bind(generation).fetch_optional(&state.pool).await?
        .ok_or_else(invalid_snapshot_assignment)
}

fn snapshot_reason_for_operation(operation: &OperationRecord) -> Result<SnapshotReason, ApiError> {
    match operation.kind {
        OperationKind::Snapshot => serde_json::from_value(
            operation
                .payload
                .get("reason")
                .cloned()
                .ok_or_else(invalid_snapshot_assignment)?,
        )
        .map_err(|_| invalid_snapshot_assignment()),
        OperationKind::Stop | OperationKind::Restart => Ok(SnapshotReason::Shutdown),
        OperationKind::Restore => Ok(SnapshotReason::Restore),
        OperationKind::Reset => Ok(SnapshotReason::Reset),
        OperationKind::Start | OperationKind::Delete => Err(invalid_snapshot_assignment()),
    }
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn invalid_snapshot_assignment() -> ApiError {
    ApiError::with_code(
        StatusCode::CONFLICT,
        "worker_snapshot_invalid",
        "worker snapshot request is stale or outside this operation",
    )
}

fn snapshot_reason_name(reason: SnapshotReason) -> &'static str {
    match reason {
        SnapshotReason::Periodic => "periodic",
        SnapshotReason::Shutdown => "shutdown",
        SnapshotReason::Manual => "manual",
        SnapshotReason::Restore => "restore",
        SnapshotReason::Reset => "reset",
        SnapshotReason::Export => "export",
    }
}
