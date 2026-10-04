use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ci::{
    CiPreview, CiPreviewRepository, CiPreviewRuntimeRepository, CiPreviewState, RuntimeLease,
};

use super::{ApiError, AppState, internal::require_internal};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewAdmissionRequest {
    pub hostname: String,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct PreviewAdmissionResponse {
    pub preview: Option<CiPreview>,
    pub state: CiPreviewState,
}

/// Public, read-only preview status projection used by GitHub Check Run links.
/// It intentionally omits build logs, failure details, capabilities, and runtime
/// assignment data; those remain on actor-authenticated internal routes.
pub(super) async fn public_status(
    State(state): State<Arc<AppState>>,
    Path(hostname): Path<String>,
) -> Result<Response, ApiError> {
    if crate::routing::ci_subdomain_from_hostname(&hostname, crate::ci::CI_ROOT_DOMAIN).is_none() {
        return Err(ApiError::with_code(
            StatusCode::NOT_FOUND,
            "ci_preview_not_found",
            "preview status is unavailable",
        ));
    }
    let repository = CiPreviewRepository::new(state.pool.clone());
    let preview = repository
        .lookup_preview(&hostname)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::with_code(
                StatusCode::NOT_FOUND,
                "ci_preview_not_found",
                "preview status is unavailable",
            )
        })?;
    let mut response = Json(crate::ci::CiPublicPreviewStatus {
        hostname: preview.alias.value,
        commit_sha: preview.commit_sha,
        kind: preview.alias.kind,
        state: preview.state,
        expires_at: preview.expires_at,
    })
    .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

pub(super) async fn status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(hostname): Path<String>,
) -> Result<Json<PreviewAdmissionResponse>, ApiError> {
    require_internal(&state, &headers)?;
    let repository = CiPreviewRepository::new(state.pool.clone());
    let preview = repository
        .lookup_preview(&hostname)
        .await
        .map_err(ApiError::from)?;
    let state = preview
        .as_ref()
        .map(|preview| preview.state)
        .unwrap_or(CiPreviewState::Expired);
    Ok(Json(PreviewAdmissionResponse { preview, state }))
}

pub(super) async fn admit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<PreviewAdmissionRequest>,
) -> Result<(StatusCode, Json<PreviewAdmissionResponse>), ApiError> {
    require_internal(&state, &headers)?;
    if request.hostname.len() > 253
        || request
            .hostname
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte == 0)
    {
        return Err(ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_ci_hostname",
            "invalid CI preview hostname",
        ));
    }
    let repository = CiPreviewRepository::new(state.pool.clone());
    let preview = repository
        .lookup_preview(&request.hostname)
        .await
        .map_err(ApiError::from)?;
    let Some(preview) = preview else {
        return Err(ApiError::with_code(
            StatusCode::NOT_FOUND,
            "ci_preview_not_found",
            "CI preview does not exist or has expired",
        ));
    };
    repository
        .touch_preview(preview.id)
        .await
        .map_err(ApiError::from)?;
    let mut preview = if preview.state == CiPreviewState::Lobby {
        let (preview, request, build_id) = repository
            .prepare_lazy_branch_admission(preview.id, crate::ci::CI_ROOT_DOMAIN)
            .await
            .map_err(ApiError::from)?;
        if let (Some(builder), Some(request), Some(build_id)) =
            (state.ci_build_coordinator.clone(), request, build_id)
        {
            tokio::spawn(async move {
                if let Err(error) = builder.ensure_build(request).await {
                    tracing::error!(build_id = %build_id, ?error, "lazy CI branch build failed");
                }
            });
        }
        preview
    } else {
        preview
    };
    if preview.state == CiPreviewState::Ready {
        let runtimes = CiPreviewRuntimeRepository::new(state.pool.clone());
        let capability_hash = blake3::hash(&rand::random::<[u8; 32]>());
        match runtimes
            .claim_start(preview.id, capability_hash.as_bytes())
            .await
            .map_err(ApiError::from)?
        {
            RuntimeLease::Claimed(_) | RuntimeLease::Starting(_) => {
                preview.state = CiPreviewState::Booting;
            }
            RuntimeLease::AlreadyRunning(_) => {
                preview.state = CiPreviewState::Running;
            }
            RuntimeLease::NoWorker => {}
        }
    }
    let status = match preview.state {
        CiPreviewState::Ready | CiPreviewState::Booting => StatusCode::ACCEPTED,
        CiPreviewState::Running => StatusCode::OK,
        CiPreviewState::Lobby
        | CiPreviewState::Queued
        | CiPreviewState::Fetching
        | CiPreviewState::Patching
        | CiPreviewState::Compiling
        | CiPreviewState::Packaging => StatusCode::ACCEPTED,
        CiPreviewState::Stopping | CiPreviewState::Failed | CiPreviewState::Expired => {
            StatusCode::SERVICE_UNAVAILABLE
        }
    };
    Ok((
        status,
        Json(PreviewAdmissionResponse {
            state: preview.state,
            preview: Some(preview),
        }),
    ))
}
