use std::{collections::HashSet, path::Component};

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use futures::StreamExt;
use object_store::ObjectStore;
use serde::Deserialize;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::{
    contracts::worker::WorkerExportTask,
    model::{OperationKind, OperationRecord, OperationStatus, SERVER_STORAGE_BYTES},
    snapshot::{ObjectKind, SnapshotManifest, WorkerObjectKey, physical_object_path},
};

use super::export_not_found;
use super::{
    ApiError, AppState, bounded_worker_error, invalid_export, invalid_snapshot, is_lower_hex,
    lock_worker_operation, server_not_found, stale_operation, worker_auth,
};

const MAX_ARCHIVE_BYTES: i64 = SERVER_STORAGE_BYTES;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::control_plane::api) struct PublishSnapshotRequest {
    operation_id: Uuid,
    generation: i64,
    attempt: i32,
    snapshot_generation: i64,
    size_bytes: i64,
}

pub(in crate::control_plane::api) async fn publish_snapshot(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path(server_id): Path<Uuid>,
    Json(request): Json<PublishSnapshotRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    if request.generation <= 0
        || request.attempt < 1
        || request.snapshot_generation <= 0
        || !(0..=SERVER_STORAGE_BYTES).contains(&request.size_bytes)
    {
        return Err(stale_operation());
    }
    let mut tx = state.pool.begin().await?;
    let server = sqlx::query(
        "SELECT generation, current_snapshot_generation
         FROM servers WHERE id = $1 FOR UPDATE",
    )
    .bind(server_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(server_not_found)?;
    let server_generation: i64 = server.try_get("generation")?;
    let current_generation: Option<i64> = server.try_get("current_snapshot_generation")?;
    if server_generation != request.generation {
        return Err(stale_operation());
    }
    let operation = lock_worker_operation(
        &mut tx,
        worker_id,
        request.operation_id,
        server_id,
        request.generation,
        request.attempt,
    )
    .await?;
    if operation.status != OperationStatus::Running {
        return Err(stale_operation());
    }

    let snapshot = sqlx::query(
        "SELECT manifest_key, size_bytes, checksum, reason::text AS reason,
                created_at, complete
         FROM snapshots WHERE server_id = $1 AND generation = $2 FOR UPDATE",
    )
    .bind(server_id)
    .bind(request.snapshot_generation)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(invalid_snapshot)?;
    let manifest_key: String = snapshot.try_get("manifest_key")?;
    let size_bytes: i64 = snapshot.try_get("size_bytes")?;
    let checksum: String = snapshot.try_get("checksum")?;
    let reason: String = snapshot.try_get("reason")?;
    let created_at: chrono::DateTime<chrono::Utc> = snapshot.try_get("created_at")?;
    let complete: bool = snapshot.try_get("complete")?;
    let expected_reason = snapshot_reason_for_operation(&operation)?;
    if !complete
        || size_bytes != request.size_bytes
        || created_at < operation.created_at
        || reason != expected_reason
    {
        return Err(invalid_snapshot());
    }
    verify_snapshot_manifest(
        state.object_store.as_ref(),
        server_id,
        request.snapshot_generation,
        &manifest_key,
        &checksum,
        size_bytes,
    )
    .await?;
    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }

    if current_generation == Some(request.snapshot_generation) {
        tx.commit().await?;
        return Ok(StatusCode::NO_CONTENT);
    }
    if current_generation.is_some_and(|generation| request.snapshot_generation <= generation) {
        return Err(stale_operation());
    }

    let updated = sqlx::query(
        "UPDATE servers SET current_snapshot_generation = $2, storage_bytes = $3,
             updated_at = now()
         WHERE id = $1 AND generation = $4
           AND (current_snapshot_generation IS NULL OR current_snapshot_generation < $2)",
    )
    .bind(server_id)
    .bind(request.snapshot_generation)
    .bind(size_bytes)
    .bind(request.generation)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(stale_operation());
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

fn snapshot_reason_for_operation(operation: &OperationRecord) -> Result<String, ApiError> {
    let reason = match operation.kind {
        OperationKind::Snapshot => operation
            .payload
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(invalid_snapshot)?,
        OperationKind::Stop | OperationKind::Restart => "shutdown".to_owned(),
        OperationKind::Restore => "restore".to_owned(),
        OperationKind::Reset => "reset".to_owned(),
        OperationKind::Start | OperationKind::Delete => return Err(invalid_snapshot()),
    };
    Ok(reason)
}

pub(in crate::control_plane::api) async fn verify_snapshot_manifest(
    store: &dyn ObjectStore,
    server_id: Uuid,
    generation: i64,
    manifest_key: &str,
    checksum: &str,
    size_bytes: i64,
) -> Result<(), ApiError> {
    let key = WorkerObjectKey::parse(manifest_key).map_err(|_| invalid_snapshot())?;
    if !matches!(key.kind(), ObjectKind::Manifest { generation: key_generation, checksum: ref key_checksum }
        if key_generation == generation && key_checksum == checksum)
    {
        return Err(invalid_snapshot());
    }
    let physical_key =
        physical_object_path(server_id, manifest_key).map_err(|_| invalid_snapshot())?;
    let metadata = store
        .head(&physical_key)
        .await
        .map_err(|_| invalid_snapshot())?;
    if metadata.size > key.max_bytes() {
        return Err(invalid_snapshot());
    }
    let bytes = store
        .get(&physical_key)
        .await
        .map_err(|_| invalid_snapshot())?
        .bytes()
        .await
        .map_err(|_| invalid_snapshot())?;
    if blake3::hash(&bytes).to_hex().as_str() != checksum {
        return Err(invalid_snapshot());
    }
    let manifest: SnapshotManifest =
        serde_json::from_slice(&bytes).map_err(|_| invalid_snapshot())?;
    if manifest.server_id != server_id || manifest.generation != generation {
        return Err(invalid_snapshot());
    }
    let mut total = 0_u64;
    let mut paths = HashSet::new();
    for file in manifest.files {
        if !safe_manifest_path(&file.path) || !paths.insert(file.path.clone()) {
            return Err(invalid_snapshot());
        }
        let chunk_key = WorkerObjectKey::parse(&format!("chunks/{}", file.blake3))
            .map_err(|_| invalid_snapshot())?;
        if !matches!(chunk_key.kind(), ObjectKind::Chunk) {
            return Err(invalid_snapshot());
        }
        total = total
            .checked_add(file.size)
            .filter(|size| *size <= SERVER_STORAGE_BYTES as u64)
            .ok_or_else(invalid_snapshot)?;
    }
    if total != size_bytes as u64 {
        return Err(invalid_snapshot());
    }
    Ok(())
}

fn safe_manifest_path(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|character| matches!(character, '\\' | '\0'))
        && std::path::Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::control_plane::api) struct ExportFenceRequest {
    source_generation: i64,
    attempt: i32,
    archive_key: Option<String>,
    archive_size: Option<i64>,
    archive_checksum: Option<String>,
    error: Option<String>,
}

