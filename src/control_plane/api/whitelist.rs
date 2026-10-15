use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use garde::Validate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::actor::{
    ApiBrowserServerHeaders, ApiJson, ApiPath, ApiQuery, ApiServerCapabilityHeaders,
    ApiServerHeaders, ServerIdPath,
};
use super::internal::{
    require_active_user, require_browser_server_access, require_server_access,
    require_server_capability,
};
use super::player_resolution::{ResolvedPlayer, resolve_player_name, validate_player_name};
use super::server_access::instance_capability_server_id;
use super::{ApiError, AppState, ServerSettings, mutation_error};
use crate::model::ServerRecord;

#[derive(Debug, Clone, Serialize, sqlx::FromRow, JsonSchema)]
pub struct WhitelistEntry {
    pub player_id: Uuid,
    pub name: String,
    /// Who put the player on the whitelist: the owner's account name for
    /// additions in the panel, the player's name for additions in game.
    /// Unknown for a login-link session and for entries older than this field.
    pub added_by: Option<String>,
    /// Whether the player was added from inside the game.
    pub added_in_game: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A player the whitelist turned away in the last day.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, JsonSchema)]
pub struct JoinRequest {
    pub player_id: Uuid,
    pub name: String,
    /// How often they tried, as the server reported it (at most once a minute).
    pub attempts: i32,
    pub first_attempt_at: DateTime<Utc>,
    pub last_attempt_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Whitelist {
    pub enabled: bool,
    /// Whether players on the whitelist may let in someone it turned away.
    pub whitelist_by_members: bool,
    /// Whether the owner's account has a linked Minecraft account, which is
    /// always on the whitelist and an operator. Without one the owner can
    /// mark a join request as their own.
    pub owner_has_minecraft_account: bool,
    pub revision: i64,
    pub applied_revision: i64,
    pub synchronized: bool,
    pub entries: Vec<WhitelistEntry>,
    /// Players turned away in the last day, most recent first.
    pub requests: Vec<JoinRequest>,
}

/// How long a join request stays listed.
const REQUEST_LIFETIME: &str = "24 hours";
/// A server keeps at most this many requests; the oldest go first.
const MAX_REQUESTS: i64 = 50;
/// A retry this soon after the last one is counted but not shown in the
/// server's activity again.
const QUIET_RETRY: &str = "10 minutes";

pub(super) async fn whitelist_state(pool: &PgPool, server_id: Uuid) -> Result<Whitelist, ApiError> {
    let (enabled, settings): (bool, serde_json::Value) = sqlx::query_as(
        "SELECT COALESCE(settings ->> 'whitelist', 'false') = 'true', settings
         FROM servers WHERE id = $1",
    )
    .bind(server_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(ApiError::server_not_found)?;
    let whitelist_by_members = ServerSettings::from_stored(settings)?.whitelist_by_members;
    let owner_has_minecraft_account = owner_has_minecraft_account(pool, server_id).await?;
    let revision = sqlx::query_as::<_, (i64, i64)>(
        "SELECT revision, applied_revision
         FROM server_whitelist_state WHERE server_id = $1",
    )
    .bind(server_id)
    .fetch_optional(pool)
    .await?
    .unwrap_or((0, 0));
    let entries = sqlx::query_as::<_, WhitelistEntry>(
        "SELECT player_id, name, added_by, added_in_game, created_at, updated_at
         FROM whitelist_entries
         WHERE server_id = $1
         ORDER BY lower(name), player_id",
    )
    .bind(server_id)
    .fetch_all(pool)
    .await?;
    let requests = sqlx::query_as::<_, JoinRequest>(&format!(
        "SELECT player_id, name, attempts, first_attempt_at, last_attempt_at
         FROM whitelist_join_requests
         WHERE server_id = $1 AND last_attempt_at > now() - interval '{REQUEST_LIFETIME}'
         ORDER BY last_attempt_at DESC, player_id"
    ))
    .bind(server_id)
    .fetch_all(pool)
    .await?;
    Ok(Whitelist {
        enabled,
        whitelist_by_members,
        owner_has_minecraft_account,
        revision: revision.0,
        applied_revision: revision.1,
        synchronized: revision.0 == revision.1,
        entries,
        requests,
    })
}

async fn owner_has_minecraft_account(
    executor: impl sqlx::PgExecutor<'_>,
    server_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1 FROM servers s JOIN identities i ON i.user_id = s.owner_id
             WHERE s.id = $1 AND i.provider IN ('minecraft_java', 'minecraft_bedrock')
               AND i.minecraft_player_id IS NOT NULL
         )",
    )
    .bind(server_id)
    .fetch_one(executor)
    .await
}

