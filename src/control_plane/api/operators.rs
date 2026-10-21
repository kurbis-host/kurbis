use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use garde::Validate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::actor::{ApiBrowserServerHeaders, ApiJson, ApiPath, ApiQuery, ServerIdPath};
use super::internal::{require_browser_server_access, require_browser_server_view};
use super::player_resolution::{resolve_player_name, validate_player_name};
use super::{ApiError, AppState, mutation_error};

#[derive(Debug, Clone, Serialize, sqlx::FromRow, JsonSchema)]
pub struct OperatorEntry {
    pub player_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub is_owner: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Operators {
    pub revision: i64,
    pub applied_revision: i64,
    pub synchronized: bool,
    pub entries: Vec<OperatorEntry>,
}

pub(super) async fn operators_state(pool: &PgPool, server_id: Uuid) -> Result<Operators, ApiError> {
    let revision = sqlx::query_as::<_, (i64, i64)>(
        "SELECT revision, applied_revision
         FROM server_operator_state WHERE server_id = $1",
    )
    .bind(server_id)
    .fetch_optional(pool)
    .await?
    .unwrap_or((1, 0));
    let entries = sqlx::query_as::<_, OperatorEntry>(
        "WITH managed AS (
             SELECT player_id, name, created_at, updated_at
             FROM operator_entries WHERE server_id = $1
         ), owners AS (
             SELECT DISTINCT ON (i.minecraft_player_id)
                    i.minecraft_player_id AS player_id, i.display_name AS name,
                    i.created_at, i.updated_at
             FROM servers s JOIN identities i ON i.user_id = s.owner_id
             WHERE s.id = $1 AND i.provider IN ('minecraft_java', 'minecraft_bedrock')
               AND i.minecraft_player_id IS NOT NULL
             ORDER BY i.minecraft_player_id, i.created_at, i.id
         )
         SELECT COALESCE(o.player_id, m.player_id) AS player_id,
                COALESCE(o.name, m.name) AS name,
                COALESCE(m.created_at, o.created_at) AS created_at,
                COALESCE(m.updated_at, o.updated_at) AS updated_at,
                (o.player_id IS NOT NULL) AS is_owner
         FROM managed m FULL OUTER JOIN owners o ON o.player_id = m.player_id
         ORDER BY lower(COALESCE(o.name, m.name)), COALESCE(o.player_id, m.player_id)",
    )
    .bind(server_id)
    .fetch_all(pool)
    .await?;
    Ok(Operators {
        revision: revision.0,
        applied_revision: revision.1,
        synchronized: revision.0 == revision.1,
        entries,
    })
}

pub(super) async fn operators(
    State(state): State<Arc<AppState>>,
    headers: ApiBrowserServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
) -> Result<Json<Operators>, ApiError> {
    let server = require_browser_server_view(&state, &headers, path.server_id).await?;
    Ok(Json(operators_state(&state.pool, server.id).await?))
}

#[derive(Debug, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub(super) struct OperatorMutation {
    /// Java names are resolved with Mojang. Bedrock names resolve only while
    /// the player is online on this server or already has a stored roster entry.
    #[garde(length(min = 1, max = 64))]
    name: String,
}

pub(super) async fn add_operator(
    State(state): State<Arc<AppState>>,
    headers: ApiBrowserServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
    ApiJson(input): ApiJson<OperatorMutation>,
) -> Result<(StatusCode, Json<Operators>), ApiError> {
    let server = require_browser_server_access(&state, &headers, path.server_id).await?;
    let resolved = resolve_player_name(&state, server.id, &input.name).await?;
    let name = validate_player_name(&resolved.name)?.to_owned();
    let mut tx = state.pool.begin().await?;
    crate::control_plane::identity::lock_identity_writes(&mut tx).await?;
    let is_owner: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1 FROM servers s JOIN identities i ON i.user_id = s.owner_id
             WHERE s.id = $1 AND i.provider IN ('minecraft_java', 'minecraft_bedrock')
               AND i.minecraft_player_id = $2
         )",
    )
    .bind(server.id)
    .bind(resolved.player_id)
    .fetch_one(&mut *tx)
    .await?;
    if is_owner {
        return Err(ApiError::with_code(
            StatusCode::CONFLICT,
            "owner_operator_always_enabled",
            "the server owner is always an operator and cannot be managed as a regular entry",
        ));
    }
    sqlx::query(
        "INSERT INTO operator_entries(server_id, player_id, name)
         VALUES($1, $2, $3)
         ON CONFLICT(server_id, player_id) DO UPDATE
           SET name = EXCLUDED.name, updated_at = now()",
    )
    .bind(server.id)
    .bind(resolved.player_id)
    .bind(name)
    .execute(&mut *tx)
    .await
    .map_err(|error| mutation_error(error, "player is already managed as an operator"))?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(operators_state(&state.pool, server.id).await?),
    ))
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct OperatorDeleteQuery {
    player_id: Uuid,
}

pub(super) async fn remove_operator(
    State(state): State<Arc<AppState>>,
    headers: ApiBrowserServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
    ApiQuery(query): ApiQuery<OperatorDeleteQuery>,
) -> Result<Json<Operators>, ApiError> {
    let server = require_browser_server_access(&state, &headers, path.server_id).await?;
    let mut tx = state.pool.begin().await?;
    crate::control_plane::identity::lock_identity_writes(&mut tx).await?;
    let result = sqlx::query(
        "DELETE FROM operator_entries e
         WHERE e.server_id = $1 AND e.player_id = $2
           AND NOT EXISTS (
               SELECT 1 FROM servers s JOIN identities i ON i.user_id = s.owner_id
               WHERE s.id = e.server_id
                 AND i.provider IN ('minecraft_java', 'minecraft_bedrock')
                 AND i.minecraft_player_id = e.player_id
           )",
    )
    .bind(server.id)
    .bind(query.player_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::with_code(
            StatusCode::NOT_FOUND,
            "operator_entry_not_found",
            "operator entry not found",
        ));
    }
    Ok(Json(operators_state(&state.pool, server.id).await?))
}
