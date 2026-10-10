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
    BuildKey, BuildRequest, CiHostname, CiPreview, CiPreviewRepository, CiPreviewRuntimeRepository,
    CiPreviewState, CiRefKind, RuntimeLease,
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

async fn ensure_commit_preview_on_demand(
    state: &AppState,
    repository: &CiPreviewRepository,
    hostname: &str,
    existing: Option<CiPreview>,
) -> Result<Option<CiPreview>, ApiError> {
    let Some(identity) = state.ci_build_identity.as_ref() else {
        return Ok(existing);
    };
    let Some(builder) = state.ci_build_coordinator.as_ref() else {
        return Ok(existing);
    };
    let root_domain = crate::ci::CI_ROOT_DOMAIN;
    let normalized_host = hostname.trim_end_matches('.').to_ascii_lowercase();
    let Some(label) = normalized_host
        .strip_suffix(&format!(".{root_domain}"))
        .and_then(|host| host.strip_prefix("commit-"))
    else {
        return Ok(existing);
    };
    let alias = match CiHostname::for_ref(CiRefKind::Commit, label, root_domain) {
        Ok(alias) if alias.value == normalized_host => alias,
        _ => return Ok(existing),
    };
    let key = BuildKey::new(
        &identity.repository,
        label,
        &identity.patch_fingerprint,
        &identity.toolchain_version,
        &identity.target_architecture,
    )
    .map_err(|error| {
        ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_ci_commit",
            error.to_string(),
        )
    })?;
    let (build, _) = repository
        .ensure_build(&key)
        .await
        .map_err(ApiError::from)?;
    // A commit alias may have been created under an older host patch fingerprint.
    // Rebind only non-running aliases to the current immutable build key so a
    // source-preparer/host rollout can recover queued previews without ever
    // moving an active runtime to a different artifact identity.
    let preview = match existing {
        Some(preview)
            if matches!(
                preview.state,
                CiPreviewState::Booting | CiPreviewState::Running | CiPreviewState::Stopping
            ) =>
        {
            preview
        }
        _ => repository
            .ensure_preview(
                &identity.repository,
                label,
                CiRefKind::Commit,
                None,
                &alias,
                build.id,
            )
            .await
            .map_err(ApiError::from)?,
    };
    let retry_due = if build.stage == crate::ci::CiBuildStage::Failed {
        repository
            .is_build_retry_due(build.id)
            .await
            .map_err(ApiError::from)?
    } else {
        false
    };
    let should_start = !matches!(
        build.stage,
        crate::ci::CiBuildStage::Ready | crate::ci::CiBuildStage::Failed
    ) || retry_due;
    if should_start {
        let builder = builder.clone();
        let request = BuildRequest {
            key,
            source_repository: Some(identity.repository.clone()),
            source_archive: Vec::new(),
        };
        let build_id = build.id;
        tokio::spawn(async move {
            if let Err(error) = builder.ensure_build(request).await {
                tracing::error!(build_id = %build_id, ?error, "on-demand immutable CI commit build failed");
            }
        });
    }
    Ok(Some(preview))
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
    let existing = repository
        .lookup_preview(&request.hostname)
        .await
        .map_err(ApiError::from)?;
    let is_commit_alias = existing
        .as_ref()
        .is_some_and(|preview| preview.alias.kind == CiRefKind::Commit);
    let preview = if existing.is_none() || is_commit_alias {
        ensure_commit_preview_on_demand(&state, &repository, &request.hostname, existing.clone())
            .await?
            .or(existing)
    } else {
        existing
    };
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
    if matches!(
        preview.state,
        CiPreviewState::Ready
            | CiPreviewState::Booting
            | CiPreviewState::Running
            | CiPreviewState::Stopping
    ) {
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
            RuntimeLease::Stopping(_) => {
                preview.state = CiPreviewState::Stopping;
            }
            RuntimeLease::NoWorker => {
                // A build can be ready without a live worker or free capacity.
                // Never project an unverified stale placement as running.
                if preview.state != CiPreviewState::Stopping {
                    preview.state = CiPreviewState::Ready;
                }
            }
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
