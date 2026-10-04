//! Worker-authenticated control surface for dedicated CI preview runtimes.
//! The worker can see only previews currently assigned to its identity/lease.

use std::sync::Arc;

use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::Response,
};
use bytes::BytesMut;
use futures::StreamExt;
use object_store::{MultipartUpload, ObjectStore, path::Path as ObjectPath};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ci::{CI_RUNTIME_DISK_BYTES, CiPreviewRuntimeRepository, CiRuntimePlacement};

use super::{ApiError, AppState, worker_auth};

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkerCiRuntimes {
    runtimes: Vec<CiRuntimePlacement>,
}

pub(super) async fn assignments(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<WorkerCiRuntimes>, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let repository = CiPreviewRuntimeRepository::new(state.pool.clone());
    let runtimes = repository
        .worker_assignments(worker_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(WorkerCiRuntimes { runtimes }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeFence {
    generation: i64,
    lease_token: Uuid,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeActivityRequest {
    generation: i64,
    lease_token: Uuid,
    players: Vec<serde_json::Value>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeCapabilityRequest {
    generation: i64,
    lease_token: Uuid,
    capability: String,
}

pub(super) async fn capability(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(preview_id): Path<Uuid>,
    Json(request): Json<RuntimeCapabilityRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    if request.generation <= 0
        || request.capability.len() != 43
        || !request
            .capability
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(invalid_ci_assignment());
    }
    let repository = CiPreviewRuntimeRepository::new(state.pool.clone());
    let updated = repository
        .replace_capability_hash(
            preview_id,
            worker_id,
            request.generation,
            request.lease_token,
            blake3::hash(request.capability.as_bytes()).as_bytes(),
        )
        .await
        .map_err(ApiError::from)?;
    if !updated {
        return Err(invalid_ci_assignment());
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn started(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(preview_id): Path<Uuid>,
    Json(fence): Json<RuntimeFence>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let repository = CiPreviewRuntimeRepository::new(state.pool.clone());
    if !repository
        .mark_running(preview_id, worker_id, fence.generation, fence.lease_token)
        .await
        .map_err(ApiError::from)?
    {
        return Err(invalid_ci_assignment());
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn start_failed(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(preview_id): Path<Uuid>,
    Json(fence): Json<RuntimeFence>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let repository = CiPreviewRuntimeRepository::new(state.pool.clone());
    if !repository
        .mark_start_failed(preview_id, worker_id, fence.generation, fence.lease_token)
        .await
        .map_err(ApiError::from)?
    {
        return Err(invalid_ci_assignment());
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn stopped(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(preview_id): Path<Uuid>,
    Json(fence): Json<RuntimeFence>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let repository = CiPreviewRuntimeRepository::new(state.pool.clone());
    let placement = repository
        .current(preview_id)
        .await
        .map_err(ApiError::from)?
        .filter(|placement| {
            placement.worker_id == worker_id
                && placement.generation == fence.generation
                && placement.lease_token == fence.lease_token
                && placement.state == crate::ci::CiRuntimeState::Stopping
        })
        .ok_or_else(invalid_ci_assignment)?;
    if !repository
        .complete_idle_stop(&placement, true)
        .await
        .map_err(ApiError::from)?
    {
        return Err(invalid_ci_assignment());
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn deleted(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(preview_id): Path<Uuid>,
    Json(fence): Json<RuntimeFence>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let repository = CiPreviewRuntimeRepository::new(state.pool.clone());
    let placement = repository
        .current(preview_id)
        .await
        .map_err(ApiError::from)?
        .filter(|placement| {
            placement.worker_id == worker_id
                && placement.generation == fence.generation
                && placement.lease_token == fence.lease_token
                && placement.state == crate::ci::CiRuntimeState::Deleting
        })
        .ok_or_else(invalid_ci_assignment)?;
    if let Some(snapshot) = repository
        .world_snapshot_for_deletion(preview_id, worker_id, fence.generation, fence.lease_token)
        .await
        .map_err(ApiError::from)?
    {
        let path = ObjectPath::parse(&snapshot.object_key).map_err(|_| invalid_ci_assignment())?;
        state
            .object_store
            .delete(&path)
            .await
            .map_err(|_| invalid_ci_assignment())?;
        if !repository
            .complete_world_snapshot_deletion(
                preview_id,
                worker_id,
                fence.generation,
                fence.lease_token,
                &snapshot.object_key,
            )
            .await
            .map_err(ApiError::from)?
        {
            return Err(invalid_ci_assignment());
        }
    }
    if !repository
        .complete_expiry(&placement)
        .await
        .map_err(ApiError::from)?
    {
        return Err(invalid_ci_assignment());
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn runtime_lost(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(preview_id): Path<Uuid>,
    Json(fence): Json<RuntimeFence>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let repository = CiPreviewRuntimeRepository::new(state.pool.clone());
    if !repository
        .mark_runtime_lost(preview_id, worker_id, fence.generation, fence.lease_token)
        .await
        .map_err(ApiError::from)?
    {
        return Err(invalid_ci_assignment());
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn activity(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(preview_id): Path<Uuid>,
    Json(fence): Json<RuntimeActivityRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let repository = CiPreviewRuntimeRepository::new(state.pool.clone());
    let placement = repository
        .current(preview_id)
        .await
        .map_err(ApiError::from)?
        .filter(|placement| {
            placement.worker_id == worker_id
                && placement.generation == fence.generation
                && placement.lease_token == fence.lease_token
                && placement.state == crate::ci::CiRuntimeState::Running
        })
        .ok_or_else(invalid_ci_assignment)?;
    if !repository
        .report_players(
            placement.preview_id,
            worker_id,
            fence.generation,
            fence.lease_token,
            &serde_json::Value::Array(fence.players),
        )
        .await
        .map_err(ApiError::from)?
    {
        return Err(invalid_ci_assignment());
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn artifact(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((preview_id, generation, lease_token)): Path<(Uuid, i64, Uuid)>,
) -> Result<Response, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let repository = CiPreviewRuntimeRepository::new(state.pool.clone());
    let Some((object_key, size_bytes, digest)) = repository
        .artifact_object_key(preview_id, worker_id, generation, lease_token)
        .await
        .map_err(ApiError::from)?
    else {
        return Err(invalid_ci_assignment());
    };
    const MAX_ARTIFACT_BYTES: i64 = 1024 * 1024 * 1024;
    if !(0..=MAX_ARTIFACT_BYTES).contains(&size_bytes) {
        return Err(invalid_ci_assignment());
    }
    let path = object_store::path::Path::parse(object_key).map_err(|_| invalid_ci_assignment())?;
    let object = state
        .object_store
        .get(&path)
        .await
        .map_err(|_| invalid_ci_assignment())?;
    let bytes = object.bytes().await.map_err(|_| invalid_ci_assignment())?;
    if bytes.len() as i64 != size_bytes || blake3::hash(&bytes).to_hex().as_str() != digest {
        return Err(invalid_ci_assignment());
    }
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/gzip")
        .header(header::CONTENT_LENGTH, bytes.len())
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(Body::from(bytes))
        .map_err(|_| invalid_ci_assignment())?)
}

pub(super) async fn world_snapshot_requested(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((preview_id, generation, lease_token)): Path<(Uuid, i64, Uuid)>,
) -> Result<Json<WorldSnapshotRequestState>, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let repository = CiPreviewRuntimeRepository::new(state.pool.clone());
    require_ci_placement(
        &repository,
        preview_id,
        worker_id,
        generation,
        lease_token,
        crate::ci::CiRuntimeState::Running,
    )
    .await?;
    let requested = repository
        .world_snapshot_requested(preview_id, worker_id, generation, lease_token)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(WorldSnapshotRequestState { requested }))
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorldSnapshotRequestState {
    requested: bool,
}

pub(super) async fn world_snapshot(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((preview_id, generation, lease_token)): Path<(Uuid, i64, Uuid)>,
    request: axum::http::Request<Body>,
) -> Result<Response, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let repository = CiPreviewRuntimeRepository::new(state.pool.clone());
    match *request.method() {
        axum::http::Method::GET => {
            require_ci_placement(
                &repository,
                preview_id,
                worker_id,
                generation,
                lease_token,
                crate::ci::CiRuntimeState::Starting,
            )
            .await?;
            let snapshot = repository
                .world_snapshot_metadata(preview_id, worker_id, generation, lease_token)
                .await
                .map_err(ApiError::from)?;
            let Some(snapshot) = snapshot else {
                return Ok(Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(Body::empty())
                    .expect("static snapshot response is valid"));
            };
            let path =
                ObjectPath::parse(&snapshot.object_key).map_err(|_| invalid_ci_assignment())?;
            let object = state
                .object_store
                .get(&path)
                .await
                .map_err(|_| invalid_ci_assignment())?;
            if object.meta.size as i64 != snapshot.size_bytes {
                return Err(invalid_ci_assignment());
            }
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/gzip")
                .header(header::CONTENT_LENGTH, snapshot.size_bytes)
                .header("x-ci-world-snapshot-checksum", &snapshot.checksum)
                .header(header::CACHE_CONTROL, "private, no-store")
                .header("x-content-type-options", "nosniff")
                .body(Body::from_stream(object.into_stream()))
                .map_err(|_| invalid_ci_assignment())
        }
        axum::http::Method::PUT => {
            let placement = repository
                .current(preview_id)
                .await
                .map_err(ApiError::from)?
                .filter(|placement| {
                    placement.worker_id == worker_id
                        && placement.generation == generation
                        && placement.lease_token == lease_token
                        && ((placement.state == crate::ci::CiRuntimeState::Stopping
                            && placement.active)
                            || (placement.state == crate::ci::CiRuntimeState::Running
                                && placement.active))
                })
                .ok_or_else(invalid_ci_assignment)?;
            if placement.state == crate::ci::CiRuntimeState::Running
                && !repository
                    .world_snapshot_requested(preview_id, worker_id, generation, lease_token)
                    .await
                    .map_err(ApiError::from)?
            {
                return Err(invalid_ci_assignment());
            }
            let declared_size = request
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<i64>().ok())
                .ok_or_else(invalid_ci_assignment)?;
            if !(1..=CI_RUNTIME_DISK_BYTES).contains(&declared_size) {
                return Err(invalid_ci_assignment());
            }
            let key = format!(
                "ci-previews/{preview_id}/worlds/{generation}-{}.tar.gz",
                Uuid::new_v4().simple()
            );
            let path = ObjectPath::parse(&key).map_err(|_| invalid_ci_assignment())?;
            let mut upload = state
                .object_store
                .put_multipart(&path)
                .await
                .map_err(|_| invalid_ci_assignment())?;
            let mut body = request.into_body().into_data_stream();
            let mut hasher = blake3::Hasher::new();
            let mut total = 0_i64;
            let mut buffered = BytesMut::with_capacity(8 * 1024 * 1024);
            while let Some(chunk) = body.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(_) => {
                        let _ = upload.abort().await;
                        return Err(invalid_ci_assignment());
                    }
                };
                total = total
                    .checked_add(i64::try_from(chunk.len()).map_err(|_| invalid_ci_assignment())?)
                    .ok_or_else(invalid_ci_assignment)?;
                if total > declared_size || total > CI_RUNTIME_DISK_BYTES {
                    let _ = upload.abort().await;
                    return Err(invalid_ci_assignment());
                }
                hasher.update(&chunk);
                buffered.extend_from_slice(&chunk);
                while buffered.len() >= 8 * 1024 * 1024 {
                    let part = buffered.split_to(8 * 1024 * 1024).freeze();
                    if upload.put_part(part.into()).await.is_err() {
                        let _ = upload.abort().await;
                        return Err(invalid_ci_assignment());
                    }
                }
            }
            if total != declared_size {
                let _ = upload.abort().await;
                return Err(invalid_ci_assignment());
            }
            if !buffered.is_empty() && upload.put_part(buffered.freeze().into()).await.is_err() {
                let _ = upload.abort().await;
                return Err(invalid_ci_assignment());
            }
            if upload.complete().await.is_err() {
                return Err(invalid_ci_assignment());
            }
            let checksum = hasher.finalize().to_hex().to_string();
            let old_key = match repository
                .publish_world_snapshot(
                    preview_id,
                    worker_id,
                    generation,
                    lease_token,
                    &key,
                    declared_size,
                    &checksum,
                )
                .await
            {
                Ok(old_key) => old_key,
                Err(error) => {
                    let _ = state.object_store.delete(&path).await;
                    return Err(ApiError::from(error));
                }
            };
            if let Some(old_key) = old_key
                && let Ok(old_path) = ObjectPath::parse(&old_key)
            {
                let _ = state.object_store.delete(&old_path).await;
            }
            Ok(Response::builder()
                .status(StatusCode::NO_CONTENT)
                .body(Body::empty())
                .expect("static snapshot upload response is valid"))
        }
        _ => Err(ApiError::with_code(
            StatusCode::METHOD_NOT_ALLOWED,
            "ci_snapshot_method_not_allowed",
            "CI snapshot endpoint only supports GET and PUT",
        )),
    }
}

pub(super) async fn world_snapshot_complete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(preview_id): Path<Uuid>,
    Json(fence): Json<RuntimeFence>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let repository = CiPreviewRuntimeRepository::new(state.pool.clone());
    require_ci_placement(
        &repository,
        preview_id,
        worker_id,
        fence.generation,
        fence.lease_token,
        crate::ci::CiRuntimeState::Running,
    )
    .await?;
    if !repository
        .complete_world_snapshot_request(preview_id, worker_id, fence.generation, fence.lease_token)
        .await
        .map_err(ApiError::from)?
    {
        return Err(invalid_ci_assignment());
    }
    Ok(StatusCode::NO_CONTENT)
}

fn invalid_ci_assignment() -> ApiError {
    ApiError::with_code(
        StatusCode::CONFLICT,
        "ci_runtime_assignment_invalid",
        "CI runtime assignment is stale or outside this worker's authority",
    )
}

async fn require_ci_placement(
    repository: &CiPreviewRuntimeRepository,
    preview_id: Uuid,
    worker_id: Uuid,
    generation: i64,
    lease_token: Uuid,
    allowed_state: crate::ci::CiRuntimeState,
) -> Result<CiRuntimePlacement, ApiError> {
    repository
        .current(preview_id)
        .await
        .map_err(ApiError::from)?
        .filter(|placement| {
            placement.worker_id == worker_id
                && placement.generation == generation
                && placement.lease_token == lease_token
                && placement.state == allowed_state
                && (allowed_state != crate::ci::CiRuntimeState::Running || placement.active)
                && (allowed_state != crate::ci::CiRuntimeState::Starting || !placement.active)
        })
        .ok_or_else(invalid_ci_assignment)
}
