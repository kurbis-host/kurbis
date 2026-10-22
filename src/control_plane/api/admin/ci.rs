use std::sync::Arc;

use axum::{Json, extract::State};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::super::{ApiError, ApiPath, AppState};
use super::AdminUser;
use super::overview::{AdminGitHubRateLimit, AdminStateCount};

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminCiRepository {
    repository: String,
    default_branch: String,
    enabled: bool,
    last_successful_poll: Option<DateTime<Utc>>,
    /// A poller holds the repository until then.
    poll_lease_expires_at: Option<DateTime<Utc>>,
}

/// A builder slot in use.
#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminBuilderSlot {
    builder_identity: String,
    slot: i16,
    requested_memory_bytes: i64,
    heartbeat_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
}

/// Where a build is published: a preview hostname and the ref it follows.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub(super) struct AdminBuildAlias {
    hostname: String,
    kind: String,
    /// Empty for a commit preview, which follows no ref.
    ref_name: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminBuild {
    id: Uuid,
    commit_sha: String,
    stage: String,
    failure_stage: Option<String>,
    failure_class: Option<String>,
    failure_message: Option<String>,
    retry_count: i32,
    retry_at: Option<DateTime<Utc>>,
    retention_policy: String,
    requested_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    has_log: bool,
    #[sqlx(json)]
    aliases: Vec<AdminBuildAlias>,
    /// Pull requests whose queued build this is.
    pull_requests: Vec<i64>,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminPreview {
    id: Uuid,
    hostname: String,
    kind: String,
    /// Empty for a commit preview, which follows no ref.
    ref_name: Option<String>,
    commit_sha: String,
    state: String,
    expires_at: DateTime<Utc>,
    runtime_state: Option<String>,
    worker_name: Option<String>,
    players_online: i64,
    last_activity_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminWebhookDeliveries {
    pending: i64,
    failing: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AdminCi {
    repositories: Vec<AdminCiRepository>,
    github_rate_limits: Vec<AdminGitHubRateLimit>,
    /// Pull-request build requests: live ones, and those finished in the last week.
    queue: Vec<AdminStateCount>,
    /// Builds by stage: unfinished ones, and those finished in the last week.
    builds_by_stage: Vec<AdminStateCount>,
    builder_slots: Vec<AdminBuilderSlot>,
    /// The latest 50 builds to change.
    builds: Vec<AdminBuild>,
    /// Previews not yet expired, latest change first.
    previews: Vec<AdminPreview>,
    webhook_deliveries: AdminWebhookDeliveries,
}

pub(super) async fn ci(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> Result<Json<AdminCi>, ApiError> {
    let pool = &state.pool;
    let repositories = sqlx::query_as::<_, AdminCiRepository>(
        "SELECT repository, default_branch, enabled, last_successful_poll, poll_lease_expires_at
         FROM ci_watched_repositories ORDER BY repository",
    )
    .fetch_all(pool);
    let rate_limits = sqlx::query_as::<_, AdminGitHubRateLimit>(
        "SELECT scope, remaining, reset_at, paused_until, consecutive_failures
         FROM ci_github_rate_limits ORDER BY scope",
    )
    .fetch_all(pool);
    let queue = sqlx::query_as::<_, AdminStateCount>(
        "SELECT state, count(*) AS count FROM ci_build_queue
         WHERE state IN ('pending', 'running', 'failed') OR finished_at > now() - interval '7 days'
         GROUP BY state ORDER BY state",
    )
    .fetch_all(pool);
    let builds_by_stage = sqlx::query_as::<_, AdminStateCount>(
        "SELECT stage AS state, count(*) AS count FROM ci_builds
         WHERE stage NOT IN ('ready', 'failed', 'expired') OR updated_at > now() - interval '7 days'
         GROUP BY stage ORDER BY stage",
    )
    .fetch_all(pool);
    let builder_slots = sqlx::query_as::<_, AdminBuilderSlot>(
        "SELECT builder_identity, slot, requested_memory_bytes, heartbeat_at, created_at
         FROM ci_build_admission_leases ORDER BY builder_identity, slot",
    )
    .fetch_all(pool);
    let builds = sqlx::query_as::<_, AdminBuild>(
        "SELECT ci_builds.id, ci_builds.commit_sha, ci_builds.stage, ci_builds.failure_stage,
                ci_builds.failure_class, ci_builds.failure_message, ci_builds.retry_count,
                ci_builds.retry_at, ci_builds.retention_policy, ci_builds.requested_at,
                ci_builds.created_at, ci_builds.updated_at,
                ci_builds.build_log IS NOT NULL AND ci_builds.build_log <> '' AS has_log,
                (SELECT coalesce(jsonb_agg(jsonb_build_object(
                            'hostname', hostname, 'kind', kind, 'ref_name', ref_name)
                         ORDER BY hostname), '[]'::jsonb)
                 FROM ci_preview_aliases WHERE ci_preview_aliases.build_id = ci_builds.id) AS aliases,
                array(SELECT DISTINCT pull_request_number FROM ci_build_queue
                      WHERE ci_build_queue.build_id = ci_builds.id
                      ORDER BY pull_request_number) AS pull_requests
         FROM ci_builds ORDER BY ci_builds.updated_at DESC LIMIT 50",
    )
    .fetch_all(pool);
    let previews = sqlx::query_as::<_, AdminPreview>(
        "SELECT alias.id, alias.hostname, alias.kind, alias.ref_name, alias.commit_sha, alias.state,
                alias.expires_at, runtime.state AS runtime_state, workers.name AS worker_name,
                CASE jsonb_typeof(runtime.reported_players)
                     WHEN 'array' THEN jsonb_array_length(runtime.reported_players) ELSE 0 END::bigint
                  AS players_online,
                runtime.last_activity_at
         FROM ci_preview_aliases alias
         LEFT JOIN ci_runtime_placements runtime ON runtime.preview_id = alias.id
         LEFT JOIN workers ON workers.id = runtime.worker_id
         WHERE alias.expires_at > now()
         ORDER BY alias.updated_at DESC LIMIT 100",
    )
    .fetch_all(pool);
    let webhook_deliveries = sqlx::query_as::<_, AdminWebhookDeliveries>(
        "SELECT count(*) FILTER (WHERE processed_at IS NULL) AS pending,
                count(*) FILTER (WHERE processed_at IS NULL AND last_error IS NOT NULL) AS failing
         FROM ci_github_webhook_deliveries",
    )
    .fetch_one(pool);
    let (
        repositories,
        github_rate_limits,
        queue,
        builds_by_stage,
        builder_slots,
        builds,
        previews,
        webhook_deliveries,
    ) = tokio::try_join!(
        repositories,
        rate_limits,
        queue,
        builds_by_stage,
        builder_slots,
        builds,
        previews,
        webhook_deliveries
    )?;
    Ok(Json(AdminCi {
        repositories,
        github_rate_limits,
        queue,
        builds_by_stage,
        builder_slots,
        builds,
        previews,
        webhook_deliveries,
    }))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct BuildIdPath {
    build_id: Uuid,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminBuildLog {
    id: Uuid,
    commit_sha: String,
    stage: String,
    failure_message: Option<String>,
    build_log: Option<String>,
}

pub(super) async fn build_log(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ApiPath(path): ApiPath<BuildIdPath>,
) -> Result<Json<AdminBuildLog>, ApiError> {
    sqlx::query_as::<_, AdminBuildLog>(
        "SELECT id, commit_sha, stage, failure_message, build_log FROM ci_builds WHERE id = $1",
    )
    .bind(path.build_id)
    .fetch_optional(&state.pool)
    .await?
    .map(Json)
    .ok_or_else(|| {
        ApiError::with_code(
            axum::http::StatusCode::NOT_FOUND,
            "build_not_found",
            "build not found",
        )
    })
}