pub(super) async fn whitelist(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
) -> Result<Json<Whitelist>, ApiError> {
    let server_id = path.server_id;
    let server = require_server_access(&state, &headers, server_id).await?;
    Ok(Json(whitelist_state(&state.pool, server.id).await?))
}

#[derive(Debug, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub(super) struct WhitelistMutation {
    /// The Minecraft name to resolve. Java names are looked up through Mojang;
    /// Bedrock names resolve only from this server's live players or stored roster.
    #[garde(length(min = 1, max = 64))]
    name: String,
}

pub(super) async fn add_whitelist_entry(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
    ApiJson(input): ApiJson<WhitelistMutation>,
) -> Result<(StatusCode, Json<Whitelist>), ApiError> {
    let server_id = path.server_id;
    let server = require_server_access(&state, &headers, server_id).await?;
    // Builds of the control plugin from before `whitelist/in-game` add here
    // without saying who in game asked.
    let from_server =
        instance_capability_server_id(&state.pool, &headers).await? == Some(server.id);
    let added_by = if from_server {
        None
    } else {
        panel_actor_name(&state, &headers, &server).await
    };
    let resolved = resolve_player_name(&state, server.id, &input.name).await?;
    insert_entry(
        &state.pool,
        &server,
        &resolved,
        added_by.as_deref(),
        from_server,
    )
    .await
}

#[derive(Debug, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub(super) struct InGameWhitelistMutation {
    /// The player to add, as Pumpkin named them.
    #[garde(length(min = 1, max = 64))]
    name: String,
    /// The UUID Pumpkin reported when it turned this player away. A Bedrock
    /// player who has never been on the server cannot be resolved by name, so
    /// the runtime that saw them vouches for it, as it does for the player
    /// roster. Without it the name is resolved like a panel addition.
    #[serde(default)]
    player_id: Option<Uuid>,
    /// The player in game who adds them. An operator always may; a player on
    /// the whitelist may while the server's `whitelist_by_members` setting is on.
    #[garde(dive)]
    added_by: InGamePlayer,
}

#[derive(Debug, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub(super) struct InGamePlayer {
    #[garde(skip)]
    player_id: Uuid,
    #[garde(length(min = 1, max = 64))]
    name: String,
}

/// Add a player from in game, for the server's own plugin only. The plugin
/// vouches for who is asking; the operator roster, the whitelist and the
/// server's settings decide whether that player may add someone.
pub(super) async fn add_whitelist_entry_in_game(
    State(state): State<Arc<AppState>>,
    headers: ApiServerCapabilityHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
    ApiJson(input): ApiJson<InGameWhitelistMutation>,
) -> Result<(StatusCode, Json<Whitelist>), ApiError> {
    let server = require_server_capability(&state, &headers, path.server_id).await?;
    require_may_add_in_game(&state.pool, &server, &input.added_by).await?;
    let added_by = validate_player_name(&input.added_by.name)?.to_owned();
    let resolved = match input.player_id {
        Some(player_id) if !player_id.is_nil() => ResolvedPlayer {
            player_id,
            name: validate_player_name(&input.name)?.to_owned(),
        },
        Some(_) => {
            return Err(ApiError::with_code(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_player_id",
                "player_id must not be the nil UUID",
            ));
        }
        None => resolve_player_name(&state, server.id, &input.name).await?,
    };
    insert_entry(&state.pool, &server, &resolved, Some(&added_by), true).await
}

