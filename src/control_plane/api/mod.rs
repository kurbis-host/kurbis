use std::{sync::Arc, time::Duration};

use aide::{
    axum::{
        ApiRouter,
        routing::{
            delete_with as api_delete_with, get as api_get, get_with as api_get_with,
            post as api_post, post_with as api_post_with, put_with as api_put_with,
        },
    },
    openapi::{ApiKeyLocation, Info, OpenApi, SecurityScheme},
};
use axum::{
    Json, Router,
    http::{HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use object_store::ObjectStore;
use reqwest::Client;
use schemars::JsonSchema;
use serde::Serialize;
use sqlx::PgPool;
use tokio::time::sleep;
use tower_http::{set_header::SetResponseHeaderLayer, trace::TraceLayer};

use crate::config::ControlPlaneConfig;

#[cfg(test)]
use super::lifecycle::LifecycleFailure;
use super::{
    auth,
    lifecycle::{Lifecycle, LifecycleCommandError},
};

mod account;
mod activity;
mod actor;
mod commands;
mod events;
mod exports;
mod internal;
mod logs;
mod openapi;
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
mod worker_snapshots;
mod worker_state;

#[cfg(test)]
mod tests;

pub use crate::contracts::lifecycle::{CommandResponse, ServerCommandRequest};
pub use activity::{ActivityEntry, ServerEventKind};
pub use actor::{
    ApiAccountHeaders, ApiAccountOrServerGrantHeaders, ApiJson, ApiPath, ApiQuery,
    ApiServerCapabilityHeaders, ApiServerGrantHeaders, ApiServerHeaders, OAuthProviderPath,
    ServerScopedSessionPath,
};
pub use events::{ServerEvent, ServerEventPayload};
pub use logs::{LogEntry, LogPage};
pub use players::{PlayerEdition, PlayerPresence};
pub use session::{AccountStatus, ServiceLimits, Session, SessionUser};
pub use types::{
    Difficulty, GameMode, LifecycleStatus, Operation, Problem, Server, ServerSettings, Snapshot,
    StorageUsage,
};
pub use whitelist::{Whitelist, WhitelistEntry};

pub fn openapi_document() -> aide::openapi::OpenApi {
    let mut document = api_document_template();
    let _router = api_router().finish_api_with(&mut document, add_security_schemes);
    document
}

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

#[derive(Debug, Serialize, JsonSchema)]
pub struct ApiErrorResponse {
    pub code: &'static str,
    pub message: String,
}

impl aide::operation::OperationOutput for ApiError {
    type Inner = ApiErrorResponse;

    fn operation_response(
        context: &mut aide::generate::GenContext,
        operation: &mut aide::openapi::Operation,
    ) -> Option<aide::openapi::Response> {
        <Json<ApiErrorResponse> as aide::operation::OperationOutput>::operation_response(
            context, operation,
        )
    }

    fn inferred_responses(
        context: &mut aide::generate::GenContext,
        operation: &mut aide::openapi::Operation,
    ) -> Vec<(Option<u16>, aide::openapi::Response)> {
        let Some(response) = Self::operation_response(context, operation) else {
            return Vec::new();
        };
        [400, 401, 403, 404, 409, 415, 422, 500, 503]
            .into_iter()
            .map(|status| (Some(status), response.clone()))
            .collect()
    }
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
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "request could not be completed",
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

fn lifecycle_error(error: impl Into<LifecycleCommandError>) -> ApiError {
    match error.into() {
        LifecycleCommandError::NotFound => ApiError::with_code(
            StatusCode::NOT_FOUND,
            "lifecycle_resource_not_found",
            "the requested server or lifecycle resource was not found",
        ),
        LifecycleCommandError::Forbidden => ApiError::with_code(
            StatusCode::FORBIDDEN,
            "lifecycle_command_forbidden",
            "the authenticated actor cannot issue this command",
        ),
        LifecycleCommandError::Invalid => ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_lifecycle_command",
            "the lifecycle command is invalid",
        ),
        LifecycleCommandError::Conflict => ApiError::with_code(
            StatusCode::CONFLICT,
            "invalid_lifecycle_state",
            "the command conflicts with the server's current state",
        ),
        LifecycleCommandError::Unavailable => ApiError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "lifecycle_capacity_unavailable",
            "the lifecycle service is temporarily unavailable",
        ),
        LifecycleCommandError::Internal => ApiError::with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            "lifecycle_service_error",
            "the lifecycle service could not complete the request",
        ),
    }
}

fn api_document_template() -> OpenApi {
    OpenApi {
        info: Info {
            title: "Kurbis Control Plane API".to_owned(),
            version: "1.0.0".to_owned(),
            description: Some(
                "Browser JSON API. OAuth callbacks are redirects, server events use SSE, and export downloads stream ZIP data. Worker and gateway protocol routes are intentionally private.".to_owned(),
            ),
            ..Info::default()
        },
        ..OpenApi::default()
    }
}

