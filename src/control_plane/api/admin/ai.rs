use std::sync::Arc;

use axum::{Json, extract::State};
use chrono::{DateTime, NaiveDate, Utc};
use schemars::JsonSchema;
use serde::Serialize;
use uuid::Uuid;

use super::super::{ApiError, AppState};
use super::AdminUser;
use super::overview::AdminStateCount;

/// The budgets the AI gateway enforces, each a month's spend in millionths of a US dollar.
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AdminAiBudgets {
    /// The default for a server; a server may set its own up to the account's.
    server_monthly_micros: i64,
    account_monthly_micros: i64,
    max_concurrent_calls: usize,
    models: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminAiMonth {
    cost_micros: i64,
    model_calls: i64,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    /// Requests started this month.
    jobs: i64,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminAiDay {
    day: NaiveDate,
    cost_micros: i64,
    model_calls: i64,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminAiModel {
    model: String,
    cost_micros: i64,
    model_calls: i64,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
}

/// A server or account and what it spent this month.
#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminAiSpender {
    id: Uuid,
    /// The server's address or the account's name; null once it is deleted.
    name: Option<String>,
    cost_micros: i64,
    model_calls: i64,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminAiJob {
    id: Uuid,
    server_id: Uuid,
    subdomain: Option<String>,
    operator_name: String,
    prompt: String,
    state: String,
    error: Option<String>,
    stalls: i32,
    model_calls: i32,
    tool_calls: i32,
    cost_micros: i64,
    created_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AdminAi {
    /// Null when no model provider is configured.
    budgets: Option<AdminAiBudgets>,
    month: AdminAiMonth,
    /// The last 30 days, oldest first, every day present.
    by_day: Vec<AdminAiDay>,
    /// This month.
    models: Vec<AdminAiModel>,
    /// The ten that spent most this month.
    top_servers: Vec<AdminAiSpender>,
    top_accounts: Vec<AdminAiSpender>,
    /// Requests of the last 30 days by state.
    jobs_by_state: Vec<AdminStateCount>,
    /// The latest 50 requests that failed or stalled.
    problem_jobs: Vec<AdminAiJob>,
}

const USAGE_TOTALS: &str = "coalesce(sum(cost_micros), 0)::bigint AS cost_micros,
       count(*) AS model_calls,
       coalesce(sum(input_tokens), 0)::bigint AS input_tokens,
       coalesce(sum(output_tokens), 0)::bigint AS output_tokens,
       coalesce(sum(cache_read_input_tokens), 0)::bigint AS cache_read_input_tokens,
       coalesce(sum(cache_creation_input_tokens), 0)::bigint AS cache_creation_input_tokens";

pub(super) async fn ai(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> Result<Json<AdminAi>, ApiError> {
    let pool = &state.pool;
    let month_query = format!(
        "SELECT {USAGE_TOTALS},
                (SELECT count(*) FROM ai_jobs WHERE created_at >= date_trunc('month', now())) AS jobs
         FROM ai_usage WHERE created_at >= date_trunc('month', now())"
    );
    let month = sqlx::query_as::<_, AdminAiMonth>(&month_query).fetch_one(pool);
    let by_day = sqlx::query_as::<_, AdminAiDay>(
        "SELECT days.day::date AS day,
                coalesce(sum(ai_usage.cost_micros), 0)::bigint AS cost_micros,
                count(ai_usage.id) AS model_calls
         FROM generate_series((now() AT TIME ZONE 'UTC')::date - 29,
                              (now() AT TIME ZONE 'UTC')::date, interval '1 day') AS days(day)
         LEFT JOIN ai_usage ON ai_usage.created_at >= (days.day AT TIME ZONE 'UTC')
                           AND ai_usage.created_at < ((days.day + interval '1 day') AT TIME ZONE 'UTC')
         GROUP BY days.day ORDER BY days.day",
    )
    .fetch_all(pool);
    let models_query = format!(
        "SELECT model, {USAGE_TOTALS} FROM ai_usage
         WHERE created_at >= date_trunc('month', now())
         GROUP BY model ORDER BY cost_micros DESC"
    );
    let models = sqlx::query_as::<_, AdminAiModel>(&models_query).fetch_all(pool);
    let top_servers = sqlx::query_as::<_, AdminAiSpender>(
        "SELECT ai_usage.server_id AS id, servers.subdomain AS name,
                sum(ai_usage.cost_micros)::bigint AS cost_micros, count(*) AS model_calls
         FROM ai_usage LEFT JOIN servers ON servers.id = ai_usage.server_id
         WHERE ai_usage.created_at >= date_trunc('month', now())
         GROUP BY ai_usage.server_id, servers.subdomain
         ORDER BY cost_micros DESC LIMIT 10",
    )
    .fetch_all(pool);
    let top_accounts = sqlx::query_as::<_, AdminAiSpender>(
        "SELECT ai_usage.owner_id AS id, users.display_name AS name,
                sum(ai_usage.cost_micros)::bigint AS cost_micros, count(*) AS model_calls
         FROM ai_usage LEFT JOIN users ON users.id = ai_usage.owner_id
         WHERE ai_usage.created_at >= date_trunc('month', now())
         GROUP BY ai_usage.owner_id, users.display_name
         ORDER BY cost_micros DESC LIMIT 10",
    )
    .fetch_all(pool);
    let jobs_by_state = sqlx::query_as::<_, AdminStateCount>(
        "SELECT state, count(*) AS count FROM ai_jobs
         WHERE created_at > now() - interval '30 days'
         GROUP BY state ORDER BY state",
    )
    .fetch_all(pool);
    let problem_jobs = sqlx::query_as::<_, AdminAiJob>(
        "SELECT ai_jobs.id, ai_jobs.server_id, servers.subdomain, ai_jobs.operator_name,
                ai_jobs.prompt, ai_jobs.state, ai_jobs.error, ai_jobs.stalls, ai_jobs.model_calls,
                ai_jobs.tool_calls, ai_jobs.cost_micros, ai_jobs.created_at, ai_jobs.finished_at
         FROM ai_jobs LEFT JOIN servers ON servers.id = ai_jobs.server_id
         WHERE ai_jobs.state = 'failed' OR ai_jobs.stalls > 0
         ORDER BY ai_jobs.created_at DESC LIMIT 50",
    )
    .fetch_all(pool);
    let (month, by_day, models, top_servers, top_accounts, jobs_by_state, problem_jobs) = tokio::try_join!(
        month,
        by_day,
        models,
        top_servers,
        top_accounts,
        jobs_by_state,
        problem_jobs
    )?;
    let budgets = state.ai.as_ref().map(|ai| {
        let config = ai.config();
        AdminAiBudgets {
            server_monthly_micros: config.server_monthly_budget_micros,
            account_monthly_micros: config.account_monthly_budget_micros,
            max_concurrent_calls: config.max_concurrent_calls,
            models: config.models.iter().map(|model| model.id.clone()).collect(),
        }
    });
    Ok(Json(AdminAi {
        budgets,
        month,
        by_day,
        models,
        top_servers,
        top_accounts,
        jobs_by_state,
        problem_jobs,
    }))
}
