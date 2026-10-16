//! The whole service at a glance, for the landing page: how many servers there are,
//! how many are running and how many people are on them, and how much of the live
//! workers' CPU and memory the running servers use.
//!
//! Usage comes only from runtime samples the API would still show for their server
//! (`crate::runtime_stats`): fresh, and from the server's current generation. A
//! resource no running server reported stays unknown rather than being read as zero.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;
use sqlx::{PgExecutor, PgPool, Row};

use crate::runtime_stats::RUNTIME_STATS_FRESHNESS_SECONDS;

/// Minutes of history the page graphs, a column a minute.
pub const HISTORY_MINUTES: i32 = 120;
/// Days of servers the page graphs, a bar a day.
pub const SERVER_DAYS: i32 = 30;
/// How long a minute's sample is kept.
const RETENTION_HOURS: i32 = 24;

/// The running servers' totals at one moment.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct FleetUsage {
    pub servers_online: i64,
    pub players_online: i64,
    /// CPU the running servers use, in thousandths of a core.
    pub cpu_used_millis: Option<i64>,
    /// CPU of every live worker, in thousandths of a core.
    pub cpu_total_millis: Option<i64>,
    pub memory_used_bytes: Option<i64>,
    pub memory_total_bytes: Option<i64>,
    /// The running servers' average ticks per second.
    pub tps: Option<f64>,
}

/// One minute of the service, as it was sampled at the start of that minute.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct FleetSample {
    pub minute: DateTime<Utc>,
    pub servers_online: i64,
    pub players_online: i64,
    pub cpu_used_millis: Option<i64>,
    pub cpu_total_millis: Option<i64>,
    pub memory_used_bytes: Option<i64>,
    pub memory_total_bytes: Option<i64>,
    pub tps: Option<f64>,
}

/// The whole service now, and how it got here.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct FleetStats {
    pub measured_at: DateTime<Utc>,
    /// Servers made and not deleted.
    pub servers_created: i64,
    pub servers_online: i64,
    pub players_online: i64,
    pub cpu_used_millis: Option<i64>,
    pub cpu_total_millis: Option<i64>,
    pub memory_used_bytes: Option<i64>,
    pub memory_total_bytes: Option<i64>,
    pub tps: Option<f64>,
    /// The last two hours, a sample a minute, oldest first. A minute nobody sampled is missing.
    pub history: Vec<FleetSample>,
    /// Servers there were at the end of each of the last thirty days, oldest first; the last is today so far.
    pub servers_by_day: Vec<i64>,
}

struct Now {
    measured_at: DateTime<Utc>,
    servers_created: i64,
    usage: FleetUsage,
}

async fn now<'e>(executor: impl PgExecutor<'e>) -> sqlx::Result<Now> {
    let row = sqlx::query(
        "WITH online AS (
             SELECT id, generation FROM servers WHERE observed_state = 'online'
         ), sampled AS (
             SELECT stats.stats
             FROM server_runtime_stats stats
             JOIN online ON online.id = stats.server_id AND online.generation = stats.generation
             WHERE stats.measured_at > now() - make_interval(secs => $1::double precision)
         ), live_workers AS (
             SELECT total_cpu_millis, total_memory_bytes
             FROM workers
             WHERE heartbeat_at > now() - make_interval(secs => $1::double precision)
         )
         SELECT now() AS measured_at,
                (SELECT count(*) FROM servers WHERE desired_state <> 'deleted') AS servers_created,
                (SELECT count(*) FROM online) AS servers_online,
                (SELECT count(*) FROM server_players JOIN online ON online.id = server_players.server_id) AS players_online,
                (SELECT round(sum((stats->>'cpu_percent')::double precision / 100
                                  * (stats->>'cpu_limit_millis')::double precision))::bigint
                 FROM sampled) AS cpu_used_millis,
                (SELECT sum(total_cpu_millis)::bigint FROM live_workers) AS cpu_total_millis,
                (SELECT sum((stats->>'memory_used_bytes')::bigint)::bigint FROM sampled) AS memory_used_bytes,
                (SELECT sum(total_memory_bytes)::bigint FROM live_workers) AS memory_total_bytes,
                (SELECT avg((stats->>'tps')::double precision) FROM sampled) AS tps",
    )
    .bind(RUNTIME_STATS_FRESHNESS_SECONDS as f64)
    .fetch_one(executor)
    .await?;
    Ok(Now {
        measured_at: row.try_get("measured_at")?,
        servers_created: row.try_get("servers_created")?,
        usage: FleetUsage {
            servers_online: row.try_get("servers_online")?,
            players_online: row.try_get("players_online")?,
            cpu_used_millis: row.try_get("cpu_used_millis")?,
            cpu_total_millis: row.try_get("cpu_total_millis")?,
            memory_used_bytes: row.try_get("memory_used_bytes")?,
            memory_total_bytes: row.try_get("memory_total_bytes")?,
            tps: row.try_get("tps")?,
        },
    })
}

