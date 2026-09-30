use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::internal::require_owned_server;
use super::{ApiError, AppState, mutation_error};

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct WhitelistEntry {
    pub player_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct Whitelist {
    pub enabled: bool,
    pub revision: i64,
    pub applied_revision: i64,
    pub synchronized: bool,
    pub entries: Vec<WhitelistEntry>,
}

pub(super) async fn whitelist_state(pool: &PgPool, server_id: Uuid) -> Result<Whitelist, ApiError> {
    let enabled: bool = sqlx::query_scalar(
        "SELECT COALESCE(settings ->> 'whitelist', 'false') = 'true'
         FROM servers WHERE id = $1",
    )
    .bind(server_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| {
        ApiError::with_code(
            StatusCode::NOT_FOUND,
            "server_not_found",
            "server not found",
        )
    })?;
    let revision = sqlx::query_as::<_, (i64, i64)>(
        "SELECT revision, applied_revision
         FROM server_whitelist_state WHERE server_id = $1",
    )
    .bind(server_id)
    .fetch_optional(pool)
    .await?
    .unwrap_or((0, 0));
    let entries = sqlx::query_as::<_, WhitelistEntry>(
        "SELECT player_id, name, created_at, updated_at
         FROM whitelist_entries
         WHERE server_id = $1
         ORDER BY lower(name), player_id",
    )
    .bind(server_id)
    .fetch_all(pool)
    .await?;
    Ok(Whitelist {
        enabled,
        revision: revision.0,
        applied_revision: revision.1,
        synchronized: revision.0 == revision.1,
        entries,
    })
}

pub(super) async fn whitelist(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Whitelist>, ApiError> {
    let server = require_owned_server(&state, &headers).await?;
    Ok(Json(whitelist_state(&state.pool, server.id).await?))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WhitelistMutation {
    player_id: Uuid,
    name: String,
}

fn validate_player_name(name: &str) -> Result<&str, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_player_name",
            "player name must contain 1 to 64 printable characters",
        ));
    }
    Ok(name)
}

pub(super) async fn add_whitelist_entry(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(input): Json<WhitelistMutation>,
) -> Result<(StatusCode, Json<Whitelist>), ApiError> {
    let server = require_owned_server(&state, &headers).await?;
    let name = validate_player_name(&input.name)?;
    let result = sqlx::query(
        "INSERT INTO whitelist_entries(server_id, player_id, name)
         VALUES($1, $2, $3)
         ON CONFLICT(server_id, player_id) DO UPDATE
           SET name = EXCLUDED.name, updated_at = now()",
    )
    .bind(server.id)
    .bind(input.player_id)
    .bind(name)
    .execute(&state.pool)
    .await;
    result.map_err(|error| mutation_error(error, "player name is already whitelisted"))?;
    Ok((
        StatusCode::CREATED,
        Json(whitelist_state(&state.pool, server.id).await?),
    ))
}

#[derive(Debug, Deserialize)]
pub(super) struct WhitelistDeleteQuery {
    player_id: Uuid,
}

pub(super) async fn remove_whitelist_entry(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WhitelistDeleteQuery>,
) -> Result<Json<Whitelist>, ApiError> {
    let server = require_owned_server(&state, &headers).await?;
    let result = sqlx::query(
        "DELETE FROM whitelist_entries
         WHERE server_id = $1 AND player_id = $2",
    )
    .bind(server.id)
    .bind(query.player_id)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::with_code(
            StatusCode::NOT_FOUND,
            "whitelist_entry_not_found",
            "whitelist entry not found",
        ));
    }
    Ok(Json(whitelist_state(&state.pool, server.id).await?))
}
