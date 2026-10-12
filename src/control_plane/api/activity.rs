use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::actor::{ApiPath, ApiQuery, ApiServerHeaders, ServerIdPath};
use super::internal::require_server_access;
use super::{ApiError, AppState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServerEventKind {
    Server,
    Operation,
    Snapshot,
    Log,
    Players,
    Whitelist,
    Operators,
    Ai,
}

impl TryFrom<&str> for ServerEventKind {
    type Error = ApiError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "server" => Ok(Self::Server),
            "operation" => Ok(Self::Operation),
            "snapshot" => Ok(Self::Snapshot),
            "log" => Ok(Self::Log),
            "players" => Ok(Self::Players),
            "whitelist" => Ok(Self::Whitelist),
            "operators" => Ok(Self::Operators),
            "ai" => Ok(Self::Ai),
            _ => Err(ApiError::with_code(
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid_server_event",
                "database contains an unknown server event type",
            )),
        }
    }
}

impl ServerEventKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Operation => "operation",
            Self::Snapshot => "snapshot",
            Self::Log => "log",
            Self::Players => "players",
            Self::Whitelist => "whitelist",
            Self::Operators => "operators",
            Self::Ai => "ai",
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ActivityEntry {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: ServerEventKind,
    pub summary: String,
    pub detail: Option<String>,
    pub status: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct LimitQuery {
    limit: Option<i64>,
}

#[derive(sqlx::FromRow)]
pub(super) struct EventRow {
    pub(super) id: i64,
    pub(super) event_type: String,
    pub(super) created_at: DateTime<Utc>,
}

pub(super) async fn activity(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
    ApiQuery(query): ApiQuery<LimitQuery>,
) -> Result<Json<Vec<ActivityEntry>>, ApiError> {
    let server_id = path.server_id;
    let server = require_server_access(&state, &headers, server_id).await?;
    let rows = sqlx::query_as::<_, EventRow>(
        "SELECT id, event_type, created_at
         FROM server_events
         WHERE server_id = $1
         ORDER BY id DESC LIMIT $2",
    )
    .bind(server.id)
    .bind(query.limit.unwrap_or(100).clamp(1, 500))
    .fetch_all(&state.pool)
    .await?;

    let entries = rows
        .into_iter()
        .map(|row| {
            let kind = ServerEventKind::try_from(row.event_type.as_str())?;
            let summary = match kind {
                ServerEventKind::Server => "Server state changed",
                ServerEventKind::Operation => "Lifecycle operation changed",
                ServerEventKind::Snapshot => "Snapshot history changed",
                ServerEventKind::Log => "Server log updated",
                ServerEventKind::Players => "Player presence changed",
                ServerEventKind::Whitelist => "Whitelist changed",
                ServerEventKind::Operators => "Operator roster changed",
                ServerEventKind::Ai => "AI assistant activity",
            }
            .to_owned();
            Ok(ActivityEntry {
                id: row.id,
                kind,
                summary,
                detail: None,
                status: None,
                created_at: row.created_at,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(Json(entries))
}
