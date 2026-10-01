use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use futures::future::try_join_all;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::model::{
    ObservedState, OperationRecord as DbOperation, SERVER_MEMORY_BYTES, SERVER_STORAGE_BYTES,
    ServerRecord, SnapshotRecord as DbSnapshot, validate_subdomain,
};

use super::internal::{require_active_user, require_server_owner};
use super::types::{
    LifecycleStatus, Operation, Problem, Server, ServerSettings, ServerSettingsPatch, Snapshot,
    StorageUsage,
};
use super::{ApiError, AppState, lifecycle_error, mutation_error};

#[derive(Debug, Serialize)]
pub struct ServerActionResponse {
    pub observed_state: ObservedState,
}

pub(super) async fn insert_server_record(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    subdomain: &str,
    settings: serde_json::Value,
) -> Result<ServerRecord, sqlx::Error> {
    sqlx::query_as::<_, ServerRecord>(
        "INSERT INTO servers(id, owner_id, subdomain, settings)
         VALUES($1,$2,$3,$4)
         RETURNING id, owner_id, subdomain, is_default, desired_state, observed_state, settings, storage_bytes,
                   generation, current_snapshot_generation, last_error, created_at, updated_at",
    )
    .bind(Uuid::new_v4())
    .bind(owner_id)
    .bind(subdomain)
    .bind(settings)
    .fetch_one(&mut **tx)
    .await
}

pub(super) async fn server_view(
    pool: &PgPool,
    server: ServerRecord,
    root_domain: &str,
) -> Result<Server, ApiError> {
    let operation_future = sqlx::query_as::<_, DbOperation>(
        "SELECT id, server_id, worker_id, kind, status, generation, attempt, payload, last_error,
                created_at, updated_at
         FROM operations
         WHERE server_id = $1 AND status IN ('pending', 'running')
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(server.id)
    .fetch_optional(pool);
    let snapshot_future = sqlx::query_as::<_, DbSnapshot>(
        "SELECT id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete,
                created_at
         FROM snapshots
         WHERE server_id = $1 AND complete
         ORDER BY generation DESC LIMIT 1",
    )
    .bind(server.id)
    .fetch_optional(pool);
    let player_count_future =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM server_players WHERE server_id = $1")
            .bind(server.id)
            .fetch_one(pool);
    let (operation, snapshot, player_count) =
        tokio::try_join!(operation_future, snapshot_future, player_count_future)?;

    let operation = operation.map(Operation::from);
    let address = format!("{}.{}", server.subdomain, root_domain);
    let lifecycle = LifecycleStatus {
        desired_state: server.desired_state,
        observed_state: server.observed_state,
        stage: operation
            .as_ref()
            .and_then(|operation| operation.stage.clone()),
    };
    let error = server.last_error.clone().map(|detail| Problem {
        code: "lifecycle_error",
        message: "The server needs attention".to_owned(),
        detail: Some(detail),
        retryable: server.observed_state == ObservedState::Failed,
    });

    Ok(Server {
        id: server.id,
        subdomain: server.subdomain,
        is_default: server.is_default,
        java_address: address.clone(),
        bedrock_address: address,
        settings: ServerSettings::from_stored(server.settings)?,
        lifecycle,
        current_operation: operation,
        latest_snapshot: snapshot.map(Snapshot::from),
        player_count,
        storage: StorageUsage {
            used_bytes: server.storage_bytes,
            limit_bytes: SERVER_STORAGE_BYTES,
        },
        memory_limit_bytes: SERVER_MEMORY_BYTES,
        generation: server.generation,
        current_snapshot_generation: server.current_snapshot_generation,
        error,
        created_at: server.created_at,
        updated_at: server.updated_at,
    })
}

pub(super) async fn get_server(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(server_id): Path<Uuid>,
) -> Result<Json<Option<Server>>, ApiError> {
    let server = require_server_owner(&state, &headers, server_id).await?;
    Ok(Json(Some(
        server_view(&state.pool, server, &state.config.root_domain).await?,
    )))
}

pub(super) async fn list_servers(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<Server>>, ApiError> {
    let user = require_active_user(&state, &headers).await?;
    let servers = sqlx::query_as::<_, ServerRecord>(
        "SELECT id, owner_id, subdomain, is_default, desired_state, observed_state, settings,
                storage_bytes, generation, current_snapshot_generation, last_error,
                created_at, updated_at
         FROM servers WHERE owner_id = $1 AND desired_state <> 'deleted'
         ORDER BY is_default DESC, created_at, id",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    let views = try_join_all(
        servers
            .into_iter()
            .map(|server| server_view(&state.pool, server, &state.config.root_domain)),
    )
    .await?;
    Ok(Json(views))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateServerRequest {
    subdomain: String,
    #[serde(default)]
    settings: ServerSettings,
}

pub(super) async fn create_server_for_user(
    state: &AppState,
    owner_id: Uuid,
    subdomain: &str,
    settings: ServerSettings,
) -> Result<ServerRecord, ApiError> {
    if !validate_subdomain(subdomain) {
        return Err(ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_subdomain",
            "invalid subdomain",
        ));
    }
    settings.validate()?;
    let mut tx = state.pool.begin().await?;
    let account_status: Option<String> =
        sqlx::query_scalar("SELECT account_status FROM users WHERE id = $1 FOR UPDATE")
            .bind(owner_id)
            .fetch_optional(&mut *tx)
            .await?;
    if account_status.as_deref() != Some("active") {
        return Err(ApiError::with_code(
            StatusCode::FORBIDDEN,
            "account_suspended",
            "account is suspended",
        ));
    }
    let has_server: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM servers WHERE owner_id = $1)")
            .bind(owner_id)
            .fetch_one(&mut *tx)
            .await?;
    if has_server {
        return Err(ApiError::with_code(
            StatusCode::CONFLICT,
            "account_has_server",
            "this account already has a hosted world",
        ));
    }
    let server = insert_server_record(&mut tx, owner_id, subdomain, settings.into_stored())
        .await
        .map_err(|error| {
            mutation_error(error, "account already has a server or subdomain is taken")
        })?;
    tx.commit().await?;
    Ok(server)
}

pub(super) async fn create_server(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(input): Json<CreateServerRequest>,
) -> Result<(StatusCode, Json<Server>), ApiError> {
    let user = require_active_user(&state, &headers).await?;
    let server = create_server_for_user(&state, user.id, &input.subdomain, input.settings).await?;
    let view = server_view(&state.pool, server, &state.config.root_domain).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateServerRequest {
    subdomain: Option<String>,
    settings: Option<ServerSettingsPatch>,
}

pub(super) async fn update_server(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(server_id): Path<Uuid>,
    Json(input): Json<UpdateServerRequest>,
) -> Result<Json<Server>, ApiError> {
    require_server_owner(&state, &headers, server_id).await?;
    let mut tx = state.pool.begin().await?;
    let current = sqlx::query_as::<_, ServerRecord>(
        "SELECT id, owner_id, subdomain, is_default, desired_state, observed_state, settings, storage_bytes,
                generation, current_snapshot_generation, last_error, created_at, updated_at
         FROM servers WHERE id = $1 AND desired_state <> 'deleted'
         FOR UPDATE",
    )
    .bind(server_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        ApiError::with_code(
            StatusCode::NOT_FOUND,
            "server_not_found",
            "server not found",
        )
    })?;
    if current.observed_state != ObservedState::Sleeping {
        return Err(ApiError::with_code(
            StatusCode::CONFLICT,
            "server_not_sleeping",
            "server must be sleeping to change configuration",
        ));
    }

    let subdomain = input.subdomain.unwrap_or_else(|| current.subdomain.clone());
    if !validate_subdomain(&subdomain) {
        return Err(ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_subdomain",
            "invalid subdomain",
        ));
    }

    let mut settings = ServerSettings::from_stored(current.settings.clone())?;
    if let Some(patch) = input.settings {
        settings.merge(patch);
    }
    settings.validate()?;

    let server = sqlx::query_as::<_, ServerRecord>(
        "UPDATE servers SET subdomain = $2, settings = $3, updated_at = now()
         WHERE id = $1
         RETURNING id, owner_id, subdomain, is_default, desired_state, observed_state, settings, storage_bytes,
                   generation, current_snapshot_generation, last_error, created_at, updated_at",
    )
    .bind(current.id)
    .bind(subdomain)
    .bind(settings.into_stored())
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| mutation_error(error, "subdomain is taken"))?;
    tx.commit().await?;
    Ok(Json(
        server_view(&state.pool, server, &state.config.root_domain).await?,
    ))
}

