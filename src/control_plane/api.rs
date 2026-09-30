use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::time::sleep;
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};
use uuid::Uuid;

use crate::{
    config::ControlPlaneConfig,
    model::{ObservedState, RouteTarget, ServerRecord, validate_subdomain},
};

use super::{
    auth::{self, CurrentUser},
    lifecycle::Lifecycle,
};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub lifecycle: Lifecycle,
    pub config: ControlPlaneConfig,
    pub http: Client,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        tracing::warn!(error = ?error, "request failed");
        Self::new(StatusCode::BAD_REQUEST, error.to_string())
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(error = ?error, "database request failed");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "database request failed")
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    let origin: HeaderValue = state
        .config
        .panel_origin
        .parse()
        .expect("PANEL_ORIGIN must be a valid origin");
    Router::new()
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .route("/auth/login", get(auth::login))
        .route("/auth/callback", get(auth::callback))
        .route("/auth/logout", post(auth::logout))
        .route("/api/me", get(me))
        .route(
            "/api/server",
            get(get_server)
                .post(create_server)
                .delete(delete_server)
                .patch(update_server),
        )
        .route("/api/server/start", post(start_server))
        .route("/api/server/stop", post(stop_server))
        .route("/api/server/restart", post(restart_server))
        .route("/api/server/logs", get(logs))
        .route("/internal/resolve/{hostname}", get(resolve))
        .route("/internal/wake/{hostname}", post(wake))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::exact(origin))
                .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
                .allow_headers([header::CONTENT_TYPE])
                .allow_credentials(true),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub fn spawn_queue_drain(lifecycle: Lifecycle) {
    tokio::spawn(async move {
        loop {
            if let Err(error) = lifecycle.drain_queue_once().await {
                tracing::error!(?error, "failed to drain start queue");
            }
            sleep(Duration::from_secs(2)).await;
        }
    });
}

async fn me(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<CurrentUser>, ApiError> {
    Ok(Json(require_user(&state, &headers).await?))
}

#[derive(Debug, Serialize)]
struct ServerView {
    id: Uuid,
    subdomain: String,
    java_address: String,
    bedrock_address: String,
    desired_state: crate::model::DesiredState,
    observed_state: ObservedState,
    settings: Value,
    storage_bytes: i64,
    storage_limit_bytes: i64,
    memory_limit_bytes: i64,
    last_error: Option<String>,
}

impl ServerView {
    fn from_record(server: ServerRecord, root_domain: &str) -> Self {
        let address = format!("{}.{}", server.subdomain, root_domain);
        Self {
            id: server.id,
            subdomain: server.subdomain,
            java_address: address.clone(),
            bedrock_address: address,
            desired_state: server.desired_state,
            observed_state: server.observed_state,
            settings: server.settings,
            storage_bytes: server.storage_bytes,
            storage_limit_bytes: crate::model::SERVER_STORAGE_BYTES,
            memory_limit_bytes: crate::model::SERVER_MEMORY_BYTES,
            last_error: server.last_error,
        }
    }
}

async fn get_server(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Option<ServerView>>, ApiError> {
    let user = require_user(&state, &headers).await?;
    let server = owned_server(&state.pool, user.id).await?;
    Ok(Json(server.map(|server| {
        ServerView::from_record(server, &state.config.root_domain)
    })))
}

#[derive(Debug, Deserialize)]
struct CreateServer {
    subdomain: String,
}

async fn create_server(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(input): Json<CreateServer>,
) -> Result<(StatusCode, Json<ServerView>), ApiError> {
    let user = require_user(&state, &headers).await?;
    if !validate_subdomain(&input.subdomain) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid subdomain",
        ));
    }
    let result = sqlx::query_as::<_, ServerRecord>(
        "INSERT INTO servers(id, owner_id, subdomain)
         VALUES($1,$2,$3)
         RETURNING id, owner_id, subdomain, desired_state, observed_state, settings, storage_bytes,
                   generation, current_snapshot_generation, last_error, created_at, updated_at",
    )
    .bind(Uuid::new_v4())
    .bind(user.id)
    .bind(&input.subdomain)
    .fetch_one(&state.pool)
    .await;
    let server = match result {
        Ok(server) => server,
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "account already has a server or subdomain is taken",
            ));
        }
        Err(error) => return Err(error.into()),
    };
    Ok((
        StatusCode::CREATED,
        Json(ServerView::from_record(server, &state.config.root_domain)),
    ))
}

