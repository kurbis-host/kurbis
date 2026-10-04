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
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ci::{CiPreviewRuntimeRepository, CiRuntimePlacement};

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

fn invalid_ci_assignment() -> ApiError {
    ApiError::with_code(
        StatusCode::CONFLICT,
        "ci_runtime_assignment_invalid",
        "CI runtime assignment is stale or outside this worker's authority",
    )
}
