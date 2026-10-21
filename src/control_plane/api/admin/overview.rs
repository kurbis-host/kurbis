use std::sync::Arc;

use axum::{Json, extract::State};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use crate::control_plane::supervision::WorkerState;
use crate::fleet_stats::{self, FleetSample, FleetUsage};

use super::super::{ApiError, ApiQuery, AppState};
use super::{AdminUser, RangeQuery, WORKER_LIVE_SECONDS};

/// A server in a passing state (queued, restoring, starting, stopping, snapshotting,
/// resetting) unchanged for longer than this is stuck.
pub(super) const STUCK_MINUTES: i32 = 10;

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum AdminTaskState {
    Starting,
    Running,
    BackingOff,
    Fatal,
    Stopped,
}

impl From<WorkerState> for AdminTaskState {
    fn from(state: WorkerState) -> Self {
        match state {
            WorkerState::Starting => Self::Starting,
            WorkerState::Running => Self::Running,
            WorkerState::BackingOff => Self::BackingOff,
            WorkerState::Fatal => Self::Fatal,
            WorkerState::Stopped => Self::Stopped,
        }
    }
}

/// One of the control plane's background tasks (start queue, maintenance, CI poller…).
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AdminTask {
    name: String,
    state: AdminTaskState,
    /// The control plane is unhealthy while a required task is not running.
    required: bool,
    last_heartbeat: Option<DateTime<Utc>>,
    last_success: Option<DateTime<Utc>>,
    last_error: Option<String>,
    consecutive_failures: u32,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminWorkerCounts {
    total: i64,
    /// Heartbeat within the last 30 seconds.
    live: i64,
    draining: i64,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminStartQueue {
    /// Servers waiting for a worker with room.
    depth: i64,
    oldest_requested_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminStateCount {
    state: String,
    count: i64,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminServerCounts {
    /// Servers not deleted.
    total: i64,
    failed: i64,
    /// In a passing state for more than ten minutes.
    stuck: i64,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminOperationCounts {
    running: i64,
    /// Pending again after a failed attempt.
    retrying: i64,
    failed_last_day: i64,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminAccountCounts {
    total: i64,
    suspended: i64,
    signups_last_day: i64,
    signups_last_week: i64,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminTableSize {
    name: String,
    /// Rows by the planner's estimate.
    rows: i64,
    /// Table, indexes and TOAST.
    bytes: i64,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminServerStorage {
    server_id: uuid::Uuid,
    subdomain: String,
    storage_bytes: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AdminStorage {
    /// Complete snapshots and their total size.
    snapshots: i64,
    snapshot_bytes: i64,
    /// What servers' worlds take, as they last reported.
    server_bytes: i64,
    database_bytes: i64,
    largest_tables: Vec<AdminTableSize>,
    largest_servers: Vec<AdminServerStorage>,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminGitHubRateLimit {
    scope: String,
    remaining: Option<i64>,
    reset_at: Option<DateTime<Utc>>,
    /// Requests wait until then after GitHub said to slow down.
    paused_until: Option<DateTime<Utc>>,
    consecutive_failures: i32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AdminOverview {
    measured_at: DateTime<Utc>,
    /// The tasks of the control plane that answered.
    tasks: Vec<AdminTask>,
    workers: AdminWorkerCounts,
    start_queue: AdminStartQueue,
    servers: AdminServerCounts,
    servers_by_state: Vec<AdminStateCount>,
    operations: AdminOperationCounts,
    fleet: FleetUsage,
    storage: AdminStorage,
    accounts: AdminAccountCounts,
    github_rate_limits: Vec<AdminGitHubRateLimit>,
    ai_cost_this_month_micros: i64,
}

async fn storage(state: &AppState) -> Result<AdminStorage, ApiError> {
    let (snapshots, snapshot_bytes, server_bytes, database_bytes): (i64, i64, i64, i64) =
        sqlx::query_as(
            "SELECT (SELECT count(*) FROM snapshots WHERE complete),
                    (SELECT coalesce(sum(size_bytes), 0)::bigint FROM snapshots WHERE complete),
                    (SELECT coalesce(sum(storage_bytes), 0)::bigint FROM servers
                     WHERE desired_state <> 'deleted'),
                    pg_database_size(current_database())",
        )
        .fetch_one(&state.pool)
        .await?;
    let largest_tables = sqlx::query_as::<_, AdminTableSize>(
        "SELECT relname::text AS name, greatest(reltuples, 0)::bigint AS rows,
                pg_total_relation_size(oid) AS bytes
         FROM pg_class
         WHERE relkind = 'r' AND relnamespace = 'public'::regnamespace
         ORDER BY bytes DESC LIMIT 8",
    )
    .fetch_all(&state.pool)
    .await?;
    let largest_servers = sqlx::query_as::<_, AdminServerStorage>(
        "SELECT id AS server_id, subdomain, storage_bytes FROM servers
         WHERE desired_state <> 'deleted'
         ORDER BY storage_bytes DESC LIMIT 5",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(AdminStorage {
        snapshots,
        snapshot_bytes,
        server_bytes,
        database_bytes,
        largest_tables,
        largest_servers,
    })
}

pub(super) async fn overview(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> Result<Json<AdminOverview>, ApiError> {
    let pool = &state.pool;
    let workers = sqlx::query_as::<_, AdminWorkerCounts>(
        "SELECT count(*) AS total,
                count(*) FILTER (WHERE heartbeat_at > now() - make_interval(secs => $1)) AS live,
                count(*) FILTER (WHERE draining) AS draining
         FROM workers",
    )
    .bind(WORKER_LIVE_SECONDS)
    .fetch_one(pool);
    let start_queue = sqlx::query_as::<_, AdminStartQueue>(
        "SELECT count(*) AS depth, min(requested_at) AS oldest_requested_at FROM start_requests",
    )
    .fetch_one(pool);
    let servers = sqlx::query_as::<_, AdminServerCounts>(
        "SELECT count(*) AS total,
                count(*) FILTER (WHERE observed_state = 'failed') AS failed,
                count(*) FILTER (WHERE observed_state IN ('queued', 'restoring', 'starting', 'stopping',
                                                          'snapshotting', 'resetting')
                                   AND updated_at < now() - make_interval(mins => $1)) AS stuck
         FROM servers WHERE desired_state <> 'deleted'",
    )
    .bind(STUCK_MINUTES)
    .fetch_one(pool);
    let servers_by_state = sqlx::query_as::<_, AdminStateCount>(
        "SELECT observed_state::text AS state, count(*) AS count
         FROM servers WHERE desired_state <> 'deleted'
         GROUP BY observed_state ORDER BY observed_state",
    )
    .fetch_all(pool);
    let operations = sqlx::query_as::<_, AdminOperationCounts>(
        "SELECT (SELECT count(*) FROM operations WHERE status = 'running') AS running,
                (SELECT count(*) FROM operations WHERE status = 'pending' AND attempt > 0) AS retrying,
                (SELECT count(*) FROM operations
                 WHERE status = 'failed' AND updated_at > now() - interval '1 day') AS failed_last_day",
    )
    .fetch_one(pool);
    let accounts = sqlx::query_as::<_, AdminAccountCounts>(
        "SELECT count(*) AS total,
                count(*) FILTER (WHERE account_status = 'suspended') AS suspended,
                count(*) FILTER (WHERE created_at > now() - interval '1 day') AS signups_last_day,
                count(*) FILTER (WHERE created_at > now() - interval '7 days') AS signups_last_week
         FROM users",
    )
    .fetch_one(pool);
    let rate_limits = sqlx::query_as::<_, AdminGitHubRateLimit>(
        "SELECT scope, remaining, reset_at, paused_until, consecutive_failures
         FROM ci_github_rate_limits ORDER BY scope",
    )
    .fetch_all(pool);
    let ai_cost = sqlx::query_scalar::<_, i64>(
        "SELECT coalesce(sum(cost_micros), 0)::bigint FROM ai_usage
         WHERE created_at >= date_trunc('month', now())",
    )
    .fetch_one(pool);
    let measured_at = sqlx::query_scalar::<_, DateTime<Utc>>("SELECT now()").fetch_one(pool);
    let (
        workers,
        start_queue,
        servers,
        servers_by_state,
        operations,
        accounts,
        rate_limits,
        ai_cost,
        measured_at,
        fleet,
    ) = tokio::try_join!(
        workers,
        start_queue,
        servers,
        servers_by_state,
        operations,
        accounts,
        rate_limits,
        ai_cost,
        measured_at,
        fleet_stats::usage(pool),
    )?;
    let tasks = state
        .supervision
        .snapshot()
        .await
        .workers
        .into_iter()
        .map(|task| AdminTask {
            name: task.name,
            state: task.state.into(),
            required: task.required,
            last_heartbeat: task.last_heartbeat,
            last_success: task.last_success,
            last_error: task.last_error,
            consecutive_failures: task.consecutive_failures,
        })
        .collect();
    Ok(Json(AdminOverview {
        measured_at,
        tasks,
        workers,
        start_queue,
        servers,
        servers_by_state,
        operations,
        fleet,
        storage: storage(&state).await?,
        accounts,
        github_rate_limits: rate_limits,
        ai_cost_this_month_micros: ai_cost,
    }))
}

pub(super) async fn fleet(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ApiQuery(query): ApiQuery<RangeQuery>,
) -> Result<Json<Vec<FleetSample>>, ApiError> {
    Ok(Json(
        fleet_stats::history(
            &state.pool,
            query.range.minutes(),
            query.range.bucket_minutes(),
        )
        .await?,
    ))
}