struct LockedExport {
    status: String,
    stage: String,
    archive_key: Option<String>,
    archive_size: Option<i64>,
    archive_checksum: Option<String>,
}

pub(in crate::control_plane::api) async fn claim_export(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Option<WorkerExportTask>>, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("kurbis-worker-export-claim:{worker_id}"))
        .execute(&mut *tx)
        .await?;

    let running_server = lock_server_with_worker_export(&mut tx, worker_id, true).await?;
    let mut task = if let Some(server_id) = running_server {
        sqlx::query(
            "SELECT id, server_id, source_generation, attempt
             FROM world_exports
             WHERE server_id = $1 AND worker_id = $2 AND status = 'running'
             ORDER BY updated_at, id
             FOR UPDATE LIMIT 1",
        )
        .bind(server_id)
        .bind(worker_id)
        .fetch_optional(&mut *tx)
        .await?
        .map(|row| export_task_from_row(&row))
        .transpose()?
    } else {
        None
    };

    if task.is_none()
        && let Some(server_id) = lock_server_with_worker_export(&mut tx, worker_id, false).await?
    {
        task = sqlx::query(
            "SELECT id, server_id, source_generation, attempt
             FROM world_exports
             WHERE server_id = $1 AND worker_id = $2 AND status = 'pending'
               AND stage = 'ready' AND source_generation IS NOT NULL
             ORDER BY created_at, id
             FOR UPDATE LIMIT 1",
        )
        .bind(server_id)
        .bind(worker_id)
        .fetch_optional(&mut *tx)
        .await?
        .map(|row| export_task_from_row(&row))
        .transpose()?;
        if let Some(task) = task.as_mut() {
            let attempt = task.attempt.checked_add(1).ok_or_else(stale_operation)?;
            let updated = sqlx::query(
                "UPDATE world_exports SET status = 'running',
                     attempt = $2, stage = 'archiving', updated_at = now()
                 WHERE id = $1 AND worker_id = $3 AND status = 'pending'",
            )
            .bind(task.id)
            .bind(attempt)
            .bind(worker_id)
            .execute(&mut *tx)
            .await?;
            if updated.rows_affected() != 1 {
                return Err(stale_operation());
            }
            task.attempt = attempt;
        }
    }

    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    tx.commit().await?;
    Ok(Json(task))
}