#[derive(Debug, Deserialize)]
struct UpdateServer {
    subdomain: Option<String>,
    settings: Option<ServerSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServerSettings {
    motd: Option<String>,
    difficulty: Option<String>,
    gamemode: Option<String>,
    view_distance: Option<u8>,
    simulation_distance: Option<u8>,
    hardcore: Option<bool>,
    whitelist: Option<bool>,
}

impl ServerSettings {
    fn validate(&self) -> Result<(), ApiError> {
        if self.motd.as_ref().is_some_and(|v| v.len() > 120) {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "motd is too long",
            ));
        }
        if self
            .difficulty
            .as_ref()
            .is_some_and(|v| !["Peaceful", "Easy", "Normal", "Hard"].contains(&v.as_str()))
        {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid difficulty",
            ));
        }
        if self.gamemode.as_ref().is_some_and(|v| {
            !["Survival", "Creative", "Adventure", "Spectator"].contains(&v.as_str())
        }) {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid gamemode",
            ));
        }
        if self.view_distance.is_some_and(|v| !(2..=32).contains(&v))
            || self
                .simulation_distance
                .is_some_and(|v| !(2..=32).contains(&v))
        {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "distance must be between 2 and 32",
            ));
        }
        Ok(())
    }
}

async fn update_server(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(input): Json<UpdateServer>,
) -> Result<Json<ServerView>, ApiError> {
    let user = require_user(&state, &headers).await?;
    let mut tx = state.pool.begin().await?;
    let current = sqlx::query_as::<_, ServerRecord>(
        "SELECT id, owner_id, subdomain, desired_state, observed_state, settings, storage_bytes,
                generation, current_snapshot_generation, last_error, created_at, updated_at
         FROM servers WHERE owner_id = $1 FOR UPDATE",
    )
    .bind(user.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "server not found"))?;
    if current.observed_state != ObservedState::Sleeping {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "server must be sleeping to change configuration",
        ));
    }
    let subdomain = input.subdomain.unwrap_or(current.subdomain);
    if !validate_subdomain(&subdomain) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid subdomain",
        ));
    }
    let settings = if let Some(settings) = input.settings {
        settings.validate()?;
        serde_json::to_value(settings).expect("settings serialize")
    } else {
        current.settings
    };
    let server = sqlx::query_as::<_, ServerRecord>(
        "UPDATE servers SET subdomain = $2, settings = $3, updated_at = now()
         WHERE id = $1
         RETURNING id, owner_id, subdomain, desired_state, observed_state, settings, storage_bytes,
                   generation, current_snapshot_generation, last_error, created_at, updated_at",
    )
    .bind(current.id)
    .bind(subdomain)
    .bind(settings)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(ServerView::from_record(
        server,
        &state.config.root_domain,
    )))
}

