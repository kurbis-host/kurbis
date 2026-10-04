//! Server-capability API for a CI preview's own Kurbis plugin. These handlers
//! resolve CI runtime records only; no account server table or account API is used.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    ci::{CiPreviewRuntimeRepository, CiRuntimeState},
    contracts::lifecycle::ServerCommandRequest,
};

use super::{ApiError, AppState, types::ServerSettings};

fn bearer(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|token| {
            token.len() == 43
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
        .ok_or_else(invalid_ci_capability)
}

fn invalid_ci_capability() -> ApiError {
    ApiError::with_code(
        StatusCode::UNAUTHORIZED,
        "invalid_ci_preview_capability",
        "CI preview capability is invalid or expired",
    )
}

async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    preview_id: Uuid,
) -> Result<(CiPreviewRuntimeRepository, CiRuntimePlacementView, Value), ApiError> {
    let token = bearer(headers)?;
    let runtimes = CiPreviewRuntimeRepository::new(state.pool.clone());
    let preview = crate::ci::CiPreviewRepository::new(state.pool.clone())
        .lookup_preview_by_id(preview_id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(invalid_ci_capability)?;
    if !runtimes
        .capability_valid(preview_id, preview.runtime_generation, token)
        .await
        .map_err(ApiError::from)?
    {
        return Err(invalid_ci_capability());
    }
    let placement = runtimes
        .current(preview_id)
        .await
        .map_err(ApiError::from)?
        .filter(|placement| {
            placement.active
                && placement.state == CiRuntimeState::Running
                && placement.generation == preview.runtime_generation
        })
        .map(CiRuntimePlacementView::from)
        .ok_or_else(invalid_ci_capability)?;
    runtimes
        .touch_player_activity(preview_id)
        .await
        .map_err(ApiError::from)?;
    let settings = runtimes
        .preview_settings(preview_id)
        .await
        .map_err(ApiError::from)?
        .unwrap_or_else(|| json!({}));
    Ok((runtimes, placement, settings))
}

#[derive(Debug)]
struct CiRuntimePlacementView {
    generation: i64,
}
impl From<crate::ci::CiRuntimePlacement> for CiRuntimePlacementView {
    fn from(placement: crate::ci::CiRuntimePlacement) -> Self {
        Self {
            generation: placement.generation,
        }
    }
}

pub(super) async fn status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(preview_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let (runtimes, placement, settings) = authorize(&state, &headers, preview_id).await?;
    let preview = crate::ci::CiPreviewRepository::new(state.pool.clone())
        .lookup_preview_by_id(preview_id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(invalid_ci_capability)?;
    let players = runtimes
        .preview_reported_players(preview_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(json!({
        "id": preview.id,
        "subdomain": preview.alias.value,
        "desired_state": "running",
        "observed_state": "online",
        "settings": settings,
        "player_count": players.len(),
        "runtime_generation": placement.generation,
        "expires_at": preview.expires_at,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdatePreviewSettings {
    settings: Value,
}

pub(super) async fn update_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(preview_id): Path<Uuid>,
    Json(request): Json<UpdatePreviewSettings>,
) -> Result<Json<Value>, ApiError> {
    let (runtimes, _placement, current) = authorize(&state, &headers, preview_id).await?;
    let patch = request.settings.as_object().ok_or_else(|| {
        ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_settings",
            "settings must be an object",
        )
    })?;
    let mut merged = current.as_object().cloned().unwrap_or_default();
    merged.extend(patch.clone());
    let settings = ServerSettings::from_stored(Value::Object(merged))?;
    let stored = serde_json::to_value(settings).map_err(|_| {
        ApiError::with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_settings",
            "could not serialize CI preview settings",
        )
    })?;
    if !runtimes
        .update_preview_settings(preview_id, &stored)
        .await
        .map_err(ApiError::from)?
    {
        return Err(invalid_ci_capability());
    }
    Ok(Json(json!({ "settings": stored })))
}

pub(super) async fn players(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(preview_id): Path<Uuid>,
) -> Result<Json<Vec<Value>>, ApiError> {
    let (runtimes, _placement, _settings) = authorize(&state, &headers, preview_id).await?;
    let reported = runtimes
        .preview_reported_players(preview_id)
        .await
        .map_err(ApiError::from)?;
    let now = Utc::now();
    let players = reported.into_iter().filter_map(|player| {
        let id = player.get("id")?.clone();
        let name = player.get("name")?.clone();
        let edition = player.get("edition")?.clone();
        Some(json!({"player_id":id,"name":name,"edition":edition,"connected_at":now,"last_seen_at":now}))
    }).collect();
    Ok(Json(players))
}

pub(super) async fn command(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(preview_id): Path<Uuid>,
    Json(command): Json<ServerCommandRequest>,
) -> Result<Json<Value>, ApiError> {
    let (runtimes, placement, _settings) = authorize(&state, &headers, preview_id).await?;
    let observed_state = match command {
        ServerCommandRequest::Stop { .. } | ServerCommandRequest::Restart => {
            if !runtimes
                .request_stop(preview_id)
                .await
                .map_err(ApiError::from)?
            {
                return Err(ApiError::with_code(
                    StatusCode::CONFLICT,
                    "ci_preview_not_running",
                    "CI preview is not currently running",
                ));
            }
            "stopping"
        }
        ServerCommandRequest::Start { .. } => "online",
        ServerCommandRequest::Snapshot { .. } => {
            if !runtimes
                .request_world_snapshot(preview_id)
                .await
                .map_err(ApiError::from)?
            {
                return Err(ApiError::with_code(
                    StatusCode::CONFLICT,
                    "ci_preview_not_running",
                    "CI preview is not currently running",
                ));
            }
            "running"
        }
        ServerCommandRequest::Restore { .. }
        | ServerCommandRequest::Reset { .. }
        | ServerCommandRequest::Delete => {
            return Err(ApiError::with_code(
                StatusCode::FORBIDDEN,
                "ci_preview_command_not_allowed",
                "this lifecycle operation is not available to a CI preview",
            ));
        }
    };
    let _ = placement;
    Ok(Json(json!({ "observed_state": observed_state })))
}
