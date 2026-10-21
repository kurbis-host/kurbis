use std::{collections::BTreeMap, sync::Arc};

use axum::{Json, extract::State};
use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::Serialize;

use super::super::{ApiError, AppState};
use super::AdminUser;

/// Weeks of signups the retention table shows, and weeks after signup it follows.
const COHORT_WEEKS: i32 = 12;
const RETURN_WEEKS: usize = 8;

/// One UTC day.
#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminDay {
    day: NaiveDate,
    signups: i32,
    servers_created: i32,
    /// Successful wakes asked for that day, and the servers they woke.
    wakes: i32,
    servers_woken: i32,
    /// Highest per-minute sample of the day; null for days nobody sampled.
    peak_servers_online: Option<i32>,
    peak_players_online: Option<i32>,
    ai_cost_micros: i64,
}

/// The accounts made in one week, and how many of them had their server woken in each
/// week after.
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AdminCohort {
    /// Monday of the signup week.
    week: NaiveDate,
    accounts: i64,
    /// Accounts with a wake in UTC calendar week 0 (the week of signup), 1, … up to 7
    /// after it; only weeks that have begun are listed.
    returned: Vec<i64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AdminGrowth {
    /// Every day since the first account, oldest first.
    days: Vec<AdminDay>,
    /// The last twelve signup weeks, oldest first.
    cohorts: Vec<AdminCohort>,
}

pub(super) async fn growth(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> Result<Json<AdminGrowth>, ApiError> {
    let pool = &state.pool;
    let days = sqlx::query_as::<_, AdminDay>(
        "SELECT day, signups, servers_created, wakes, servers_woken, peak_servers_online,
                peak_players_online, ai_cost_micros
         FROM daily_stats ORDER BY day",
    )
    .fetch_all(pool);
    // Weeks are UTC calendar weeks, starting on Monday: an account's week 0 is the week
    // it signed up in, week 1 the calendar week after, whatever day it signed up on.
    let sizes = sqlx::query_as::<_, (NaiveDate, i64, i32)>(
        "WITH first_week AS (
             SELECT date_trunc('week', now() AT TIME ZONE 'UTC') - make_interval(weeks => $1 - 1) AS at
         )
         SELECT week::date, count(*),
                (floor(extract(epoch FROM date_trunc('week', now() AT TIME ZONE 'UTC') - week)
                       / 604800))::int
         FROM (SELECT date_trunc('week', created_at AT TIME ZONE 'UTC') AS week
               FROM users, first_week WHERE created_at >= first_week.at AT TIME ZONE 'UTC'
              ) signups
         GROUP BY week ORDER BY week",
    )
    .bind(COHORT_WEEKS)
    .fetch_all(pool);
    let returns = sqlx::query_as::<_, (NaiveDate, i32, i64)>(
        "WITH first_week AS (
             SELECT date_trunc('week', now() AT TIME ZONE 'UTC') - make_interval(weeks => $1 - 1) AS at
         ), cohort AS (
             SELECT id, created_at, date_trunc('week', created_at AT TIME ZONE 'UTC') AS week
             FROM users, first_week WHERE created_at >= first_week.at AT TIME ZONE 'UTC'
         )
         SELECT cohort.week::date,
                (floor(extract(epoch FROM date_trunc('week', operations.created_at AT TIME ZONE 'UTC')
                                          - cohort.week) / 604800))::int,
                count(DISTINCT cohort.id)
         FROM cohort
         JOIN servers ON servers.owner_id = cohort.id
         JOIN operations ON operations.server_id = servers.id
                        AND operations.kind = 'start' AND operations.status = 'succeeded'
                        AND operations.created_at >= cohort.created_at
                        AND operations.created_at
                            < (cohort.week + make_interval(weeks => $2)) AT TIME ZONE 'UTC'
         GROUP BY 1, 2",
    )
    .bind(COHORT_WEEKS)
    .bind(RETURN_WEEKS as i32)
    .fetch_all(pool);
    let (days, sizes, returns) = tokio::try_join!(days, sizes, returns)?;
    let mut returned: BTreeMap<(NaiveDate, i32), i64> = BTreeMap::new();
    for (week, index, accounts) in returns {
        returned.insert((week, index), accounts);
    }
    let cohorts = sizes
        .into_iter()
        .map(|(week, accounts, weeks_since)| AdminCohort {
            week,
            accounts,
            returned: (0..=weeks_since.clamp(0, RETURN_WEEKS as i32 - 1))
                .map(|index| returned.get(&(week, index)).copied().unwrap_or(0))
                .collect(),
        })
        .collect();
    Ok(Json(AdminGrowth { days, cohorts }))
}
