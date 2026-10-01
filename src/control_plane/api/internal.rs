use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    control_plane::identity,
    model::{ObservedState, RouteTarget, ServerRecord, subdomain_from_hostname},
    pumpkin_control::VerifiedMinecraftIdentity,
};

use super::auth;
use super::auth::CurrentUser;
use super::server::ServerActionResponse;
use super::types::ServerSettings;
use super::{ApiError, AppState, lifecycle_error};

pub(super) async fn resolve(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(hostname): Path<String>,
) -> Result<Json<RouteTarget>, ApiError> {
    require_internal(&state, &headers)?;
    let subdomain = subdomain_from_hostname(&hostname, &state.config.root_domain)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown hostname"))?;
    let target = resolve_subdomain(&state.pool, &subdomain)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown hostname"))?;
    Ok(Json(target))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AdmissionRequest {
    hostname: String,
    identity: Option<VerifiedMinecraftIdentity>,
    minecraft_player_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub(super) struct AdmissionResponse {
    target: RouteTarget,
    private_message: Option<String>,
    created: bool,
}

pub(super) async fn admit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AdmissionRequest>,
) -> Result<Json<AdmissionResponse>, ApiError> {
    require_internal(&state, &headers)?;
    let subdomain = subdomain_from_hostname(&request.hostname, &state.config.root_domain)
        .ok_or_else(|| {
            ApiError::with_code(
                StatusCode::NOT_FOUND,
                "invalid_hostname",
                "unknown server address",
            )
        })?;
    if let Some(target) = resolve_subdomain(&state.pool, &subdomain).await? {
        return Ok(Json(AdmissionResponse {
            target,
            private_message: None,
            created: false,
        }));
    }

    let identity = request.identity.as_ref().ok_or_else(|| {
        ApiError::with_code(
            StatusCode::NOT_FOUND,
            "unknown_hostname",
            "unknown server address",
        )
    })?;
    let profile_id = request.minecraft_player_id.ok_or_else(|| {
        ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_identity",
            "verified Minecraft profile is missing",
        )
    })?;
    let key = identity::minecraft_identity_key(identity, profile_id).map_err(|message| {
        ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_identity",
            message,
        )
    })?;
    let mut tx = state.pool.begin().await?;
    let owner_id = identity::resolve_minecraft_owner(&mut tx, &key, profile_id).await?;
    let Some(owner_id) = owner_id else {
        tx.rollback().await?;
        return Err(ApiError::with_code(
            StatusCode::NOT_FOUND,
            "identity_not_linked",
            "link this Minecraft account from the account page before creating a server",
        ));
    };
    let account_status: Option<String> =
        sqlx::query_scalar("SELECT account_status FROM users WHERE id = $1 FOR UPDATE")
            .bind(owner_id)
            .fetch_optional(&mut *tx)
            .await?;
    if account_status.as_deref() != Some("active") {
        tx.rollback().await?;
        return Err(ApiError::with_code(
            StatusCode::FORBIDDEN,
            "account_suspended",
            "account is suspended",
        ));
    }
    let identity_still_linked: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM identities WHERE provider = $1 AND subject = $2 AND minecraft_player_id = $3 AND user_id = $4)",
    )
    .bind(key.provider)
    .bind(&key.subject)
    .bind(profile_id)
    .bind(owner_id)
    .fetch_one(&mut *tx)
    .await?;
    if !identity_still_linked {
        tx.rollback().await?;
        return Err(ApiError::with_code(
            StatusCode::CONFLICT,
            "identity_changed",
            "Minecraft account linking changed during admission",
        ));
    }

    let existing_address = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM servers WHERE subdomain = $1 AND desired_state <> 'deleted'",
    )
    .bind(&subdomain)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(server_id) = existing_address {
        tx.commit().await?;
        let target = resolve_subdomain(&state.pool, &subdomain)
            .await?
            .ok_or_else(|| {
                ApiError::with_code(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "route_unavailable",
                    "server route is temporarily unavailable",
                )
            })?;
        let _ = server_id;
        return Ok(Json(AdmissionResponse {
            target,
            private_message: None,
            created: false,
        }));
    }

    let account_has_server: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM servers WHERE owner_id = $1)")
            .bind(owner_id)
            .fetch_one(&mut *tx)
            .await?;

    let default_server: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, subdomain FROM servers WHERE owner_id = $1 AND desired_state <> 'deleted'
         ORDER BY is_default DESC, created_at, id LIMIT 1",
    )
    .bind(owner_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((server_id, default_subdomain)) = default_server {
        tx.commit().await?;
        let target = resolve_subdomain(&state.pool, &default_subdomain)
            .await?
            .ok_or_else(|| {
                ApiError::with_code(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "route_unavailable",
                    "default server route is temporarily unavailable",
                )
            })?;
        let _ = server_id;
        let address = format!("{default_subdomain}.{}", state.config.root_domain);
        return Ok(Json(AdmissionResponse {
            target,
            private_message: Some(format!(
                "You already have a server at {address}; connecting you there."
            )),
            created: false,
        }));
    }
    if account_has_server {
        return Err(ApiError::with_code(
            StatusCode::CONFLICT,
            "server_deletion_in_progress",
            "your previous server is still being removed",
        ));
    }

    let insert = super::server::insert_server_record(
        &mut tx,
        owner_id,
        &subdomain,
        ServerSettings::default().into_stored(),
    )
    .await;
    let server_id = match insert {
        Ok(server) => server.id,
        Err(error) => {
            tx.rollback().await?;
            if let Some(target) = resolve_subdomain(&state.pool, &subdomain).await? {
                return Ok(Json(AdmissionResponse {
                    target,
                    private_message: None,
                    created: false,
                }));
            }
            return Err(super::mutation_error(
                error,
                "server address or account already exists",
            ));
        }
    };
    tx.commit().await?;

    state
        .lifecycle
        .request_start(server_id, "gateway_join")
        .await
        .map_err(lifecycle_error)?;
    let target = resolve_subdomain(&state.pool, &subdomain)
        .await?
        .ok_or_else(|| {
            ApiError::with_code(
                StatusCode::SERVICE_UNAVAILABLE,
                "route_unavailable",
                "server is being prepared",
            )
        })?;
    Ok(Json(AdmissionResponse {
        target,
        private_message: None,
        created: true,
    }))
}

