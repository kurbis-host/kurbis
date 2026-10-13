//! Worker routes that install a server's plugins into its runtime.

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
};
use uuid::Uuid;

use crate::{
    contracts::plugins::{MAX_PLUGIN_BYTES, PLUGIN_SHA256_HEADER, WorkerPluginsApplied},
    control_plane::plugins,
};

use super::{
    ApiError, AppState, worker_auth,
    worker_state::{ContextQuery, lock_current_placement, validate_assignment},
};

/// The binary of one of the server's enabled plugins, for a worker that holds
/// an assignment for the server.
pub(super) async fn plugin_binary(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path((server_id, plugin_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<ContextQuery>,
) -> Result<Response, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    validate_assignment(&state.pool, worker_id, server_id, query.assignment()?).await?;
    let sha256 = plugins::enabled_plugin_blob(&state.pool, server_id, plugin_id)
        .await?
        .ok_or_else(|| {
            ApiError::with_code(
                StatusCode::NOT_FOUND,
                "plugin_not_found",
                "plugin not found",
            )
        })?;
    let bytes = state
        .object_store
        .get(&plugins::blob_path(&sha256))
        .await
        .map_err(|error| anyhow::Error::new(error).context("read plugin binary"))?
        .bytes()
        .await
        .map_err(|error| anyhow::Error::new(error).context("read plugin binary"))?;
    if bytes.len() > MAX_PLUGIN_BYTES {
        return Err(anyhow::anyhow!("stored plugin binary exceeds the size limit").into());
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/wasm")
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(
            PLUGIN_SHA256_HEADER,
            HeaderValue::from_str(&sha256).map_err(anyhow::Error::new)?,
        )
        .body(Body::from(bytes))
        .map_err(|error| anyhow::Error::new(error).into())
}

/// Record what the runtime loaded for a revision of the plugin list.
pub(super) async fn plugins_applied(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path(server_id): Path<Uuid>,
    Json(request): Json<WorkerPluginsApplied>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let mut tx = state.pool.begin().await?;
    lock_current_placement(&mut tx, worker_id, server_id, request.generation).await?;
    plugins::record_applied(&mut tx, server_id, request.revision, &request.plugins).await?;
    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
