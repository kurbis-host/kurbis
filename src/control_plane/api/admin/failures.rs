//! Everything that failed, from every table that records a failure, in one list.

use std::sync::Arc;

use axum::{Json, extract::State};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;
use uuid::Uuid;

use super::super::{ApiError, ApiQuery, AppState};
use super::{AdminUser, RangeQuery};

/// Each failure since `now() - $1 minutes` as (source, kind, message, at, server_id,
/// subdomain, reference). A source is where it was recorded; the kind says more
/// (an operation's kind, a build's stage, a plugin's name).
const FAILURES: &str = "WITH cutoff AS (SELECT now() - make_interval(mins => $1) AS at),
failures AS (
    SELECT 'operation' AS source, operations.kind::text AS kind,
           coalesce(operations.last_error, 'failed without a message') AS message,
           operations.updated_at AS at, operations.server_id, operations.id::text AS reference
    FROM operations, cutoff
    WHERE operations.status = 'failed' AND operations.updated_at > cutoff.at
    UNION ALL
    SELECT 'operation_retry', operations.kind::text, operations.last_error, operations.updated_at,
           operations.server_id, operations.id::text
    FROM operations, cutoff
    WHERE operations.status = 'pending' AND operations.attempt > 0
      AND operations.last_error IS NOT NULL AND operations.updated_at > cutoff.at
    UNION ALL
    SELECT 'server', servers.observed_state::text,
           coalesce(servers.last_error, 'failed without a message'), servers.updated_at,
           servers.id, servers.id::text
    FROM servers WHERE servers.observed_state = 'failed'
    UNION ALL
    SELECT 'export', world_exports.stage, coalesce(world_exports.last_error, 'failed without a message'),
           world_exports.updated_at, world_exports.server_id, world_exports.id::text
    FROM world_exports, cutoff
    WHERE world_exports.status = 'failed' AND world_exports.updated_at > cutoff.at
    UNION ALL
    SELECT 'plugin', server_plugins.name, server_plugins.runtime_error,
           coalesce(server_plugins.reported_at, server_plugins.updated_at),
           server_plugins.server_id, server_plugins.id::text
    FROM server_plugins WHERE server_plugins.runtime_error IS NOT NULL
    UNION ALL
    SELECT 'console', split_part(console_commands.command, ' ', 1),
           coalesce(console_commands.error, 'failed without a message'),
           console_commands.updated_at, console_commands.server_id, console_commands.id::text
    FROM console_commands, cutoff
    WHERE console_commands.status = 'failed' AND console_commands.updated_at > cutoff.at
    UNION ALL
    SELECT 'ai', 'job', coalesce(ai_jobs.error, 'failed without a message'),
           coalesce(ai_jobs.finished_at, ai_jobs.updated_at), ai_jobs.server_id, ai_jobs.id::text
    FROM ai_jobs, cutoff
    WHERE ai_jobs.state = 'failed' AND coalesce(ai_jobs.finished_at, ai_jobs.updated_at) > cutoff.at
    UNION ALL
    SELECT 'ci_build', coalesce(ci_builds.failure_stage, 'unknown'),
           concat_ws(': ', ci_builds.failure_class,
                     coalesce(ci_builds.failure_message, 'failed without a message')),
           ci_builds.updated_at, NULL, ci_builds.id::text
    FROM ci_builds, cutoff
    WHERE ci_builds.stage = 'failed' AND ci_builds.updated_at > cutoff.at
    UNION ALL
    SELECT 'ci_queue', 'pull request', coalesce(ci_build_queue.last_error, 'failed without a message'),
           ci_build_queue.updated_at, NULL, ci_build_queue.pull_request_number::text
    FROM ci_build_queue, cutoff
    WHERE ci_build_queue.state = 'failed' AND ci_build_queue.updated_at > cutoff.at
    UNION ALL
    SELECT 'github_webhook', ci_github_webhook_deliveries.action,
           ci_github_webhook_deliveries.last_error, ci_github_webhook_deliveries.received_at,
           NULL, ci_github_webhook_deliveries.delivery_id::text
    FROM ci_github_webhook_deliveries
    WHERE ci_github_webhook_deliveries.last_error IS NOT NULL
      AND ci_github_webhook_deliveries.processed_at IS NULL
    UNION ALL
    SELECT 'snapshot_gc', 'object', ci_world_snapshot_gc.last_error, ci_world_snapshot_gc.queued_at,
           NULL, ci_world_snapshot_gc.object_key
    FROM ci_world_snapshot_gc WHERE ci_world_snapshot_gc.last_error IS NOT NULL
), described AS (
    -- Ids, long hex and numbers vary between failures that are otherwise the same.
    SELECT failures.*, servers.subdomain,
           regexp_replace(
               regexp_replace(
                   regexp_replace(left(failures.message, 400),
                                  '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}', '<id>', 'gi'),
                   '[0-9a-f]{16,}', '<hex>', 'gi'),
               '[0-9]+', '<n>', 'g') AS signature
    FROM failures LEFT JOIN servers ON servers.id = failures.server_id
)";

/// Failures that said the same thing, ids and numbers aside.
#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminFailureGroup {
    source: String,
    kind: String,
    signature: String,
    /// The latest failure's own words.
    message: String,
    count: i64,
    servers: i64,
    first_at: DateTime<Utc>,
    last_at: DateTime<Utc>,
    /// The latest failure's server, when it had one.
    server_id: Option<Uuid>,
    subdomain: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminFailure {
    source: String,
    kind: String,
    message: String,
    at: DateTime<Utc>,
    server_id: Option<Uuid>,
    subdomain: Option<String>,
    /// The failed row: an operation, export, plugin, command, job or build id, a pull
    /// request number, a webhook delivery or an object key.
    reference: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AdminFailures {
    /// The 100 groups seen last, latest first.
    groups: Vec<AdminFailureGroup>,
    /// The latest 100 failures.
    recent: Vec<AdminFailure>,
}

pub(super) async fn failures(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ApiQuery(query): ApiQuery<RangeQuery>,
) -> Result<Json<AdminFailures>, ApiError> {
    let minutes = query.range.minutes();
    let groups_query = format!(
        "{FAILURES}
         SELECT source, kind, signature,
                (array_agg(message ORDER BY at DESC))[1] AS message,
                count(*) AS count, count(DISTINCT server_id) AS servers,
                min(at) AS first_at, max(at) AS last_at,
                (array_agg(server_id ORDER BY at DESC))[1] AS server_id,
                (array_agg(subdomain ORDER BY at DESC))[1] AS subdomain
         FROM described
         GROUP BY source, kind, signature
         ORDER BY last_at DESC LIMIT 100"
    );
    let groups = sqlx::query_as::<_, AdminFailureGroup>(&groups_query)
        .bind(minutes)
        .fetch_all(&state.pool);
    let recent_query = format!(
        "{FAILURES}
         SELECT source, kind, message, at, server_id, subdomain, reference
         FROM described ORDER BY at DESC LIMIT 100"
    );
    let recent = sqlx::query_as::<_, AdminFailure>(&recent_query)
        .bind(minutes)
        .fetch_all(&state.pool);
    let (groups, recent) = tokio::try_join!(groups, recent)?;
    Ok(Json(AdminFailures { groups, recent }))
}
