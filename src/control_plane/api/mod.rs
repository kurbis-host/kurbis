use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    http::{HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use reqwest::Client;
use serde::Serialize;
use sqlx::PgPool;
use tokio::time::sleep;
use tower_http::{set_header::SetResponseHeaderLayer, trace::TraceLayer};

use crate::config::ControlPlaneConfig;

use super::{auth, lifecycle::Lifecycle};

mod account;
mod activity;
mod events;
mod internal;
mod logs;
mod players;
mod server;
mod session;
mod snapshots;
mod types;
mod whitelist;

#[cfg(test)]
mod tests;

pub use activity::{ActivityEntry, ServerEventKind};
pub use events::{ServerEvent, ServerEventPayload};
pub use internal::resolve_subdomain;
pub use logs::{LogEntry, LogPage};
pub use players::{PlayerEdition, PlayerPresence};
pub use server::ServerActionResponse;
pub use session::{AccountStatus, ServiceLimits, Session, SessionUser};
pub use types::{
    Difficulty, GameMode, LifecycleStatus, Operation, Problem, Server, ServerSettings, Snapshot,
    StorageUsage,
};
pub use whitelist::{Whitelist, WhitelistEntry};

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
    code: &'static str,
    message: String,
}

#[derive(Debug, Serialize)]
struct ApiErrorResponse {
    code: &'static str,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self::with_code(status, "request_failed", message)
    }

    pub(super) fn with_code(
        status: StatusCode,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ApiErrorResponse {
                code: self.code,
                message: self.message,
            }),
        )
            .into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        tracing::warn!(error = ?error, "request failed");
        Self::with_code(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            error.to_string(),
        )
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(error = ?error, "database request failed");
        Self::with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database_error",
            "database request failed",
        )
    }
}

fn mutation_error(error: sqlx::Error, conflict: &str) -> ApiError {
    if matches!(&error, sqlx::Error::Database(db) if db.is_unique_violation()) {
        ApiError::with_code(StatusCode::CONFLICT, "conflict", conflict)
    } else {
        error.into()
    }
}

fn lifecycle_error(error: anyhow::Error) -> ApiError {
    tracing::warn!(error = ?error, "lifecycle request rejected");
    ApiError::with_code(
        StatusCode::CONFLICT,
        "invalid_lifecycle_state",
        error.to_string(),
    )
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .route("/auth/{provider}/login", get(auth::login))
        .route("/auth/{provider}/link", get(auth::link_oauth))
        .route("/auth/{provider}/callback", get(auth::callback))
        .route("/auth/minecraft/redeem", post(auth::redeem_minecraft))
        .route("/auth/logout", post(auth::logout))
        .route("/api/v1/session", get(session::session))
        .route("/api/v1/account", get(account::account))
        .route(
            "/api/v1/account/sessions/{session_id}",
            axum::routing::delete(account::revoke_session),
        )
        .route(
            "/api/v1/server",
            get(server::get_server)
                .post(server::create_server)
                .patch(server::update_server)
                .delete(server::delete_server),
        )
        .route("/api/v1/server/actions", post(server::server_action))
        .route("/api/v1/server/events", get(events::server_events))
        .route("/api/v1/server/logs", get(logs::logs))
        .route("/api/v1/server/activity", get(activity::activity))
        .route("/api/v1/server/snapshots", get(snapshots::snapshots))
        .route(
            "/api/v1/server/snapshots/{generation}/restore",
            post(snapshots::restore_snapshot),
        )
        .route("/api/v1/server/players", get(players::players))
        .route(
            "/api/v1/server/whitelist",
            get(whitelist::whitelist)
                .post(whitelist::add_whitelist_entry)
                .delete(whitelist::remove_whitelist_entry),
        )
        .route("/internal/resolve/{hostname}", get(internal::resolve))
        .route("/internal/wake/{hostname}", post(internal::wake))
        .route(
            "/internal/minecraft-login-links",
            post(internal::mint_minecraft_link),
        )
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<_>| {
                tracing::info_span!(
                    "http_request",
                    method = %request.method(),
                    path = %request.uri().path(),
                    version = ?request.version(),
                )
            }),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::renew_session_cookie,
        ))
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
