//! The operator dashboard's read-only API under `/api/v1/admin`.
//!
//! Only accounts with one of `ADMIN_IDENTITIES` linked may read it; to everyone else,
//! signed in or not, these routes do not exist. Admins may also read (never change)
//! any server through the ordinary server routes; see `internal::require_server_view`.

use std::sync::Arc;

use aide::axum::{ApiRouter, routing::get_with};
use axum::http::{HeaderMap, StatusCode, request::Parts};
use axum::{Json, extract::FromRequestParts};
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use super::{ApiError, AppState, internal::require_active_user};

mod accounts;
mod ai;
mod ci;
mod failures;
mod growth;
mod overview;
mod servers;
mod workers;

/// A worker whose last heartbeat is older than this takes no new servers.
pub(super) const WORKER_LIVE_SECONDS: f64 = 30.0;

/// Proof that the request comes from an active account with an admin identity linked.
#[derive(Debug, Clone, Copy)]
pub(super) struct AdminUser;

fn not_found() -> ApiError {
    ApiError::with_code(StatusCode::NOT_FOUND, "not_found", "not found")
}

/// Whether the account has one of the configured admin identities linked.
pub(super) async fn is_admin(state: &AppState, user_id: Uuid) -> Result<bool, sqlx::Error> {
    let identities = &state.config.admin_identities;
    if identities.is_empty() {
        return Ok(false);
    }
    let providers: Vec<&str> = identities
        .iter()
        .map(|identity| identity.provider.as_str())
        .collect();
    let subjects: Vec<&str> = identities
        .iter()
        .map(|identity| identity.subject.as_str())
        .collect();
    sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1 FROM identities
             JOIN unnest($2::text[], $3::text[]) AS admin(provider, subject)
               ON admin.provider = identities.provider AND admin.subject = identities.subject
             WHERE identities.user_id = $1)",
    )
    .bind(user_id)
    .bind(providers)
    .bind(subjects)
    .fetch_one(&state.pool)
    .await
}

/// The request's account when it is an active admin.
pub(super) async fn admin_user(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<AdminUser>, ApiError> {
    let Ok(user) = require_active_user(state, headers).await else {
        return Ok(None);
    };
    Ok(is_admin(state, user.id).await?.then_some(AdminUser))
}

impl FromRequestParts<Arc<AppState>> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        admin_user(state, &parts.headers)
            .await?
            .ok_or_else(not_found)
    }
}

impl aide::operation::OperationInput for AdminUser {
    fn operation_input(
        _context: &mut aide::generate::GenContext,
        operation: &mut aide::openapi::Operation,
    ) {
        operation.security.push(
            [("accountSessionCookie".to_owned(), Vec::<String>::new())]
                .into_iter()
                .collect(),
        );
    }
}

/// How far back a chart looks.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum AdminRange {
    Hour,
    #[default]
    Day,
    Week,
    Month,
}

impl AdminRange {
    pub(super) fn minutes(self) -> i32 {
        match self {
            Self::Hour => 60,
            Self::Day => 24 * 60,
            Self::Week => 7 * 24 * 60,
            Self::Month => 30 * 24 * 60,
        }
    }

