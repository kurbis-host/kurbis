use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    control_plane::identity,
    model::ServerRecord,
    routing::{AdmissionRequest, AdmissionResponse, BedrockSignalTarget, RouteRepository},
};

use super::auth;
use super::auth::CurrentUser;
use super::types::ServerSettings;
use super::{ApiError, AppState};

pub(super) async fn bedrock_signal_target(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(server_id): Path<Uuid>,
) -> Result<Json<BedrockSignalTarget>, ApiError> {
    require_internal(&state, &headers)?;
    let target = sqlx::query_as::<_, (Uuid, String, i32, String, i32, i64)>(
        "SELECT p.server_id, w.address, p.bedrock_port,
                w.signal_address, p.signal_port, p.generation
         FROM placements p
         JOIN servers s ON s.id = p.server_id
         JOIN workers w ON w.id = p.worker_id
         JOIN users u ON u.id = s.owner_id
         WHERE p.server_id = $1 AND p.active
           AND p.generation = s.generation
           AND s.desired_state = 'running' AND s.observed_state = 'online'
           AND u.account_status = 'active'
           AND w.signal_address <> ''
           AND w.heartbeat_at > now() - interval '30 seconds'",
    )
    .bind(server_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| {
        ApiError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "signal_target_unavailable",
            "server signaling target is unavailable",
        )
    })?;
    if !crate::model::is_private_signal_address(&target.3) {
        return Err(ApiError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "signal_target_unavailable",
            "server signaling target is unavailable",
        ));
    }
    let bedrock_port = u16::try_from(target.2).map_err(|_| {
        ApiError::with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_signal_target",
            "server public target is invalid",
        )
    })?;
    let signal_port = u16::try_from(target.4).map_err(|_| {
        ApiError::with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_signal_target",
            "server signaling target is invalid",
        )
    })?;
    Ok(Json(BedrockSignalTarget {
        server_id: target.0,
        worker_address: target.1,
        bedrock_port,
        signal_address: target.3,
        signal_port,
        generation: target.5,
    }))
}

pub(super) async fn admit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AdmissionRequest>,
) -> Result<Json<AdmissionResponse>, ApiError> {
    require_internal(&state, &headers)?;
    let routes = RouteRepository::new(state.pool.clone(), state.config.root_domain.clone());
    let subdomain = routes
        .subdomain_for_hostname(&request.hostname)
        .ok_or_else(|| {
            ApiError::with_code(
                StatusCode::NOT_FOUND,
                "invalid_hostname",
                "unknown server address",
            )
        })?;
    if let Some(target) = routes.resolve_subdomain(&subdomain).await? {
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

    let existing_address: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM servers WHERE subdomain = $1 AND desired_state <> 'deleted')",
    )
    .bind(&subdomain)
    .fetch_one(&mut *tx)
    .await?;
    if existing_address {
        tx.commit().await?;
        let target = routes.resolve_subdomain(&subdomain).await?.ok_or_else(|| {
            ApiError::with_code(
                StatusCode::SERVICE_UNAVAILABLE,
                "route_unavailable",
                "server route is temporarily unavailable",
            )
        })?;
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

    let default_subdomain: Option<String> = sqlx::query_scalar(
        "SELECT subdomain FROM servers WHERE owner_id = $1 AND desired_state <> 'deleted'
         ORDER BY is_default DESC, created_at, id LIMIT 1",
    )
    .bind(owner_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(default_subdomain) = default_subdomain {
        tx.commit().await?;
        let target = routes
            .resolve_subdomain(&default_subdomain)
            .await?
            .ok_or_else(|| {
                ApiError::with_code(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "route_unavailable",
                    "default server route is temporarily unavailable",
                )
            })?;
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
    match insert {
        Ok(_) => {}
        Err(error) => {
            tx.rollback().await?;
            if let Some(target) = routes.resolve_subdomain(&subdomain).await? {
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
    }
    tx.commit().await?;

    let target = routes.resolve_subdomain(&subdomain).await?.ok_or_else(|| {
        ApiError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "route_unavailable",
            "server route is unavailable",
        )
    })?;
    Ok(Json(AdmissionResponse {
        target,
        private_message: None,
        created: true,
    }))
}

pub(super) async fn mint_minecraft_link(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: Json<auth::MinecraftLinkRequest>,
) -> Result<Json<auth::MinecraftLinkResponse>, ApiError> {
    require_internal(&state, &headers)?;
    auth::create_minecraft_link(State(state), request).await
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

pub(super) async fn require_server_access(
    state: &AppState,
    headers: &HeaderMap,
    server_id: Uuid,
) -> Result<ServerRecord, ApiError> {
    let owner_error = match require_active_user(state, headers).await {
        Ok(user) => {
            if let Some(server) = server_by_id(&state.pool, server_id)
                .await?
                .filter(|server| server.owner_id == user.id)
            {
                return Ok(server);
            }
            ApiError::with_code(
                StatusCode::NOT_FOUND,
                "server_not_found",
                "server not found",
            )
        }
        Err(error) => error,
    };

    let server_session_ids =
        super::server_access::server_session_server_ids(&state.pool, headers).await?;
    if server_session_ids.contains(&server_id) {
        return server_by_id(&state.pool, server_id).await?.ok_or_else(|| {
            ApiError::with_code(
                StatusCode::NOT_FOUND,
                "server_not_found",
                "server not found",
            )
        });
    }

    let capability_server_id =
        super::server_access::instance_capability_server_id(&state.pool, headers).await?;
    if capability_server_id == Some(server_id) {
        return server_by_id(&state.pool, server_id).await?.ok_or_else(|| {
            ApiError::with_code(
                StatusCode::NOT_FOUND,
                "server_not_found",
                "server not found",
            )
        });
    }

    if !server_session_ids.is_empty() || capability_server_id.is_some() {
        return Err(ApiError::with_code(
            StatusCode::NOT_FOUND,
            "server_not_found",
            "server not found",
        ));
    }

    Err(owner_error)
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
