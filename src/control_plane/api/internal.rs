use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::model::{ObservedState, RouteTarget, ServerRecord, validate_subdomain};

use super::auth;
use super::auth::CurrentUser;
use super::server::ServerActionResponse;
use super::{ApiError, AppState, lifecycle_error};

pub(super) async fn resolve(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(hostname): Path<String>,
) -> Result<Json<RouteTarget>, ApiError> {
    require_internal(&state, &headers)?;
    let subdomain = subdomain_from_hostname(&hostname, &state.config.root_domain)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown hostname"))?;
    let target = resolve_subdomain(&state.pool, subdomain)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown hostname"))?;
    Ok(Json(target))
}

pub(super) async fn wake(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(hostname): Path<String>,
) -> Result<Json<ServerActionResponse>, ApiError> {
    require_internal(&state, &headers)?;
    let subdomain = subdomain_from_hostname(&hostname, &state.config.root_domain)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown hostname"))?;
    let server_id: Uuid = sqlx::query_scalar("SELECT id FROM servers WHERE subdomain = $1")
        .bind(subdomain)
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

pub(super) fn subdomain_from_hostname<'a>(hostname: &'a str, root_domain: &str) -> Option<&'a str> {
    let hostname = hostname.trim_end_matches('.');
    let suffix_len = root_domain.trim_end_matches('.').len() + 1;
    if hostname.len() <= suffix_len
        || !hostname.to_ascii_lowercase().ends_with(&format!(
            ".{}",
            root_domain.trim_end_matches('.').to_ascii_lowercase()
        ))
    {
        return None;
    }
    let subdomain_len = hostname.len() - suffix_len;
    let subdomain = &hostname[..subdomain_len];
    (!subdomain.contains('.') && validate_subdomain(&subdomain.to_ascii_lowercase()))
        .then_some(subdomain)
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

pub(super) async fn require_owned_server(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<ServerRecord, ApiError> {
    let user = require_active_user(state, headers).await?;
    owned_server(&state.pool, user.id).await?.ok_or_else(|| {
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

pub(super) async fn owned_server(
    pool: &PgPool,
    owner_id: Uuid,
) -> Result<Option<ServerRecord>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, owner_id, subdomain, desired_state, observed_state, settings, storage_bytes,
                generation, current_snapshot_generation, last_error, created_at, updated_at
         FROM servers WHERE owner_id = $1
         ORDER BY is_default DESC, created_at, id LIMIT 1",
    )
    .bind(owner_id)
    .fetch_optional(pool)
    .await
}

pub(super) async fn server_by_id(
    pool: &PgPool,
    server_id: Uuid,
) -> Result<Option<ServerRecord>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, owner_id, subdomain, desired_state, observed_state, settings, storage_bytes,
                generation, current_snapshot_generation, last_error, created_at, updated_at
         FROM servers WHERE id = $1",
    )
    .bind(server_id)
    .fetch_optional(pool)
    .await
}
