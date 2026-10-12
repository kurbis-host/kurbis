//! AI gateway routes: the panel's overview and settings, and the job and sync
//! protocol of the in-game plugin.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    contracts::ai::{
        AiJob, AiJobDetail, AiJobRequest, AiOverview, AiSettings, AiSyncRequest, AiSyncResponse,
    },
    control_plane::ai::{self, AiError, AiGateway},
};

use super::{
    ApiError, AppState,
    actor::{
        ApiBrowserServerHeaders, ApiJson, ApiPath, ApiServerCapabilityHeaders, ApiServerHeaders,
        ServerIdPath,
    },
    internal::{require_browser_server_access, require_server_access, require_server_capability},
    operators::operators_state,
};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct ServerAiJobPath {
    server_id: Uuid,
    job_id: Uuid,
}

impl From<AiError> for ApiError {
    fn from(error: AiError) -> Self {
        let message = error.to_string();
        let (status, code) = match error {
            AiError::Database(error) => return error.into(),
            AiError::Disabled => (StatusCode::CONFLICT, "ai_disabled"),
            AiError::UnknownModel => (StatusCode::UNPROCESSABLE_ENTITY, "ai_unknown_model"),
            AiError::BudgetAboveAccount => {
                (StatusCode::UNPROCESSABLE_ENTITY, "ai_budget_above_account")
            }
            AiError::EmptyPrompt => (StatusCode::UNPROCESSABLE_ENTITY, "ai_empty_prompt"),
            AiError::JobIdReused => (StatusCode::CONFLICT, "ai_job_id_reused"),
            AiError::JobNotFound => (StatusCode::NOT_FOUND, "ai_job_not_found"),
            AiError::ServerBudgetExhausted => {
                (StatusCode::TOO_MANY_REQUESTS, "ai_server_budget_exhausted")
            }
            AiError::AccountBudgetExhausted => {
                (StatusCode::TOO_MANY_REQUESTS, "ai_account_budget_exhausted")
            }
            AiError::JobActive => (StatusCode::CONFLICT, "ai_job_active"),
        };
        Self::with_code(status, code, message)
    }
}

fn gateway(state: &AppState) -> Result<&AiGateway, ApiError> {
    state.ai.as_ref().ok_or_else(|| {
        ApiError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "ai_unavailable",
            "The AI assistant is not set up on this Kurbis installation.",
        )
    })
}

pub(super) async fn ai_overview(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
) -> Result<Json<AiOverview>, ApiError> {
    let server = require_server_access(&state, &headers, path.server_id).await?;
    Ok(Json(
        ai::overview(&state.pool, state.ai.as_ref(), server.id, server.owner_id).await?,
    ))
}

pub(super) async fn update_ai_settings(
    State(state): State<Arc<AppState>>,
    headers: ApiBrowserServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
    ApiJson(settings): ApiJson<AiSettings>,
) -> Result<Json<AiOverview>, ApiError> {
    let server = require_browser_server_access(&state, &headers, path.server_id).await?;
    let gateway = gateway(&state)?;
    gateway.update_settings(server.id, &settings).await?;
    Ok(Json(
        ai::overview(&state.pool, Some(gateway), server.id, server.owner_id).await?,
    ))
}

pub(super) async fn create_ai_job(
    State(state): State<Arc<AppState>>,
    headers: ApiServerCapabilityHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
    ApiJson(request): ApiJson<AiJobRequest>,
) -> Result<(StatusCode, Json<AiJob>), ApiError> {
    let server = require_server_capability(&state, &headers, path.server_id).await?;
    let gateway = gateway(&state)?;
    // The plugin vouches for who is asking; the operator roster decides
    // whether that player may act on the server.
    let operators = operators_state(&state.pool, server.id).await?;
    if !operators
        .entries
        .iter()
        .any(|entry| entry.player_id == request.operator_id)
    {
        return Err(ApiError::with_code(
            StatusCode::FORBIDDEN,
            "ai_operator_required",
            "Only operators of this server can use the AI assistant.",
        ));
    }
    let (job, created) = gateway
        .create_job(server.id, server.owner_id, &request)
        .await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(job)))
}

pub(super) async fn ai_job(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerAiJobPath>,
) -> Result<Json<AiJobDetail>, ApiError> {
    let server = require_server_access(&state, &headers, path.server_id).await?;
    ai::job_detail(&state.pool, server.id, path.job_id)
        .await?
        .map(Json)
        .ok_or_else(|| AiError::JobNotFound.into())
}

pub(super) async fn cancel_ai_job(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerAiJobPath>,
) -> Result<Json<AiJob>, ApiError> {
    let server = require_server_access(&state, &headers, path.server_id).await?;
    Ok(Json(
        ai::cancel_job(&state.pool, server.id, path.job_id).await?,
    ))
}

pub(super) async fn sync_ai(
    State(state): State<Arc<AppState>>,
    headers: ApiServerCapabilityHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
    ApiJson(request): ApiJson<AiSyncRequest>,
) -> Result<Json<AiSyncResponse>, ApiError> {
    let server = require_server_capability(&state, &headers, path.server_id).await?;
    let (response, advanced) = ai::sync(&state.pool, server.id, request).await?;
    if advanced && let Some(gateway) = &state.ai {
        gateway.wake();
    }
    Ok(Json(response))
}
