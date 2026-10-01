use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    http::{HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{any, get, post, put},
};
use object_store::ObjectStore;
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
mod exports;
mod internal;
mod logs;
mod players;
mod server;
mod server_access;
mod session;
mod snapshots;
mod types;
mod whitelist;
mod worker_auth;
mod worker_control;
mod worker_objects;

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
    pub object_store: Arc<dyn ObjectStore>,
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
        .route(
            "/auth/server/redeem",
            post(server_access::redeem_server_login),
        )
        .route("/auth/logout", post(auth::logout))
        .route("/api/v1/session", get(session::session))
        .route("/api/v1/account", get(account::account))
        .route(
            "/api/v1/account/worlds/{server_id}/default",
            put(account::make_default_world),
        )
        .route(
            "/api/v1/account/sessions/{session_id}",
            axum::routing::delete(account::revoke_session),
        )
        .route(
            "/api/v1/servers",
            get(server::list_servers).post(server::create_server),
        )
        .route(
            "/api/v1/server/login-links",
            post(server_access::mint_server_login_link),
        )
        .route(
            "/api/v1/servers/{server_id}",
            get(server::get_server)
                .patch(server::update_server)
                .delete(server::delete_server),
        )
        .route(
            "/api/v1/servers/{server_id}/actions",
            post(server::server_action),
        )
        .route(
            "/api/v1/servers/{server_id}/reset",
            post(server::reset_server),
        )
        .route(
            "/api/v1/servers/{server_id}/events",
            get(events::server_events),
        )
        .route("/api/v1/servers/{server_id}/logs", get(logs::logs))
        .route(
            "/api/v1/servers/{server_id}/activity",
            get(activity::activity),
        )
        .route(
            "/api/v1/servers/{server_id}/snapshots",
            get(snapshots::snapshots),
        )
        .route(
            "/api/v1/servers/{server_id}/snapshots/{generation}/restore",
            post(snapshots::restore_snapshot),
        )
        .route(
            "/api/v1/servers/{server_id}/exports",
            get(exports::list_exports).post(exports::create_export),
        )
        .route(
            "/api/v1/servers/{server_id}/exports/{export_id}",
            get(exports::get_export).delete(exports::cancel_export),
        )
        .route(
            "/api/v1/servers/{server_id}/exports/{export_id}/download",
            get(exports::download_export),
        )
        .route("/api/v1/servers/{server_id}/players", get(players::players))
        .route(
            "/api/v1/servers/{server_id}/whitelist",
            get(whitelist::whitelist)
                .post(whitelist::add_whitelist_entry)
                .delete(whitelist::remove_whitelist_entry),
        )
        .route("/internal/resolve/{hostname}", get(internal::resolve))
        .route(
            "/internal/bedrock-signal/{server_id}",
            get(internal::bedrock_signal_target),
        )
        .route("/internal/wake/{hostname}", post(internal::wake))
        .route("/internal/admission", post(internal::admit))
        .route(
            "/internal/minecraft-login-links",
            post(internal::mint_minecraft_link),
        )
        .route(
            "/internal/worker/server-capabilities",
            post(server_access::ensure_server_capability),
        )
        .route(
            "/internal/worker/servers/{server_id}/objects/{*key}",
            any(worker_objects::object),
        )
        .route(
            "/internal/worker/control/recover",
            post(worker_control::recover),
        )
        .route(
            "/internal/worker/control/heartbeat",
            post(worker_control::heartbeat),
        )
        .route(
            "/internal/worker/control/operations/claim",
            post(worker_control::claim_operation),
        )
        .route(
            "/internal/worker/control/operations/{operation_id}/progress",
            post(worker_control::operation_progress),
        )
        .route(
            "/internal/worker/control/operations/{operation_id}/retry",
            post(worker_control::retry_operation),
        )
        .route(
            "/internal/worker/control/operations/{operation_id}/begin-snapshot-retry",
            post(worker_control::begin_snapshot_retry),
        )
        .route(
            "/internal/worker/control/operations/{operation_id}/defer-snapshot",
            post(worker_control::defer_snapshot),
        )
        .route(
            "/internal/worker/control/operations/{operation_id}/complete",
            post(worker_control::complete_operation),
        )
        .route(
            "/internal/worker/control/operations/{operation_id}/validate-delete",
            post(worker_control::validate_delete_operation),
        )
        .route(
            "/internal/worker/control/operations/{operation_id}/deactivate-placement",
            post(worker_control::deactivate_placement),
        )
        .route(
            "/internal/worker/control/runtime-lost",
            post(worker_control::runtime_lost),
        )
        .route(
            "/internal/worker/control/servers/{server_id}/stop",
            post(worker_control::request_stop),
        )
        .route(
            "/internal/worker/control/servers/{server_id}/snapshot",
            post(worker_control::request_snapshot),
        )
        .route(
            "/internal/worker/control/servers/{server_id}/snapshots/publish",
            post(worker_control::publish_snapshot),
        )
        .route(
            "/internal/worker/control/exports/claim",
            post(worker_control::claim_export),
        )
        .route(
            "/internal/worker/control/exports/{export_id}/complete",
            post(worker_control::complete_export),
        )
        .route(
            "/internal/worker/control/exports/{export_id}/fail",
            post(worker_control::fail_export),
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
