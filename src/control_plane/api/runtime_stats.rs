use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use sqlx::Row;

use crate::runtime_stats::{RUNTIME_STATS_FRESHNESS_SECONDS, RuntimeStats};

use super::{
    ApiError, AppState,
    actor::{ApiPath, ApiServerHeaders, ServerIdPath},
    internal::require_server_access,
};

/// Return the latest current-generation runtime sample when it is still fresh.
///
/// A missing or stale sample is represented as JSON `null`: callers must not
/// render an old resource snapshot as if it described the currently running
/// process.
pub(super) async fn runtime_stats(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
) -> Result<Json<Option<RuntimeStats>>, ApiError> {
    let server = require_server_access(&state, &headers, path.server_id).await?;
    let row = sqlx::query(
        "SELECT stats, measured_at
         FROM server_runtime_stats
         WHERE server_id = $1
           AND generation = $2
           AND measured_at > now() - make_interval(secs => $3::double precision)",
    )
    .bind(server.id)
    .bind(server.generation)
    .bind(RUNTIME_STATS_FRESHNESS_SECONDS as f64)
    .fetch_optional(&state.pool)
    .await?;
    let stats = if let Some(row) = row {
        let mut stats: serde_json::Value = row.try_get("stats")?;
        let measured_at: chrono::DateTime<chrono::Utc> = row.try_get("measured_at")?;
        stats["measured_at"] =
            serde_json::to_value(measured_at).expect("chrono UTC timestamps serialize to JSON");
        Some(serde_json::from_value(stats).map_err(|error| {
            tracing::error!(?error, "stored runtime stats are invalid");
            ApiError::with_code(
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid_runtime_stats",
                "stored runtime stats are invalid",
            )
        })?)
    } else {
        None
    };
    Ok(Json(stats))
}