pub(super) async fn wake(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(hostname): Path<String>,
) -> Result<Json<ServerActionResponse>, ApiError> {
    require_internal(&state, &headers)?;
    let subdomain = subdomain_from_hostname(&hostname, &state.config.root_domain)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown hostname"))?;
    let server_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM servers WHERE subdomain = $1 AND desired_state <> 'deleted'",
    )
    .bind(&subdomain)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown hostname"))?;
    let observed_state = state
        .lifecycle
        .request_start(server_id, "gateway_join")
        .await
        .map_err(lifecycle_error)?;
    Ok(Json(ServerActionResponse { observed_state }))
}

pub(super) async fn mint_minecraft_link(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: Json<auth::MinecraftLinkRequest>,
) -> Result<Json<auth::MinecraftLinkResponse>, ApiError> {
    require_internal(&state, &headers)?;
    auth::create_minecraft_link(State(state), request).await
}

pub async fn resolve_subdomain(
    pool: &PgPool,
    subdomain: &str,
) -> Result<Option<RouteTarget>, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        server_id: Uuid,
        subdomain: String,
        state: ObservedState,
        account_suspended: bool,
        worker_healthy: Option<bool>,
        worker_valid_until: Option<DateTime<Utc>>,
        worker_address: Option<String>,
        java_port: Option<i32>,
        bedrock_port: Option<i32>,
    }
    let row = sqlx::query_as::<_, Row>(
        "SELECT s.id AS server_id, s.subdomain, s.observed_state AS state,
                u.account_status <> 'active' AS account_suspended,
                CASE WHEN p.server_id IS NULL THEN NULL
                     ELSE w.heartbeat_at > now() - interval '30 seconds'
                END AS worker_healthy,
                w.heartbeat_at + interval '30 seconds' AS worker_valid_until,
                CASE WHEN u.account_status = 'active' AND p.active
                           AND w.heartbeat_at > now() - interval '30 seconds'
                     THEN w.address END AS worker_address,
                CASE WHEN u.account_status = 'active' AND p.active
                           AND w.heartbeat_at > now() - interval '30 seconds'
                     THEN p.java_port END AS java_port,
                CASE WHEN u.account_status = 'active' AND p.active
                           AND w.heartbeat_at > now() - interval '30 seconds'
                     THEN p.bedrock_port END AS bedrock_port
         FROM servers s
         JOIN users u ON u.id = s.owner_id
         LEFT JOIN placements p ON p.server_id = s.id
         LEFT JOIN workers w ON w.id = p.worker_id
         WHERE s.subdomain = $1 AND s.desired_state <> 'deleted'",
    )
    .bind(subdomain)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| RouteTarget {
        server_id: row.server_id,
        subdomain: row.subdomain,
        state: row.state,
        account_suspended: row.account_suspended,
        worker_healthy: row.worker_healthy,
        worker_valid_until: row.worker_valid_until,
        worker_address: row.worker_address,
        java_port: row.java_port.and_then(|value| u16::try_from(value).ok()),
        bedrock_port: row.bedrock_port.and_then(|value| u16::try_from(value).ok()),
    }))
}

pub(super) async fn authenticated_user(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<CurrentUser, ApiError> {
    auth::authenticate(&state.pool, headers).await.map_err(|_| {
        ApiError::with_code(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "authentication required",
        )
    })
}

pub(super) async fn require_active_user(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<CurrentUser, ApiError> {
    let user = authenticated_user(state, headers).await?;
    if user.account_status != "active" {
        return Err(ApiError::with_code(
            StatusCode::FORBIDDEN,
            "account_suspended",
            "account is suspended",
        ));
    }
    Ok(user)
}

pub(super) async fn require_server_owner(
    state: &AppState,
    headers: &HeaderMap,
    server_id: Uuid,
) -> Result<ServerRecord, ApiError> {
    let user = require_active_user(state, headers).await?;
    server_by_id(&state.pool, server_id)
        .await?
        .filter(|server| server.owner_id == user.id)
        .ok_or_else(|| {
            ApiError::with_code(
                StatusCode::NOT_FOUND,
                "server_not_found",
                "server not found",
            )
        })
}

pub(super) fn require_internal(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let expected = format!("Bearer {}", state.config.internal_token);
    let actual = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if actual != Some(expected.as_str()) {
        return Err(ApiError::with_code(
            StatusCode::UNAUTHORIZED,
            "invalid_internal_token",
            "invalid internal token",
        ));
    }
    Ok(())
}

pub(super) async fn server_by_id(
    pool: &PgPool,
    server_id: Uuid,
) -> Result<Option<ServerRecord>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, owner_id, subdomain, is_default, desired_state, observed_state, settings, storage_bytes,
                generation, current_snapshot_generation, last_error, created_at, updated_at
         FROM servers WHERE id = $1",
    )
    .bind(server_id)
    .fetch_optional(pool)
    .await
}