async fn insert_entry(
    pool: &PgPool,
    server: &ServerRecord,
    resolved: &ResolvedPlayer,
    added_by: Option<&str>,
    added_in_game: bool,
) -> Result<(StatusCode, Json<Whitelist>), ApiError> {
    let mut tx = pool.begin().await?;
    write_entry(&mut tx, server.id, resolved, added_by, added_in_game).await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(whitelist_state(pool, server.id).await?),
    ))
}

/// Put a player on the whitelist and drop their join request. Adding
/// someone already on the list refreshes their name and keeps who first
/// added them.
async fn write_entry(
    tx: &mut sqlx::PgConnection,
    server_id: Uuid,
    resolved: &ResolvedPlayer,
    added_by: Option<&str>,
    added_in_game: bool,
) -> Result<(), ApiError> {
    let name = validate_player_name(&resolved.name)?;
    sqlx::query(
        "INSERT INTO whitelist_entries(server_id, player_id, name, added_by, added_in_game)
         VALUES($1, $2, $3, $4, $5)
         ON CONFLICT(server_id, player_id) DO UPDATE
           SET name = EXCLUDED.name, updated_at = now()",
    )
    .bind(server_id)
    .bind(resolved.player_id)
    .bind(name)
    .bind(added_by)
    .bind(added_in_game)
    .execute(&mut *tx)
    .await
    .map_err(|error| mutation_error(error, "player name is already whitelisted"))?;
    sqlx::query("DELETE FROM whitelist_join_requests WHERE server_id = $1 AND player_id = $2")
        .bind(server_id)
        .bind(resolved.player_id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

#[derive(Debug, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub(super) struct JoinRequestReport {
    /// The UUID Pumpkin gave the player it turned away.
    #[garde(skip)]
    player_id: Uuid,
    #[garde(length(min = 1, max = 64))]
    name: String,
}

/// The server's plugin reports a player the whitelist turned away. The
/// runtime vouches for the UUID, as it does for the player roster.
pub(super) async fn report_join_request(
    State(state): State<Arc<AppState>>,
    headers: ApiServerCapabilityHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
    ApiJson(input): ApiJson<JoinRequestReport>,
) -> Result<Json<Whitelist>, ApiError> {
    let server = require_server_capability(&state, &headers, path.server_id).await?;
    let name = validate_player_name(&input.name)?;
    if input.player_id.is_nil() {
        return Err(ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_player_id",
            "player_id must not be the nil UUID",
        ));
    }
    let mut tx = state.pool.begin().await?;
    // Serialize reports for one server so the cap and the activity line
    // stay consistent.
    sqlx::query("SELECT 1 FROM servers WHERE id = $1 FOR UPDATE")
        .bind(server.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(&format!(
        "DELETE FROM whitelist_join_requests
         WHERE server_id = $1 AND last_attempt_at <= now() - interval '{REQUEST_LIFETIME}'"
    ))
    .bind(server.id)
    .execute(&mut *tx)
    .await?;
    let whitelisted: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM whitelist_entries WHERE server_id = $1 AND player_id = $2)",
    )
    .bind(server.id)
    .bind(input.player_id)
    .fetch_one(&mut *tx)
    .await?;
    if !whitelisted {
        let recent: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS(SELECT 1 FROM whitelist_join_requests
                           WHERE server_id = $1 AND player_id = $2
                             AND last_attempt_at > now() - interval '{QUIET_RETRY}')"
        ))
        .bind(server.id)
        .bind(input.player_id)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO whitelist_join_requests(server_id, player_id, name)
             VALUES($1, $2, $3)
             ON CONFLICT(server_id, player_id) DO UPDATE
               SET name = EXCLUDED.name,
                   attempts = whitelist_join_requests.attempts + 1,
                   last_attempt_at = now()",
        )
        .bind(server.id)
        .bind(input.player_id)
        .bind(name)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM whitelist_join_requests
             WHERE server_id = $1 AND player_id IN (
                 SELECT player_id FROM whitelist_join_requests WHERE server_id = $1
                 ORDER BY last_attempt_at DESC, player_id OFFSET $2
             )",
        )
        .bind(server.id)
        .bind(MAX_REQUESTS)
        .execute(&mut *tx)
        .await?;
        // A new request (or one after a quiet spell) shows in the activity
        // and refreshes open panels; quick retries only count.
        if !recent {
            sqlx::query("SELECT emit_server_event($1, 'whitelist', $2)")
                .bind(server.id)
                .bind(format!("{name} tried to join but is not on the whitelist"))
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(Json(whitelist_state(&state.pool, server.id).await?))
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct JoinRequestPath {
    server_id: Uuid,
    player_id: Uuid,
}

async fn join_request(
    executor: impl sqlx::PgExecutor<'_>,
    server_id: Uuid,
    player_id: Uuid,
) -> Result<ResolvedPlayer, ApiError> {
    let name: Option<String> = sqlx::query_scalar(&format!(
        "SELECT name FROM whitelist_join_requests
         WHERE server_id = $1 AND player_id = $2
           AND last_attempt_at > now() - interval '{REQUEST_LIFETIME}'"
    ))
    .bind(server_id)
    .bind(player_id)
    .fetch_optional(executor)
    .await?;
    name.map(|name| ResolvedPlayer { player_id, name })
        .ok_or_else(|| {
            ApiError::with_code(
                StatusCode::NOT_FOUND,
                "join_request_not_found",
                "nobody with that id was turned away in the last day",
            )
        })
}

/// Let in a player the whitelist turned away, from the panel.
pub(super) async fn let_in_join_request(
    State(state): State<Arc<AppState>>,
    headers: ApiBrowserServerHeaders,
    ApiPath(path): ApiPath<JoinRequestPath>,
) -> Result<(StatusCode, Json<Whitelist>), ApiError> {
    let server = require_browser_server_access(&state, &headers, path.server_id).await?;
    let resolved = join_request(&state.pool, server.id, path.player_id).await?;
    let added_by = panel_actor_name(&state, &headers, &server).await;
    insert_entry(&state.pool, &server, &resolved, added_by.as_deref(), false).await
}

/// The owner says a turned-away player is themselves: that player goes on
/// the whitelist and becomes an operator. This does not link the Minecraft
/// account to the owner's Kurbis account, so it grants nothing outside the
/// game, and it is offered only while the owner has no linked Minecraft
/// account (which would already be on the whitelist and an operator).
pub(super) async fn claim_join_request(
    State(state): State<Arc<AppState>>,
    headers: ApiBrowserServerHeaders,
    ApiPath(path): ApiPath<JoinRequestPath>,
) -> Result<(StatusCode, Json<Whitelist>), ApiError> {
    let server = require_browser_server_access(&state, &headers, path.server_id).await?;
    let owner = require_active_user(&state, &headers)
        .await
        .ok()
        .filter(|user| user.id == server.owner_id)
        .ok_or_else(|| {
            ApiError::with_code(
                StatusCode::FORBIDDEN,
                "owner_only",
                "only the server's owner can mark a join request as their own",
            )
        })?;
    let mut tx = state.pool.begin().await?;
    crate::control_plane::identity::lock_identity_writes(&mut tx).await?;
    if owner_has_minecraft_account(&mut *tx, server.id).await? {
        return Err(ApiError::with_code(
            StatusCode::CONFLICT,
            "owner_has_minecraft_account",
            "your linked Minecraft account is already on the whitelist and an operator",
        ));
    }
    let resolved = join_request(&mut *tx, server.id, path.player_id).await?;
    let owner_name = owner.display_name.trim();
    let owner_name: String = owner_name.chars().take(64).collect();
    write_entry(
        &mut tx,
        server.id,
        &resolved,
        (!owner_name.is_empty()).then_some(owner_name.as_str()),
        false,
    )
    .await?;
    sqlx::query(
        "INSERT INTO operator_entries(server_id, player_id, name)
         VALUES($1, $2, $3)
         ON CONFLICT(server_id, player_id) DO UPDATE
           SET name = EXCLUDED.name, updated_at = now()",
    )
    .bind(server.id)
    .bind(resolved.player_id)
    .bind(validate_player_name(&resolved.name)?)
    .execute(&mut *tx)
    .await
    .map_err(|error| mutation_error(error, "player is already managed as an operator"))?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(whitelist_state(&state.pool, server.id).await?),
    ))
}

