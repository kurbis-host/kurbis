use std::sync::Arc;

use axum::{Json, extract::State};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::super::{ApiError, ApiPath, ApiQuery, AppState, actor::ServerIdPath};
use super::overview::STUCK_MINUTES;
use super::{AdminRange, AdminUser, Page};

/// Which servers a list shows.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum AdminServerFilter {
    #[default]
    All,
    Online,
    Sleeping,
    Failed,
    Queued,
    /// In a passing state for more than ten minutes.
    Stuck,
    /// Never woken successfully.
    NeverStarted,
}

impl AdminServerFilter {
    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Online => "online",
            Self::Sleeping => "sleeping",
            Self::Failed => "failed",
            Self::Queued => "queued",
            Self::Stuck => "stuck",
            Self::NeverStarted => "never_started",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct ServerListQuery {
    /// Matches addresses and owner names, or a server or owner id exactly.
    search: Option<String>,
    #[serde(default)]
    filter: AdminServerFilter,
    limit: Option<i64>,
    offset: Option<i64>,
}

/// A server as the dashboard lists it.
#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminServer {
    id: Uuid,
    subdomain: String,
    owner_id: Uuid,
    owner_name: String,
    owner_status: String,
    desired_state: String,
    observed_state: String,
    worker_id: Option<Uuid>,
    worker_name: Option<String>,
    players_online: i64,
    storage_bytes: i64,
    /// When the latest successful wake was asked for.
    last_woken_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AdminServerPage {
    /// Servers matching the filter and search, across every page.
    total: i64,
    items: Vec<AdminServer>,
}

/// Every column of `AdminServer`, for `FROM servers` joined as in [`SERVER_JOINS`].
pub(super) const SERVER_COLUMNS: &str = "servers.id, servers.subdomain, servers.owner_id,
       users.display_name AS owner_name, users.account_status AS owner_status,
       servers.desired_state::text AS desired_state, servers.observed_state::text AS observed_state,
       placements.worker_id, workers.name AS worker_name,
       (SELECT count(*) FROM server_players WHERE server_players.server_id = servers.id) AS players_online,
       servers.storage_bytes,
       (SELECT max(created_at) FROM operations
        WHERE operations.server_id = servers.id AND kind = 'start' AND status = 'succeeded') AS last_woken_at,
       servers.last_error, servers.created_at, servers.updated_at";

pub(super) const SERVER_JOINS: &str = "JOIN users ON users.id = servers.owner_id
       LEFT JOIN placements ON placements.server_id = servers.id AND placements.active
       LEFT JOIN workers ON workers.id = placements.worker_id";

pub(super) async fn servers(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ApiQuery(query): ApiQuery<ServerListQuery>,
) -> Result<Json<AdminServerPage>, ApiError> {
    let filter = "WHERE servers.desired_state <> 'deleted'
           AND ($1::text IS NULL OR servers.subdomain ILIKE $1 OR users.display_name ILIKE $1
                OR servers.id::text = btrim($4) OR servers.owner_id::text = btrim($4))
           AND CASE $2::text
                 WHEN 'online' THEN servers.observed_state = 'online'
                 WHEN 'sleeping' THEN servers.observed_state = 'sleeping'
                 WHEN 'failed' THEN servers.observed_state = 'failed'
                 WHEN 'queued' THEN servers.observed_state = 'queued'
                 WHEN 'stuck' THEN servers.observed_state IN ('queued', 'restoring', 'starting',
                                                               'stopping', 'snapshotting', 'resetting')
                                   AND servers.updated_at < now() - make_interval(mins => $3)
                 WHEN 'never_started' THEN NOT EXISTS(
                     SELECT 1 FROM operations WHERE operations.server_id = servers.id
                       AND kind = 'start' AND status = 'succeeded')
                 ELSE true
               END";
    let page = Page::new(query.search.as_deref(), query.limit, query.offset);
    let total = sqlx::query_scalar::<_, i64>(&format!(
        "SELECT count(*) FROM servers {SERVER_JOINS} {filter}"
    ))
    .bind(&page.pattern)
    .bind(query.filter.as_str())
    .bind(STUCK_MINUTES)
    .bind(&page.search)
    .fetch_one(&state.pool)
    .await?;
    let items = sqlx::query_as::<_, AdminServer>(&format!(
        "SELECT {SERVER_COLUMNS} FROM servers {SERVER_JOINS} {filter}
         ORDER BY servers.updated_at DESC, servers.id LIMIT $5 OFFSET $6"
    ))
    .bind(&page.pattern)
    .bind(query.filter.as_str())
    .bind(STUCK_MINUTES)
    .bind(&page.search)
    .bind(page.limit)
    .bind(page.offset)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(AdminServerPage { total, items }))
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminPlacement {
    worker_id: Uuid,
    worker_name: String,
    generation: i64,
    active: bool,
    java_port: Option<i32>,
    bedrock_port: Option<i32>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminStartRequest {
    reason: String,
    requested_at: DateTime<Utc>,
    /// Servers ahead of this one in the queue.
    ahead: i64,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminOperation {
    id: Uuid,
    kind: String,
    status: String,
    generation: i64,
    attempt: i32,
    worker_name: Option<String>,
    last_error: Option<String>,
    created_at: DateTime<Utc>,
    claimed_at: Option<DateTime<Utc>>,
    next_attempt_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminSnapshot {
    generation: i64,
    reason: String,
    size_bytes: i64,
    complete: bool,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminPluginError {
    name: String,
    version: String,
    enabled: bool,
    runtime_error: String,
    reported_at: Option<DateTime<Utc>>,
}

/// A server over one bucket of minutes: its most players, and its average usage.
#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminServerSample {
    minute: DateTime<Utc>,
    players_online: i32,
    cpu_used_millis: Option<i64>,
    memory_used_bytes: Option<i64>,
    tps: Option<f64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AdminServerDetail {
    server: AdminServer,
    placement: Option<AdminPlacement>,
    start_request: Option<AdminStartRequest>,
    /// The latest 50, newest first.
    operations: Vec<AdminOperation>,
    /// Every snapshot still kept, newest first, complete or not.
    snapshots: Vec<AdminSnapshot>,
    plugin_errors: Vec<AdminPluginError>,
    /// Over the range, oldest first; minutes the server was not running are missing.
    samples: Vec<AdminServerSample>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct ServerDetailQuery {
    #[serde(default)]
    range: AdminRange,
}

pub(super) async fn server(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ApiPath(path): ApiPath<ServerIdPath>,
    ApiQuery(query): ApiQuery<ServerDetailQuery>,
) -> Result<Json<AdminServerDetail>, ApiError> {
    let pool = &state.pool;
    let server = sqlx::query_as::<_, AdminServer>(&format!(
        "SELECT {SERVER_COLUMNS} FROM servers {SERVER_JOINS} WHERE servers.id = $1"
    ))
    .bind(path.server_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(ApiError::server_not_found)?;
    let placement = sqlx::query_as::<_, AdminPlacement>(
        "SELECT placements.worker_id, workers.name AS worker_name, placements.generation,
                placements.active, placements.java_port, placements.bedrock_port, placements.updated_at
         FROM placements JOIN workers ON workers.id = placements.worker_id
         WHERE placements.server_id = $1",
    )
    .bind(server.id)
    .fetch_optional(pool);
    let start_request = sqlx::query_as::<_, AdminStartRequest>(
        "SELECT reason, requested_at,
                (SELECT count(*) FROM start_requests ahead
                 WHERE ahead.requested_at < start_requests.requested_at) AS ahead
         FROM start_requests WHERE server_id = $1",
    )
    .bind(server.id)
    .fetch_optional(pool);
    let operations = sqlx::query_as::<_, AdminOperation>(
        "SELECT operations.id, operations.kind::text AS kind, operations.status::text AS status,
                operations.generation, operations.attempt, workers.name AS worker_name,
                operations.last_error, operations.created_at, operations.claimed_at,
                operations.next_attempt_at, operations.updated_at
         FROM operations LEFT JOIN workers ON workers.id = operations.worker_id
         WHERE operations.server_id = $1
         ORDER BY operations.created_at DESC LIMIT 50",
    )
    .bind(server.id)
    .fetch_all(pool);
    let snapshots = sqlx::query_as::<_, AdminSnapshot>(
        "SELECT generation, reason::text AS reason, size_bytes, complete, created_at
         FROM snapshots WHERE server_id = $1 ORDER BY generation DESC",
    )
    .bind(server.id)
    .fetch_all(pool);
    let plugin_errors = sqlx::query_as::<_, AdminPluginError>(
        "SELECT name, version, enabled, runtime_error, reported_at
         FROM server_plugins WHERE server_id = $1 AND runtime_error IS NOT NULL
         ORDER BY name",
    )
    .bind(server.id)
    .fetch_all(pool);
    let samples = sqlx::query_as::<_, AdminServerSample>(
        "SELECT date_bin(make_interval(mins => $3), minute, timestamptz 'epoch') AS minute,
                max(players_online) AS players_online,
                round(avg(cpu_used_millis))::bigint AS cpu_used_millis,
                round(avg(memory_used_bytes))::bigint AS memory_used_bytes,
                avg(tps) AS tps
         FROM server_runtime_samples
         WHERE server_id = $1 AND minute > now() - make_interval(mins => $2)
         GROUP BY 1 ORDER BY 1",
    )
    .bind(server.id)
    .bind(query.range.minutes())
    .bind(query.range.bucket_minutes())
    .fetch_all(pool);
    let (placement, start_request, operations, snapshots, plugin_errors, samples) = tokio::try_join!(
        placement,
        start_request,
        operations,
        snapshots,
        plugin_errors,
        samples
    )?;
    Ok(Json(AdminServerDetail {
        server,
        placement,
        start_request,
        operations,
        snapshots,
        plugin_errors,
        samples,
    }))
}
