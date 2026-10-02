use std::sync::Arc;

use crate::model::SnapshotRecord as DbSnapshot;
use axum::{Json, extract::State};

use super::actor::{ApiPath, ApiServerHeaders, ServerIdPath};
use super::internal::require_server_access;
use super::types::Snapshot;
use super::{ApiError, AppState};

pub(super) async fn snapshots(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
) -> Result<Json<Vec<Snapshot>>, ApiError> {
    let server_id = path.server_id;
    let server = require_server_access(&state, &headers, server_id).await?;
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