fn add_security_schemes(
    api: aide::transform::TransformOpenApi<'_>,
) -> aide::transform::TransformOpenApi<'_> {
    api.security_scheme(
        "accountSessionCookie",
        SecurityScheme::ApiKey {
            location: ApiKeyLocation::Cookie,
            name: "kurbis_session".to_owned(),
            description: Some("Account browser session.".to_owned()),
            extensions: Default::default(),
        },
    )
    .security_scheme(
        "serverGrantCookie",
        SecurityScheme::ApiKey {
            location: ApiKeyLocation::Cookie,
            name: "kurbis_server_session".to_owned(),
            description: Some("Persistent server-scoped grant collection. Revoke individual server rows through the scoped session endpoint.".to_owned()),
            extensions: Default::default(),
        },
    )
    .security_scheme(
        "serverCapabilityBearer",
        SecurityScheme::Http {
            scheme: "bearer".to_owned(),
            bearer_format: Some("opaque server capability".to_owned()),
            description: Some("Placement-scoped plugin capability limited to one server.".to_owned()),
            extensions: Default::default(),
        },
    )
    .security_scheme(
        "gatewayBearer",
        SecurityScheme::Http {
            scheme: "bearer".to_owned(),
            bearer_format: Some("internal gateway credential".to_owned()),
            description: Some("Gateway actor may issue only gateway_join starts.".to_owned()),
            extensions: Default::default(),
        },
    )
    .security_scheme(
        "workerBearer",
        SecurityScheme::Http {
            scheme: "bearer".to_owned(),
            bearer_format: Some("worker API token".to_owned()),
            description: Some("Unique credential for one worker.".to_owned()),
            extensions: Default::default(),
        },
    )
    .security_scheme(
        "workerLease",
        SecurityScheme::ApiKey {
            location: ApiKeyLocation::Header,
            name: "x-worker-lease".to_owned(),
            description: Some("Active process lease; required with workerBearer for worker commands.".to_owned()),
            extensions: Default::default(),
        },
    )
}