async fn lock_server_with_worker_export(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    worker_id: Uuid,
    running: bool,
) -> Result<Option<Uuid>, ApiError> {
    let server = if running {
        sqlx::query_scalar::<_, Uuid>(
            "SELECT s.id
             FROM servers s
             WHERE EXISTS (
                 SELECT 1 FROM world_exports e
                 WHERE e.server_id = s.id AND e.worker_id = $1 AND e.status = 'running'
             )
             ORDER BY (
                 SELECT min(e.updated_at) FROM world_exports e
                 WHERE e.server_id = s.id AND e.worker_id = $1 AND e.status = 'running'
             ), s.id
             FOR UPDATE OF s SKIP LOCKED LIMIT 1",
        )
    } else {
        sqlx::query_scalar::<_, Uuid>(
            "SELECT s.id
             FROM servers s
             WHERE EXISTS (
                 SELECT 1 FROM world_exports e
                 WHERE e.server_id = s.id AND e.worker_id = $1
                   AND e.status = 'pending' AND e.stage = 'ready'
                   AND e.source_generation IS NOT NULL
             )
             ORDER BY (
                 SELECT min(e.created_at) FROM world_exports e
                 WHERE e.server_id = s.id AND e.worker_id = $1
                   AND e.status = 'pending' AND e.stage = 'ready'
                   AND e.source_generation IS NOT NULL
             ), s.id
             FOR UPDATE OF s SKIP LOCKED LIMIT 1",
        )
    };
    Ok(server.bind(worker_id).fetch_optional(&mut **tx).await?)
}

fn export_task_from_row(row: &sqlx::postgres::PgRow) -> Result<WorkerExportTask, ApiError> {
    let source_generation: Option<i64> = row.try_get("source_generation")?;
    let task = WorkerExportTask {
        id: row.try_get("id")?,
        server_id: row.try_get("server_id")?,
        source_generation: source_generation.ok_or_else(export_not_found)?,
        attempt: row.try_get("attempt")?,
    };
    if task.source_generation < 1 || task.attempt < 0 {
        return Err(export_not_found());
    }
    Ok(task)
}