/// The running servers' totals right now.
pub async fn usage<'e>(executor: impl PgExecutor<'e>) -> sqlx::Result<FleetUsage> {
    Ok(now(executor).await?.usage)
}

/// Sample the current minute, unless it already is, and forget samples older than a day.
pub async fn record_minute(pool: &PgPool) -> sqlx::Result<()> {
    let usage = usage(pool).await?;
    sqlx::query(
        "INSERT INTO fleet_stats(minute, servers_online, players_online, cpu_used_millis,
                                 cpu_total_millis, memory_used_bytes, memory_total_bytes, tps)
         VALUES(date_trunc('minute', now()), $1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT(minute) DO NOTHING",
    )
    .bind(usage.servers_online as i32)
    .bind(usage.players_online as i32)
    .bind(usage.cpu_used_millis)
    .bind(usage.cpu_total_millis)
    .bind(usage.memory_used_bytes)
    .bind(usage.memory_total_bytes)
    .bind(usage.tps.map(|tps| tps.clamp(0.0, 20.0)))
    .execute(pool)
    .await?;
    sqlx::query("DELETE FROM fleet_stats WHERE minute < now() - make_interval(hours => $1)")
        .bind(RETENTION_HOURS)
        .execute(pool)
        .await?;
    Ok(())
}

async fn history(pool: &PgPool) -> sqlx::Result<Vec<FleetSample>> {
    let rows = sqlx::query(
        "SELECT minute, servers_online, players_online, cpu_used_millis, cpu_total_millis,
                memory_used_bytes, memory_total_bytes, tps
         FROM fleet_stats
         WHERE minute > now() - make_interval(mins => $1)
         ORDER BY minute",
    )
    .bind(HISTORY_MINUTES)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(FleetSample {
                minute: row.try_get("minute")?,
                servers_online: i64::from(row.try_get::<i32, _>("servers_online")?),
                players_online: i64::from(row.try_get::<i32, _>("players_online")?),
                cpu_used_millis: row.try_get("cpu_used_millis")?,
                cpu_total_millis: row.try_get("cpu_total_millis")?,
                memory_used_bytes: row.try_get("memory_used_bytes")?,
                memory_total_bytes: row.try_get("memory_total_bytes")?,
                tps: row.try_get("tps")?,
            })
        })
        .collect()
}

async fn servers_by_day(pool: &PgPool) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT count(servers.id)
         FROM generate_series(date_trunc('day', now()) - make_interval(days => $1 - 1),
                              date_trunc('day', now()), interval '1 day') AS day(start)
         LEFT JOIN servers ON servers.desired_state <> 'deleted'
                          AND servers.created_at < day.start + interval '1 day'
         GROUP BY day.start
         ORDER BY day.start",
    )
    .bind(SERVER_DAYS)
    .fetch_all(pool)
    .await
}

/// The whole service now, the last two hours of it, and its last thirty days of servers.
pub async fn fleet_stats(pool: &PgPool) -> sqlx::Result<FleetStats> {
    let (now, history, servers_by_day) =
        tokio::try_join!(now(pool), history(pool), servers_by_day(pool))?;
    let FleetUsage {
        servers_online,
        players_online,
        cpu_used_millis,
        cpu_total_millis,
        memory_used_bytes,
        memory_total_bytes,
        tps,
    } = now.usage;
    Ok(FleetStats {
        measured_at: now.measured_at,
        servers_created: now.servers_created,
        servers_online,
        players_online,
        cpu_used_millis,
        cpu_total_millis,
        memory_used_bytes,
        memory_total_bytes,
        tps,
        history,
        servers_by_day,
    })
}