fn api_router() -> ApiRouter<Arc<AppState>> {
    ApiRouter::new()
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .api_route(
            "/auth/{provider}/login",
            api_get_with(auth::login, |operation| {
                operation
                    .description("Begin an OAuth login redirect.")
                    .response::<307, ()>()
            }),
        )
        .api_route(
            "/auth/{provider}/link",
            api_get_with(auth::link_oauth, |operation| {
                operation
                    .description("Begin a provider-link redirect for the authenticated account.")
                    .response::<307, ()>()
            }),
        )
        .api_route(
            "/auth/{provider}/callback",
            api_get_with(auth::callback, |operation| {
                operation
                    .description("Complete OAuth and redirect with an account session cookie.")
                    .response::<303, ()>()
            }),
        )
        .api_route(
            "/auth/minecraft/redeem",
            api_post_with(auth::redeem_minecraft, |operation| {
                operation
                    .description("Redeem a one-use Minecraft login token; account session cookies are set when signing in.")
                    .security_requirement("accountSessionCookie")
                    .security_requirement_multi([] as [&str; 0])
                    .response::<204, ()>()
            }),
        )
        .api_route(
            "/auth/server/redeem",
            api_post_with(server_access::redeem_server_login, |operation| {
                operation.description("Redeem a one-use server login token and set a persistent, server-scoped browser grant cookie.")
            }),
        )
        .api_route(
            "/auth/logout",
            api_post_with(auth::logout, |operation| operation.response::<204, ()>()),
        )
        .api_route("/api/v1/session", api_get(session::session))
        .api_route("/api/v1/account", api_get(account::account))
        .api_route(
            "/api/v1/account/worlds/{server_id}/default",
            api_put_with(account::make_default_world, |operation| {
                operation.response::<204, ()>()
            }),
        )
        .api_route(
            "/api/v1/account/sessions/{session_id}",
            api_delete_with(account::revoke_session, |operation| {
                operation.response::<204, ()>()
            }),
        )
        .api_route(
            "/api/v1/server/sessions",
            api_get(server_access::list_server_sessions),
        )
        .api_route(
            "/api/v1/server/sessions/{session_id}",
            api_delete_with(server_access::revoke_server_session, |operation| {
                operation.response::<204, ()>()
            }),
        )
        .api_route(
            "/api/v1/servers",
            api_get(server::list_servers).post_with(
                server::create_server,
                |operation| operation.response::<201, Json<Server>>(),
            ),
        )
        .api_route(
            "/api/v1/server/login-links",
            api_post(server_access::mint_server_login_link),
        )
        .api_route(
            "/api/v1/servers/{server_id}",
            api_get(server::get_server).patch(server::update_server),
        )
        .api_route(
            commands::COMMAND_ROUTE,
            api_post_with(commands::dispatch_command, |operation| {
                operation
                    .id("dispatchServerCommand")
                    .description("Dispatch one authorized lifecycle command for the server in the path.")
                    .security_requirement("accountSessionCookie")
                    .security_requirement("serverGrantCookie")
                    .security_requirement("serverCapabilityBearer")
                    .security_requirement("gatewayBearer")
                    .security_requirement_multi(["workerBearer", "workerLease"])
                    .response::<202, Json<CommandResponse>>()
            }),
        )
        .api_route(
            "/api/v1/servers/{server_id}/events",
            api_get_with(events::server_events, |operation| {
                operation
                    .id("streamServerEvents")
                    .description("Reconnecting server event stream with durable event IDs.")
                    .response_with::<200, Json<ServerEvent>, _>(|mut response| {
                        let response = {
                            let inner = response.inner();
                            if let Some(json) = inner.content.shift_remove("application/json") {
                                inner.content.insert("text/event-stream".to_owned(), json);
                            }
                            response
                        };
                        response.description("ServerEvent JSON payloads framed as server-sent events.")
                    })
            }),
        )
        .api_route("/api/v1/servers/{server_id}/logs", api_get(logs::logs))
        .api_route(
            "/api/v1/servers/{server_id}/activity",
            api_get(activity::activity),
        )
        .api_route(
            "/api/v1/servers/{server_id}/snapshots",
            api_get(snapshots::snapshots),
        )
        .api_route(
            "/api/v1/servers/{server_id}/exports",
            api_get(exports::list_exports).post_with(
                exports::create_export,
                |operation| operation.response::<202, Json<exports::WorldExport>>(),
            ),
        )
        .api_route(
            "/api/v1/servers/{server_id}/exports/{export_id}",
            api_get(exports::get_export).delete(exports::cancel_export),
        )
        .api_route(
            "/api/v1/servers/{server_id}/exports/{export_id}/download",
            api_get_with(exports::download_export, |operation| {
                operation
                    .description("Stream a ready world archive as ZIP bytes.")
                    .response_with::<200, Vec<u8>, _>(|mut response| {
                        let response = {
                            let inner = response.inner();
                            let body = inner
                                .content
                                .shift_remove("application/octet-stream")
                                .or_else(|| inner.content.shift_remove("application/json"));
                            if let Some(body) = body {
                                inner.content.insert("application/zip".to_owned(), body);
                            }
                            response
                        };
                        response.description("Raw ZIP stream; the JSON client does not buffer this response.")
                    })
            }),
        )
        .api_route(
            "/api/v1/servers/{server_id}/players",
            api_get(players::players),
        )
        .api_route(
            "/api/v1/servers/{server_id}/whitelist",
            api_get(whitelist::whitelist)
                .post_with(whitelist::add_whitelist_entry, |operation| {
                    operation.response::<201, Json<Whitelist>>()
                })
                .delete(whitelist::remove_whitelist_entry),
        )
        .route(
            "/internal/bedrock-signal/{server_id}",
            get(internal::bedrock_signal_target),
        )
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
        .route("/internal/worker/control/lease", post(worker_state::acquire_lease))
        .route("/internal/worker/control/servers", get(worker_state::servers))
        .route("/internal/worker/control/retained-servers", get(worker_state::retained_servers))
        .route("/internal/worker/control/servers/{server_id}/context", get(worker_state::server_context))
        .route("/internal/worker/control/servers/{server_id}/players", post(worker_state::sync_players))
        .route("/internal/worker/control/servers/{server_id}/logs", post(worker_state::append_logs))
        .route("/internal/worker/control/servers/{server_id}/whitelist-applied", post(worker_state::mark_whitelist_applied))
        .route("/internal/worker/control/servers/{server_id}/operators-applied", post(worker_state::mark_operators_applied))
        .route("/internal/worker/control/servers/{server_id}/snapshots/reserve", post(worker_snapshots::reserve_snapshot))
        .route("/internal/worker/control/servers/{server_id}/snapshots/cache-hint", post(worker_snapshots::report_snapshot_cache_hint))
        .route("/internal/worker/control/servers/{server_id}/snapshots/current", get(worker_snapshots::current_snapshot))
        .route("/internal/worker/control/servers/{server_id}/snapshots/{snapshot_generation}/metadata", post(worker_snapshots::publish_snapshot_metadata))
        .route("/internal/worker/control/servers/{server_id}/snapshots/{snapshot_generation}", get(worker_snapshots::snapshot))
        .route("/internal/worker/control/servers/{server_id}/snapshots/delete-manifests", post(worker_snapshots::delete_manifests))
        .route("/internal/worker/control/exports/{export_id}/running", get(worker_snapshots::export_running))
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
}

pub fn router(state: Arc<AppState>) -> Router {
    let mut document = api_document_template();
    api_router()
        .finish_api_with(&mut document, add_security_schemes)
        .route("/openapi.json", get(openapi::serve))
        .layer(axum::Extension(Arc::new(document)))
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