pub(in crate::control_plane::api) async fn complete_export(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path(export_id): Path<Uuid>,
    Json(request): Json<ExportFenceRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let (archive_key, archive_size, archive_checksum) = match (
        request.archive_key.as_deref(),
        request.archive_size,
        request.archive_checksum.as_deref(),
        request.error.as_ref(),
    ) {
        (Some(key), Some(size), Some(checksum), None) => (key, size, checksum),
        _ => return Err(invalid_export()),
    };
    if request.attempt < 1
        || request.source_generation < 1
        || !(0..=MAX_ARCHIVE_BYTES).contains(&archive_size)
        || !is_lower_hex(archive_checksum, 64)
    {
        return Err(invalid_export());
    }
    let expected_key = format!("exports/{export_id}/{}.zip", request.attempt);
    if archive_key != expected_key {
        return Err(invalid_export());
    }
    let key = WorkerObjectKey::parse(archive_key).map_err(|_| invalid_export())?;
    if !matches!(key.kind(), ObjectKind::Export { export_id: key_id, attempt } if key_id == export_id && attempt == request.attempt)
    {
        return Err(invalid_export());
    }
    let server_id = assigned_export_server_id(
        &state,
        export_id,
        worker_id,
        request.source_generation,
        request.attempt,
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    lock_export_server(&mut tx, server_id).await?;
    let export = lock_export_for_completion(
        &mut tx,
        export_id,
        server_id,
        worker_id,
        request.source_generation,
        request.attempt,
    )
    .await?;
    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    if export.status == "completed" {
        ensure_completed_archive_matches(&export, archive_key, archive_size, archive_checksum)?;
        tx.commit().await?;
        return Ok(StatusCode::NO_CONTENT);
    }
    if export.status != "running" {
        return Err(export_not_found());
    }
    match export.stage.as_str() {
        "archiving" => {
            sqlx::query(
                "UPDATE world_exports SET stage = 'verifying', updated_at = now()
                 WHERE id = $1 AND status = 'running' AND worker_id = $2 AND attempt = $3
                   AND source_generation = $4 AND stage = 'archiving'",
            )
            .bind(export_id)
            .bind(worker_id)
            .bind(request.attempt)
            .bind(request.source_generation)
            .execute(&mut *tx)
            .await?;
        }
        "verifying" => {}
        _ => return Err(export_not_found()),
    }
    tx.commit().await?;

    verify_export_archive(
        state.object_store.as_ref(),
        server_id,
        archive_key,
        archive_size,
        archive_checksum,
    )
    .await?;

    let mut tx = state.pool.begin().await?;
    lock_export_server(&mut tx, server_id).await?;
    let export = lock_export_for_completion(
        &mut tx,
        export_id,
        server_id,
        worker_id,
        request.source_generation,
        request.attempt,
    )
    .await?;
    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    if export.status == "completed" {
        ensure_completed_archive_matches(&export, archive_key, archive_size, archive_checksum)?;
        tx.commit().await?;
        return Ok(StatusCode::NO_CONTENT);
    }
    if export.status != "running" || export.stage != "verifying" {
        return Err(export_not_found());
    }
    let updated = sqlx::query(
        "UPDATE world_exports SET status = 'completed', stage = 'completed',
             archive_key = $2, archive_size = $3, archive_checksum = $4,
             expires_at = now() + interval '24 hours', last_error = NULL, updated_at = now()
         WHERE id = $1 AND worker_id = $5 AND status = 'running' AND stage = 'verifying'
           AND attempt = $6 AND source_generation = $7",
    )
    .bind(export_id)
    .bind(archive_key)
    .bind(archive_size)
    .bind(archive_checksum)
    .bind(worker_id)
    .bind(request.attempt)
    .bind(request.source_generation)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(stale_operation());
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn assigned_export_server_id(
    state: &AppState,
    export_id: Uuid,
    worker_id: Uuid,
    source_generation: i64,
    attempt: i32,
) -> Result<Uuid, ApiError> {
    sqlx::query_scalar(
        "SELECT server_id FROM world_exports
         WHERE id = $1 AND worker_id = $2 AND source_generation = $3 AND attempt = $4",
    )
    .bind(export_id)
    .bind(worker_id)
    .bind(source_generation)
    .bind(attempt)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(export_not_found)
}

async fn lock_export_server(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    server_id: Uuid,
) -> Result<(), ApiError> {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM servers WHERE id = $1 FOR UPDATE")
        .bind(server_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(server_not_found)?;
    Ok(())
}

async fn lock_export_for_completion(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    export_id: Uuid,
    server_id: Uuid,
    worker_id: Uuid,
    source_generation: i64,
    attempt: i32,
) -> Result<LockedExport, ApiError> {
    let row = sqlx::query(
        "SELECT status, stage, archive_key, archive_size, archive_checksum
         FROM world_exports
         WHERE id = $1 AND server_id = $2 AND worker_id = $3
           AND source_generation = $4 AND attempt = $5
         FOR UPDATE",
    )
    .bind(export_id)
    .bind(server_id)
    .bind(worker_id)
    .bind(source_generation)
    .bind(attempt)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(export_not_found)?;
    Ok(LockedExport {
        status: row.try_get("status")?,
        stage: row.try_get("stage")?,
        archive_key: row.try_get("archive_key")?,
        archive_size: row.try_get("archive_size")?,
        archive_checksum: row.try_get("archive_checksum")?,
    })
}

fn ensure_completed_archive_matches(
    export: &LockedExport,
    archive_key: &str,
    archive_size: i64,
    archive_checksum: &str,
) -> Result<(), ApiError> {
    if export.archive_key.as_deref() == Some(archive_key)
        && export.archive_size == Some(archive_size)
        && export.archive_checksum.as_deref() == Some(archive_checksum)
    {
        Ok(())
    } else {
        Err(stale_operation())
    }
}

async fn verify_export_archive(
    store: &dyn ObjectStore,
    server_id: Uuid,
    archive_key: &str,
    archive_size: i64,
    archive_checksum: &str,
) -> Result<(), ApiError> {
    let object_path = physical_object_path(server_id, archive_key).map_err(|_| invalid_export())?;
    let archive = store
        .get(&object_path)
        .await
        .map_err(|_| invalid_export())?;
    if archive.meta.size != archive_size as u64 {
        return Err(invalid_export());
    }
    let mut stream = archive.into_stream();
    let mut hasher = blake3::Hasher::new();
    let mut bytes = 0_u64;
    while let Some(part) = stream.next().await {
        let part = part.map_err(|_| invalid_export())?;
        bytes = bytes
            .checked_add(part.len() as u64)
            .filter(|size| *size <= MAX_ARCHIVE_BYTES as u64)
            .ok_or_else(invalid_export)?;
        hasher.update(&part);
    }
    if bytes != archive_size as u64 || hasher.finalize().to_hex().as_str() != archive_checksum {
        return Err(invalid_export());
    }
    Ok(())
}

pub(in crate::control_plane::api) async fn fail_export(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path(export_id): Path<Uuid>,
    Json(request): Json<ExportFenceRequest>,
) -> Result<StatusCode, ApiError> {
    let error = request.error.as_deref().ok_or_else(invalid_export)?;
    if request.archive_key.is_some()
        || request.archive_size.is_some()
        || request.archive_checksum.is_some()
        || request.attempt < 1
        || request.source_generation < 1
    {
        return Err(invalid_export());
    }
    let error = bounded_worker_error(error)?;
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let server_id = assigned_export_server_id(
        &state,
        export_id,
        worker_id,
        request.source_generation,
        request.attempt,
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    lock_export_server(&mut tx, server_id).await?;
    let current = sqlx::query(
        "SELECT status, worker_id, attempt, source_generation, last_error
         FROM world_exports WHERE id = $1 AND server_id = $2 FOR UPDATE",
    )
    .bind(export_id)
    .bind(server_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(export_not_found)?;
    let status: String = current.try_get("status")?;
    let assigned_worker: Option<Uuid> = current.try_get("worker_id")?;
    let attempt: i32 = current.try_get("attempt")?;
    let source_generation: Option<i64> = current.try_get("source_generation")?;
    let last_error: Option<String> = current.try_get("last_error")?;
    if status == "failed"
        && assigned_worker == Some(worker_id)
        && attempt == request.attempt
        && source_generation == Some(request.source_generation)
        && last_error.as_deref() == Some(error)
    {
        if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
            return Err(worker_auth::invalid_worker_token());
        }
        tx.commit().await?;
        return Ok(StatusCode::NO_CONTENT);
    }
    if status != "running"
        || assigned_worker != Some(worker_id)
        || attempt != request.attempt
        || source_generation != Some(request.source_generation)
    {
        return Err(export_not_found());
    }
    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    let result = sqlx::query(
        "UPDATE world_exports SET status = 'failed', stage = 'failed',
             last_error = $5, updated_at = now()
         WHERE id = $1 AND worker_id = $2 AND status = 'running'
           AND attempt = $3 AND source_generation = $4",
    )
    .bind(export_id)
    .bind(worker_id)
    .bind(request.attempt)
    .bind(request.source_generation)
    .bind(error)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() != 1 {
        return Err(export_not_found());
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