    /// Minutes per point, so a chart has a few hundred points at most.
    pub(super) fn bucket_minutes(self) -> i32 {
        match self {
            Self::Hour => 1,
            Self::Day => 5,
            Self::Week => 30,
            Self::Month => 120,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct RangeQuery {
    #[serde(default)]
    pub range: AdminRange,
}

/// A page of a list: `limit` rows after the first `offset`, matching `search`.
pub(super) struct Page {
    /// A `LIKE` pattern for the search, or null to match everything.
    pub pattern: Option<String>,
    /// The search as it was typed, to match ids exactly.
    pub search: String,
    pub limit: i64,
    pub offset: i64,
}

impl Page {
    pub(super) fn new(search: Option<&str>, limit: Option<i64>, offset: Option<i64>) -> Self {
        let search = search.unwrap_or_default().trim().to_owned();
        let pattern = (!search.is_empty()).then(|| {
            let escaped = search
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            format!("%{escaped}%")
        });
        Self {
            pattern,
            search,
            limit: limit.unwrap_or(50).clamp(1, 200),
            offset: offset.unwrap_or(0).max(0),
        }
    }
}

fn describe<T: JsonSchema>(
    description: &'static str,
) -> impl FnOnce(aide::transform::TransformOperation) -> aide::transform::TransformOperation {
    move |operation| {
        operation
            .description(description)
            .response::<200, Json<T>>()
    }
}

pub(super) fn router() -> ApiRouter<Arc<AppState>> {
    ApiRouter::new()
        .api_route(
            "/api/v1/admin/overview",
            get_with(
                overview::overview,
                describe::<overview::AdminOverview>(
                    "Admin only: whether everything is fine. Background tasks, workers, the start queue, servers by state, failing operations, the fleet now, storage and accounts.",
                ),
            ),
        )
        .api_route(
            "/api/v1/admin/fleet",
            get_with(
                overview::fleet,
                describe::<Vec<crate::fleet_stats::FleetSample>>(
                    "Admin only: the whole service over the range, oldest first.",
                ),
            ),
        )
        .api_route(
            "/api/v1/admin/servers",
            get_with(
                servers::servers,
                describe::<servers::AdminServerPage>(
                    "Admin only: every server, newest change first, filtered by state and search.",
                ),
            ),
        )
        .api_route(
            "/api/v1/admin/servers/{server_id}",
            get_with(
                servers::server,
                describe::<servers::AdminServerDetail>(
                    "Admin only: a server's lifecycle: its owner, placement, queue entry, operations, snapshots, plugin errors and samples over the range.",
                ),
            ),
        )
        .api_route(
            "/api/v1/admin/accounts",
            get_with(
                accounts::accounts,
                describe::<accounts::AdminAccountPage>(
                    "Admin only: every account, newest first, filtered by search.",
                ),
            ),
        )
        .api_route(
            "/api/v1/admin/accounts/{account_id}",
            get_with(
                accounts::account,
                describe::<accounts::AdminAccountDetail>(
                    "Admin only: an account's identities, sessions, servers and AI spend.",
                ),
            ),
        )
        .api_route(
            "/api/v1/admin/workers",
            get_with(
                workers::workers,
                describe::<Vec<workers::AdminWorker>>(
                    "Admin only: every worker with its capacity, heartbeat, lease and what runs on it.",
                ),
            ),
        )
        .api_route(
            "/api/v1/admin/failures",
            get_with(
                failures::failures,
                describe::<failures::AdminFailures>(
                    "Admin only: everything that failed over the range, grouped by what it said, and the latest failures.",
                ),
            ),
        )
        .api_route(
            "/api/v1/admin/ci",
            get_with(
                ci::ci,
                describe::<ci::AdminCi>(
                    "Admin only: Pumpkin preview builds: the watched repository, GitHub's rate limit, the build queue, builder slots, recent builds and running previews.",
                ),
            ),
        )
        .api_route(
            "/api/v1/admin/ci/builds/{build_id}/log",
            get_with(
                ci::build_log,
                describe::<ci::AdminBuildLog>("Admin only: a preview build's log."),
            ),
        )
        .api_route(
            "/api/v1/admin/ai",
            get_with(
                ai::ai,
                describe::<ai::AdminAi>(
                    "Admin only: the AI assistant's spend this month against its budgets, by day, model, server and account, and its failed or stalled requests.",
                ),
            ),
        )
        .api_route(
            "/api/v1/admin/growth",
            get_with(
                growth::growth,
                describe::<growth::AdminGrowth>(
                    "Admin only: every day's signups, servers, wakes, peaks and AI spend, and the share of each signup week that came back.",
                ),
            ),
        )
}
