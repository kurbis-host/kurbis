use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::actor::{ApiPath, ApiServerHeaders, ServerIdPath};
use super::internal::require_server_access;
use super::{ApiError, AppState};

#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlayerEdition {
    Java,
    Bedrock,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PlayerPresence {
    pub player_id: Uuid,
    pub name: String,
    pub edition: PlayerEdition,
    pub connected_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
pub(super) struct PlayerRow {
    pub(super) player_id: Uuid,
    pub(super) name: String,
    pub(super) edition: String,
    pub(super) connected_at: DateTime<Utc>,
    pub(super) last_seen_at: DateTime<Utc>,
}

impl TryFrom<PlayerRow> for PlayerPresence {
    type Error = ApiError;

    fn try_from(row: PlayerRow) -> Result<Self, Self::Error> {
        let edition = match row.edition.as_str() {
            "java" => PlayerEdition::Java,
            "bedrock" => PlayerEdition::Bedrock,
            _ => {
                return Err(ApiError::with_code(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "invalid_player_edition",
                    "database contains an unknown player edition",
                ));
            }
        };
        Ok(Self {
            player_id: row.player_id,
            name: row.name,
            edition,
            connected_at: row.connected_at,
            last_seen_at: row.last_seen_at,
        })
    }
}

pub(super) async fn player_list(
    pool: &PgPool,
    server_id: Uuid,
) -> Result<Vec<PlayerPresence>, ApiError> {
    sqlx::query_as::<_, PlayerRow>(
        "SELECT player_id, name, edition, connected_at, last_seen_at
         FROM server_players
         WHERE server_id = $1
         ORDER BY connected_at ASC, player_id ASC",
    )
    .bind(server_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(PlayerPresence::try_from)
    .collect()
}

pub(super) async fn players(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
) -> Result<Json<Vec<PlayerPresence>>, ApiError> {
    let server_id = path.server_id;
    let server = require_server_access(&state, &headers, server_id).await?;
    Ok(Json(player_list(&state.pool, server.id).await?))
}