async fn delete_server(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let user = require_user(&state, &headers).await?;
    let server = owned_server(&state.pool, user.id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "server not found"))?;
    state.lifecycle.request_delete(server.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn start_server(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let id = require_server_id(&state, &headers).await?;
    Ok(Json(
        json!({ "state": state.lifecycle.request_start(id, "manual").await? }),
    ))
}

async fn stop_server(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let id = require_server_id(&state, &headers).await?;
    Ok(Json(
        json!({ "state": state.lifecycle.request_stop(id, "manual").await? }),
    ))
}

async fn restart_server(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let id = require_server_id(&state, &headers).await?;
    Ok(Json(
        json!({ "state": state.lifecycle.request_restart(id).await? }),
    ))
}

#[derive(Debug, Deserialize)]
struct LogQuery {
    limit: Option<i64>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct LogLine {
    id: i64,
    created_at: chrono::DateTime<chrono::Utc>,
    line: String,
}

async fn logs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LogQuery>,
) -> Result<Json<Vec<LogLine>>, ApiError> {
    let id = require_server_id(&state, &headers).await?;
    let mut lines = sqlx::query_as::<_, LogLine>(
        "SELECT id, created_at, line FROM server_logs WHERE server_id = $1 ORDER BY id DESC LIMIT $2",
    )
    .bind(id)
    .bind(query.limit.unwrap_or(200).clamp(1, 500))
    .fetch_all(&state.pool)
    .await?;
    lines.reverse();
    Ok(Json(lines))
}

async fn resolve(
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

async fn wake(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(hostname): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_internal(&state, &headers)?;
    let subdomain = subdomain_from_hostname(&hostname, &state.config.root_domain)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown hostname"))?;
    let server_id: Uuid = sqlx::query_scalar("SELECT id FROM servers WHERE subdomain = $1")
        .bind(subdomain)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown hostname"))?;
    Ok(Json(
        json!({ "state": state.lifecycle.request_start(server_id, "gateway_join").await? }),
    ))
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
        worker_valid_until: Option<chrono::DateTime<chrono::Utc>>,
        worker_address: Option<String>,
        java_port: Option<i32>,
        bedrock_port: Option<i32>,
    }
    let row = sqlx::query_as::<_, Row>(
        "SELECT s.id AS server_id, s.subdomain, s.observed_state AS state, u.account_status <> 'active' AS account_suspended,
                CASE WHEN p.server_id IS NULL THEN NULL
                     ELSE w.heartbeat_at > now() - interval '30 seconds'
                END AS worker_healthy,
                w.heartbeat_at + interval '30 seconds' AS worker_valid_until,
                CASE WHEN u.account_status = 'active' AND p.active AND w.heartbeat_at > now() - interval '30 seconds' THEN w.address END AS worker_address,
                CASE WHEN u.account_status = 'active' AND p.active AND w.heartbeat_at > now() - interval '30 seconds' THEN p.java_port END AS java_port,
                CASE WHEN u.account_status = 'active' AND p.active AND w.heartbeat_at > now() - interval '30 seconds' THEN p.bedrock_port END AS bedrock_port
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
        java_port: row.java_port.and_then(|v| u16::try_from(v).ok()),
        bedrock_port: row.bedrock_port.and_then(|v| u16::try_from(v).ok()),
    }))
}

fn subdomain_from_hostname<'a>(hostname: &'a str, root_domain: &str) -> Option<&'a str> {
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

async fn require_user(state: &AppState, headers: &HeaderMap) -> Result<CurrentUser, ApiError> {
    auth::authenticate(&state.pool, headers)
        .await
        .map_err(|_| ApiError::new(StatusCode::UNAUTHORIZED, "authentication required"))
}

async fn require_server_id(state: &AppState, headers: &HeaderMap) -> Result<Uuid, ApiError> {
    let user = require_user(state, headers).await?;
    owned_server(&state.pool, user.id)
        .await?
        .map(|server| server.id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "server not found"))
}

fn require_internal(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let expected = format!("Bearer {}", state.config.internal_token);
    let actual = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if actual != Some(expected.as_str()) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid internal token",
        ));
    }
    Ok(())
}

async fn owned_server(pool: &PgPool, owner_id: Uuid) -> Result<Option<ServerRecord>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, owner_id, subdomain, desired_state, observed_state, settings, storage_bytes,
                generation, current_snapshot_generation, last_error, created_at, updated_at
         FROM servers WHERE owner_id = $1",
    )
    .bind(owner_id)
    .fetch_optional(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::subdomain_from_hostname;

    #[test]
    fn extracts_only_one_valid_label() {
        assert_eq!(
            subdomain_from_hostname("abc.play.example.com", "play.example.com"),
            Some("abc")
        );
        assert_eq!(
            subdomain_from_hostname("abc.def.play.example.com", "play.example.com"),
            None
        );
        assert_eq!(
            subdomain_from_hostname("play.example.com", "play.example.com"),
            None
        );
    }
}
