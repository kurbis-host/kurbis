use std::sync::Arc;

use axum::{Json, extract::State};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::runtime_stats::RUNTIME_STATS_FRESHNESS_SECONDS;

use super::super::{ApiError, AppState};
use super::{AdminUser, WORKER_LIVE_SECONDS};

/// A server placed on a worker.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub(super) struct AdminWorkerServer {
    id: Uuid,
    subdomain: String,
    observed_state: String,
    active: bool,
    players_online: i64,
}

/// A Pumpkin preview placed on a worker.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub(super) struct AdminWorkerPreview {
    preview_id: Uuid,
    hostname: Option<String>,
    state: String,
    players_online: i64,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminWorker {
    id: Uuid,
    name: String,
    address: String,
    runtime_architecture: String,
    supports_ci_previews: bool,
    draining: bool,
    heartbeat_at: DateTime<Utc>,
    /// Heartbeat within the last 30 seconds: it may take new servers.
    live: bool,
    /// When the running worker process's exclusive lease runs out.
    lease_expires_at: Option<DateTime<Utc>>,
    total_memory_bytes: i64,
    reserved_memory_bytes: i64,
    total_disk_bytes: i64,
    reserved_disk_bytes: i64,
    total_cpu_millis: i64,
    reserved_cpu_millis: i64,
    /// What its running servers use, from their fresh samples; null when none reported.
    cpu_used_millis: Option<i64>,
    memory_used_bytes: Option<i64>,
    port_min: i32,
    port_max: i32,
    /// Ports leased to servers and previews.
    ports_leased: i64,
    #[sqlx(json)]
    servers: Vec<AdminWorkerServer>,
    #[sqlx(json)]
    previews: Vec<AdminWorkerPreview>,
}

pub(super) async fn workers(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> Result<Json<Vec<AdminWorker>>, ApiError> {
    let workers = sqlx::query_as::<_, AdminWorker>(
        "SELECT workers.id, workers.name, workers.address, workers.runtime_architecture,
                workers.supports_ci_previews, workers.draining, workers.heartbeat_at,
                workers.heartbeat_at > now() - make_interval(secs => $1) AS live,
                (SELECT max(lease_expires_at) FROM worker_credentials
                 WHERE worker_credentials.worker_id = workers.id AND revoked_at IS NULL) AS lease_expires_at,
                workers.total_memory_bytes, workers.reserved_memory_bytes,
                workers.total_disk_bytes, workers.reserved_disk_bytes,
                workers.total_cpu_millis, workers.reserved_cpu_millis,
                usage.cpu_used_millis, usage.memory_used_bytes,
                workers.port_min, workers.port_max,
                (SELECT count(*) FROM port_leases WHERE port_leases.worker_id = workers.id)
                  + (SELECT count(*) FROM ci_port_leases WHERE ci_port_leases.worker_id = workers.id)
                  AS ports_leased,
                (SELECT coalesce(jsonb_agg(jsonb_build_object(
                            'id', servers.id, 'subdomain', servers.subdomain,
                            'observed_state', servers.observed_state, 'active', placements.active,
                            'players_online', (SELECT count(*) FROM server_players
                                               WHERE server_players.server_id = servers.id))
                         ORDER BY servers.subdomain), '[]'::jsonb)
                 FROM placements JOIN servers ON servers.id = placements.server_id
                 WHERE placements.worker_id = workers.id) AS servers,
                (SELECT coalesce(jsonb_agg(jsonb_build_object(
                            'preview_id', ci_runtime_placements.preview_id,
                            'hostname', ci_preview_aliases.hostname,
                            'state', ci_runtime_placements.state,
                            'players_online', CASE jsonb_typeof(ci_runtime_placements.reported_players)
                                                  WHEN 'array' THEN jsonb_array_length(ci_runtime_placements.reported_players)
                                                  ELSE 0 END)
                         ORDER BY ci_preview_aliases.hostname), '[]'::jsonb)
                 FROM ci_runtime_placements
                 LEFT JOIN ci_preview_aliases ON ci_preview_aliases.id = ci_runtime_placements.preview_id
                 WHERE ci_runtime_placements.worker_id = workers.id) AS previews
         FROM workers
         LEFT JOIN LATERAL (
             SELECT round(sum((stats.stats->>'cpu_percent')::double precision / 100
                              * (stats.stats->>'cpu_limit_millis')::double precision))::bigint AS cpu_used_millis,
                    sum((stats.stats->>'memory_used_bytes')::bigint)::bigint AS memory_used_bytes
             FROM placements
             JOIN servers ON servers.id = placements.server_id AND servers.observed_state = 'online'
             JOIN server_runtime_stats stats
               ON stats.server_id = servers.id AND stats.generation = servers.generation
              AND stats.measured_at > now() - make_interval(secs => $2)
             WHERE placements.worker_id = workers.id AND placements.active
         ) usage ON true
         ORDER BY workers.name",
    )
    .bind(WORKER_LIVE_SECONDS)
    .bind(RUNTIME_STATS_FRESHNESS_SECONDS as f64)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(workers))
}
