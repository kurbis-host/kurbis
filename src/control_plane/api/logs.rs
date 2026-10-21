use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::actor::{ApiPath, ApiQuery, ApiServerHeaders, ServerIdPath};
use super::internal::require_server_view;
use super::{ApiError, AppState};

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct LogQuery {
    after_id: Option<i64>,
    limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow, JsonSchema)]
pub struct LogEntry {
    pub id: i64,
    pub created_at: DateTime<Utc>,
    pub line: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LogPage {
    pub items: Vec<LogEntry>,
    pub next_after_id: Option<i64>,
}

pub(super) async fn logs(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
    ApiQuery(query): ApiQuery<LogQuery>,
) -> Result<Json<LogPage>, ApiError> {
    let server_id = path.server_id;
    let server = require_server_view(&state, &headers, server_id).await?;
    let limit = query.limit.unwrap_or(200).clamp(1, 500);
    if query.after_id.is_some_and(|cursor| cursor < 0) {
        return Err(ApiError::with_code(
            StatusCode::BAD_REQUEST,
            "invalid_cursor",
            "after_id must be non-negative",
        ));
    }

    let entries = if let Some(after_id) = query.after_id {
        sqlx::query_as::<_, LogEntry>(
            "SELECT id, created_at, line
             FROM server_logs
             WHERE server_id = $1 AND id > $2
             ORDER BY id ASC LIMIT $3",
        )
        .bind(server.id)
        .bind(after_id)
        .bind(limit)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query_as::<_, LogEntry>(
            "SELECT id, created_at, line
             FROM (
                 SELECT id, created_at, line
                 FROM server_logs
                 WHERE server_id = $1
                 ORDER BY id DESC LIMIT $2
             ) recent
             ORDER BY id ASC",
        )
        .bind(server.id)
        .bind(limit)
        .fetch_all(&state.pool)
        .await?
    };
    let next_after_id = entries.last().map(|entry| entry.id);
    Ok(Json(LogPage {
        items: entries,
        next_after_id,
    }))
}
