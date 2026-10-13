use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    contracts::plugins::{
        MarketInstallRequest, MarketPluginQuery, MarketPlugins, PluginUpdateRequest,
        PluginUploadQuery, ServerPlugin, ServerPlugins,
    },
    control_plane::plugins::{self, PluginError},
};

use super::actor::{
    ApiBrowserServerHeaders, ApiJson, ApiPath, ApiQuery, ApiServerHeaders, ApiWasm, ServerIdPath,
};
use super::internal::{require_browser_server_access, require_server_access};
use super::{ApiError, AppState};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct ServerPluginPath {
    server_id: Uuid,
    plugin_id: Uuid,
}

impl From<PluginError> for ApiError {
    fn from(error: PluginError) -> Self {
        let message = error.to_string();
        match error {
            PluginError::NotFound => {
                Self::with_code(StatusCode::NOT_FOUND, "plugin_not_found", message)
            }
            PluginError::NotInstallable => Self::with_code(
                StatusCode::NOT_FOUND,
                "market_plugin_not_installable",
                message,
            ),
            PluginError::MarketUnavailable(source) => {
                tracing::warn!(error = ?source, "plugin market request failed");
                Self::with_code(
                    StatusCode::BAD_GATEWAY,
                    "plugin_market_unavailable",
                    message,
                )
            }
            PluginError::InvalidBinary(_) => {
                Self::with_code(StatusCode::UNPROCESSABLE_ENTITY, "invalid_plugin", message)
            }
            PluginError::TooMany => Self::with_code(StatusCode::CONFLICT, "plugin_limit", message),
            PluginError::AlreadyInstalled => {
                Self::with_code(StatusCode::CONFLICT, "plugin_already_installed", message)
            }
            PluginError::Database(error) => error.into(),
            PluginError::Internal(error) => error.into(),
        }
    }
}

pub(super) async fn server_plugins(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
) -> Result<Json<ServerPlugins>, ApiError> {
    let server = require_server_access(&state, &headers, path.server_id).await?;
    Ok(Json(plugins::server_plugins(&state.pool, server.id).await?))
}

pub(super) async fn market_plugins(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
    ApiQuery(query): ApiQuery<MarketPluginQuery>,
) -> Result<Json<MarketPlugins>, ApiError> {
    let server = require_server_access(&state, &headers, path.server_id).await?;
    let query = query
        .query
        .map(|query| query.chars().take(64).collect::<String>());
    Ok(Json(MarketPlugins {
        plugins: plugins::market_plugins(
            &state.pool,
            &state.plugin_market,
            server.id,
            query.as_deref(),
        )
        .await?,
    }))
}

pub(super) async fn install_market_plugin(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
    ApiJson(input): ApiJson<MarketInstallRequest>,
) -> Result<Json<ServerPlugin>, ApiError> {
    let server = require_server_access(&state, &headers, path.server_id).await?;
    let (pool, store, market) = (
        state.pool.clone(),
        state.object_store.clone(),
        state.plugin_market.clone(),
    );
    // The download finishes even when the client stops waiting for it, as
    // the in-game plugin does after a few seconds.
    let installed = tokio::spawn(async move {
        plugins::install_market(&pool, &store, &market, server.id, input.market_plugin_id).await
    })
    .await
    .map_err(anyhow::Error::new)?;
    Ok(Json(installed?))
}

pub(super) async fn upload_plugin(
    State(state): State<Arc<AppState>>,
    headers: ApiBrowserServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
    ApiQuery(query): ApiQuery<PluginUploadQuery>,
    ApiWasm(bytes): ApiWasm,
) -> Result<(StatusCode, Json<ServerPlugin>), ApiError> {
    let server = require_browser_server_access(&state, &headers, path.server_id).await?;
    Ok((
        StatusCode::CREATED,
        Json(
            plugins::install_upload(
                &state.pool,
                &state.object_store,
                server.id,
                &query.file_name,
                bytes,
            )
            .await?,
        ),
    ))
}

pub(super) async fn update_plugin(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerPluginPath>,
    ApiJson(input): ApiJson<PluginUpdateRequest>,
) -> Result<Json<ServerPlugin>, ApiError> {
    let server = require_server_access(&state, &headers, path.server_id).await?;
    Ok(Json(
        plugins::set_enabled(&state.pool, server.id, path.plugin_id, input.enabled).await?,
    ))
}

pub(super) async fn remove_plugin(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerPluginPath>,
) -> Result<StatusCode, ApiError> {
    let server = require_server_access(&state, &headers, path.server_id).await?;
    plugins::remove(&state.pool, server.id, path.plugin_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