pub(super) async fn delete_server(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(server_id): Path<Uuid>,
) -> Result<(StatusCode, Json<ServerActionResponse>), ApiError> {
    let server = require_server_owner(&state, &headers, server_id).await?;
    let observed_state = state
        .lifecycle
        .request_delete(server.id)
        .await
        .map_err(lifecycle_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(ServerActionResponse { observed_state }),
    ))
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ServerAction {
    Start,
    Stop,
    Restart,
    Snapshot,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ServerActionRequest {
    action: ServerAction,
}

pub(super) async fn server_action(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(server_id): Path<Uuid>,
    Json(input): Json<ServerActionRequest>,
) -> Result<Json<ServerActionResponse>, ApiError> {
    let server = require_server_owner(&state, &headers, server_id).await?;
    let observed_state = match input.action {
        ServerAction::Start => state
            .lifecycle
            .request_start(server.id, "manual")
            .await
            .map_err(lifecycle_error)?,
        ServerAction::Stop => state
            .lifecycle
            .request_stop(server.id, "manual")
            .await
            .map_err(lifecycle_error)?,
        ServerAction::Restart => state
            .lifecycle
            .request_restart(server.id)
            .await
            .map_err(lifecycle_error)?,
        ServerAction::Snapshot => {
            if server.observed_state != ObservedState::Online {
                return Err(ApiError::with_code(
                    StatusCode::CONFLICT,
                    "server_not_online",
                    "manual snapshots require an online server",
                ));
            }
            state
                .lifecycle
                .request_snapshot(server.id)
                .await
                .map_err(lifecycle_error)?
        }
    };
    Ok(Json(ServerActionResponse { observed_state }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ResetServerRequest {
    mode: String,
}

pub(super) async fn reset_server(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(server_id): Path<Uuid>,
    Json(input): Json<ResetServerRequest>,
) -> Result<(StatusCode, Json<ServerActionResponse>), ApiError> {
    let server = require_server_owner(&state, &headers, server_id).await?;
    if !matches!(input.mode.as_str(), "world" | "full") {
        return Err(ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_reset_mode",
            "reset mode must be world or full",
        ));
    }
    let defaults = ServerSettings::default().into_stored();
    let observed_state = state
        .lifecycle
        .request_reset(server.id, &input.mode, defaults)
        .await
        .map_err(lifecycle_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(ServerActionResponse { observed_state }),
    ))
}
