use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};

use crate::model::{ObservedState, SnapshotRecord as DbSnapshot};

use super::internal::require_owned_server;
use super::server::ServerActionResponse;
use super::types::Snapshot;
use super::{ApiError, AppState, lifecycle_error};

pub(super) async fn snapshots(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<Snapshot>>, ApiError> {
    let server = require_owned_server(&state, &headers).await?;
    let snapshots = sqlx::query_as::<_, DbSnapshot>(
        "SELECT id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete,
                created_at
         FROM snapshots
         WHERE server_id = $1 AND complete
         ORDER BY generation DESC",
    )
    .bind(server.id)
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(Snapshot::from)
    .collect();
    Ok(Json(snapshots))
}

pub(super) async fn restore_snapshot(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(generation): Path<i64>,
) -> Result<Json<ServerActionResponse>, ApiError> {
    let server = require_owned_server(&state, &headers).await?;
    if server.observed_state != ObservedState::Sleeping {
        return Err(ApiError::with_code(
            StatusCode::CONFLICT,
            "server_not_sleeping",
            "snapshot restore requires a sleeping server",
        ));
    }
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM snapshots
            WHERE server_id = $1 AND generation = $2 AND complete
         )",
    )
    .bind(server.id)
    .bind(generation)
    .fetch_one(&state.pool)
    .await?;
    if !exists {
        return Err(ApiError::with_code(
            StatusCode::NOT_FOUND,
            "snapshot_not_found",
            "snapshot not found",
        ));
    }

    let observed_state = state
        .lifecycle
        .request_restore(server.id, generation)
        .await
        .map_err(lifecycle_error)?;
    Ok(Json(ServerActionResponse { observed_state }))
}