/// Forget a join request without letting the player in.
pub(super) async fn dismiss_join_request(
    State(state): State<Arc<AppState>>,
    headers: ApiBrowserServerHeaders,
    ApiPath(path): ApiPath<JoinRequestPath>,
) -> Result<Json<Whitelist>, ApiError> {
    let server = require_browser_server_access(&state, &headers, path.server_id).await?;
    let result =
        sqlx::query("DELETE FROM whitelist_join_requests WHERE server_id = $1 AND player_id = $2")
            .bind(server.id)
            .bind(path.player_id)
            .execute(&state.pool)
            .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::with_code(
            StatusCode::NOT_FOUND,
            "join_request_not_found",
            "nobody with that id was turned away in the last day",
        ));
    }
    Ok(Json(whitelist_state(&state.pool, server.id).await?))
}

/// The owner's account name when the owner adds a player in the panel. A
/// login-link session does not say who opened it.
async fn panel_actor_name(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    server: &ServerRecord,
) -> Option<String> {
    let user = require_active_user(state, headers).await.ok()?;
    let name = user.display_name.trim();
    (user.id == server.owner_id && !name.is_empty()).then(|| name.chars().take(64).collect())
}

/// An operator in game may always add a player; a player on the whitelist may
/// while the owner lets members add players.
async fn require_may_add_in_game(
    pool: &PgPool,
    server: &ServerRecord,
    adder: &InGamePlayer,
) -> Result<(), ApiError> {
    let (operator, member): (bool, bool) = sqlx::query_as(
        "SELECT
             EXISTS(SELECT 1 FROM operator_entries WHERE server_id = $1 AND player_id = $3)
             OR EXISTS(SELECT 1 FROM identities
                       WHERE user_id = $2 AND minecraft_player_id = $3
                         AND provider IN ('minecraft_java', 'minecraft_bedrock')),
             EXISTS(SELECT 1 FROM whitelist_entries WHERE server_id = $1 AND player_id = $3)",
    )
    .bind(server.id)
    .bind(server.owner_id)
    .bind(adder.player_id)
    .fetch_one(pool)
    .await?;
    if operator {
        return Ok(());
    }
    let members_may_add =
        ServerSettings::from_stored(server.settings.clone())?.whitelist_by_members;
    if member && members_may_add {
        return Ok(());
    }
    Err(ApiError::with_code(
        StatusCode::FORBIDDEN,
        "whitelist_add_not_allowed",
        if member {
            "only operators may add players to this server's whitelist"
        } else {
            "only players on the whitelist may add other players"
        },
    ))
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct WhitelistDeleteQuery {
    player_id: Uuid,
}

pub(super) async fn remove_whitelist_entry(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
    ApiQuery(query): ApiQuery<WhitelistDeleteQuery>,
) -> Result<Json<Whitelist>, ApiError> {
    let server_id = path.server_id;
    let server = require_server_access(&state, &headers, server_id).await?;
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
