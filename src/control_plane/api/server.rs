use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use futures::future::try_join_all;
use garde::Validate;
use schemars::JsonSchema;
use serde::Deserialize;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::model::{
    ObservedState, OperationRecord as DbOperation, SERVER_MEMORY_BYTES, SERVER_STORAGE_BYTES,
    ServerRecord, SnapshotRecord as DbSnapshot, validate_subdomain,
};

use super::actor::{
    ApiAccountHeaders, ApiAccountOrServerGrantHeaders, ApiJson, ApiPath, ApiServerHeaders,
    ServerIdPath,
};
use super::internal::{require_active_user, require_server_access};
use super::types::{
    LifecycleStatus, Operation, Problem, Server, ServerSettings, ServerSettingsPatch, Snapshot,
    StorageUsage,
};
use super::{ApiError, AppState, mutation_error};

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
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
) -> Result<Json<Option<Server>>, ApiError> {
    let server_id = path.server_id;
    let server = require_server_access(&state, &headers, server_id).await?;
    Ok(Json(Some(
        server_view(&state.pool, server, &state.config.root_domain).await?,
    )))
}

pub(super) async fn list_servers(
    State(state): State<Arc<AppState>>,
    headers: ApiAccountOrServerGrantHeaders,
) -> Result<Json<Vec<Server>>, ApiError> {
    let account_result = require_active_user(&state, &headers).await;
    let scoped_server_ids =
        super::server_access::server_session_server_ids(&state.pool, &headers).await?;
    let owner_id = match account_result {
        Ok(user) => Some(user.id),
        Err(error) if scoped_server_ids.is_empty() => return Err(error),
        Err(_) => None,
    };
    let servers = sqlx::query_as::<_, ServerRecord>(
        "SELECT id, owner_id, subdomain, is_default, desired_state, observed_state, settings,
                storage_bytes, generation, current_snapshot_generation, last_error,
                created_at, updated_at
         FROM servers
         WHERE desired_state <> 'deleted'
           AND (($1::uuid IS NOT NULL AND owner_id = $1) OR id = ANY($2))
         ORDER BY (is_default AND owner_id = $1) DESC, created_at, id",
    )
    .bind(owner_id)
    .bind(&scoped_server_ids)
    .fetch_all(&state.pool)
    .await?;
    let views = try_join_all(servers.into_iter().map(|mut server| {
        if Some(server.owner_id) != owner_id {
            server.is_default = false;
        }
        server_view(&state.pool, server, &state.config.root_domain)
    }))
    .await?;
    Ok(Json(views))
}

#[derive(Debug, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub(super) struct CreateServerRequest {
    #[garde(length(min = 3, max = 32))]
    subdomain: String,
    #[garde(skip)]
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
    headers: ApiAccountHeaders,
    ApiJson(input): ApiJson<CreateServerRequest>,
) -> Result<(StatusCode, Json<Server>), ApiError> {
    let user = require_active_user(&state, &headers).await?;
    let server = create_server_for_user(&state, user.id, &input.subdomain, input.settings).await?;
    let view = server_view(&state.pool, server, &state.config.root_domain).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

#[derive(Debug, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub(super) struct UpdateServerRequest {
    #[garde(skip)]
    subdomain: Option<String>,
    #[garde(skip)]
    settings: Option<ServerSettingsPatch>,
}

pub(super) async fn update_server(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
    ApiJson(input): ApiJson<UpdateServerRequest>,
) -> Result<Json<Server>, ApiError> {
    let server_id = path.server_id;
    require_server_access(&state, &headers, server_id).await?;
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
