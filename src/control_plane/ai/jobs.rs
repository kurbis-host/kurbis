//! Operator requests: creation, cancellation and the views the panel and the
//! plugin read.

use chrono::{DateTime, Utc};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use crate::contracts::ai::{
    AiJob, AiJobDetail, AiJobRequest, AiJobState, AiOverview, AiSettings, AiToolCall,
    AiToolCallState,
};

use super::{
    AiError, AiGateway,
    budget::{self, Budgets, Exhausted},
    catalog, prompt, settings,
    transitions::{self, ACTIVE_STATES},
};

/// A follow-up continues the operator's last conversation while it is this
/// fresh and short.
const CONTINUE_WITHIN_MINUTES: i32 = 60;
const MAX_CONVERSATION_MESSAGES: i32 = 160;
/// Wall-clock limit of one job, including every tool round.
const JOB_DEADLINE_MINUTES: i32 = 15;
const RECENT_JOBS: i64 = 20;

const JOB_SELECT: &str = "SELECT j.id, j.conversation_id, j.operator_id, j.operator_name,
        j.prompt, c.model, j.state, j.answer, j.error, j.model_calls, j.tool_calls,
        j.cost_micros, j.created_at, j.updated_at, j.finished_at
 FROM ai_jobs j JOIN ai_conversations c ON c.id = j.conversation_id";

#[derive(sqlx::FromRow)]
struct JobRow {
    id: Uuid,
    conversation_id: Uuid,
    operator_id: Uuid,
    operator_name: String,
    prompt: String,
    model: String,
    state: String,
    answer: Option<String>,
    error: Option<String>,
    model_calls: i32,
    tool_calls: i32,
    cost_micros: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

impl From<JobRow> for AiJob {
    fn from(row: JobRow) -> Self {
        Self {
            id: row.id,
            conversation_id: row.conversation_id,
            operator_id: row.operator_id,
            operator_name: row.operator_name,
            prompt: row.prompt,
            model: row.model,
            // The column's check constraint admits only known states.
            state: AiJobState::parse(&row.state).unwrap_or(AiJobState::Failed),
            answer: row.answer,
            error: row.error,
            model_calls: row.model_calls,
            tool_calls: row.tool_calls,
            cost_micros: row.cost_micros,
            created_at: row.created_at,
            updated_at: row.updated_at,
            finished_at: row.finished_at,
        }
    }
}

pub async fn job(
    executor: impl PgExecutor<'_>,
    server_id: Uuid,
    job_id: Uuid,
) -> Result<Option<AiJob>, sqlx::Error> {
    Ok(sqlx::query_as::<_, JobRow>(&format!(
        "{JOB_SELECT} WHERE j.id = $1 AND j.server_id = $2"
    ))
    .bind(job_id)
    .bind(server_id)
    .fetch_optional(executor)
    .await?
    .map(AiJob::from))
}

pub async fn recent_jobs(pool: &PgPool, server_id: Uuid) -> Result<Vec<AiJob>, sqlx::Error> {
    Ok(sqlx::query_as::<_, JobRow>(&format!(
        "{JOB_SELECT} WHERE j.server_id = $1
         ORDER BY j.created_at DESC, j.id DESC LIMIT $2"
    ))
    .bind(server_id)
    .bind(RECENT_JOBS)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(AiJob::from)
    .collect())
}

#[derive(sqlx::FromRow)]
struct ToolCallRow {
    call_id: String,
    name: String,
    input_json: String,
    state: String,
    output: Option<String>,
    is_error: bool,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

pub async fn job_detail(
    pool: &PgPool,
    server_id: Uuid,
    job_id: Uuid,
) -> Result<Option<AiJobDetail>, sqlx::Error> {
    let Some(job) = job(pool, server_id, job_id).await? else {
        return Ok(None);
    };
    let rows: Vec<ToolCallRow> = sqlx::query_as(
        "SELECT call_id, name, input_json, state, output, is_error, created_at, completed_at
         FROM ai_tool_calls WHERE job_id = $1
         ORDER BY seq",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await?;
    let tool_calls = rows
        .into_iter()
        .map(|row| AiToolCall {
            call_id: row.call_id,
            name: row.name,
            input: serde_json::from_str(&row.input_json).unwrap_or(serde_json::Value::Null),
            state: AiToolCallState::parse(&row.state).unwrap_or(AiToolCallState::Completed),
            output: row.output,
            is_error: row.is_error,
            created_at: row.created_at,
            completed_at: row.completed_at,
        })
        .collect();
    Ok(Some(AiJobDetail { job, tool_calls }))
}

/// Everything the panel shows. Works without a configured provider so owners
/// still see past usage.
pub async fn overview(
    pool: &PgPool,
    gateway: Option<&AiGateway>,
    server_id: Uuid,
    owner_id: Uuid,
) -> Result<AiOverview, sqlx::Error> {
    let settings = settings::load(pool, server_id).await?;
    let (server_budget, account_budget) = gateway.map_or((0, 0), |gateway| {
        let effective = gateway.effective(&settings);
        (
            effective.server_budget_micros,
            effective.account_budget_micros,
        )
    });
    Ok(AiOverview {
        available: gateway.is_some(),
        defaults: gateway.map(AiGateway::defaults),
        models: if gateway.is_some() {
            catalog::selectable_models()
        } else {
            Vec::new()
        },
        usage: budget::usage(pool, server_id, owner_id, server_budget, account_budget).await?,
        jobs: recent_jobs(pool, server_id).await?,
        settings,
    })
}

impl AiGateway {
    pub async fn update_settings(
        &self,
        server_id: Uuid,
        settings: &AiSettings,
    ) -> Result<(), AiError> {
        if let Some(model) = &settings.model
            && catalog::selectable(model).is_none()
        {
            return Err(AiError::UnknownModel);
        }
        if settings
            .monthly_budget_micros
            .is_some_and(|budget| budget > self.config().account_monthly_budget_micros)
        {
            return Err(AiError::BudgetAboveAccount);
        }
        settings::store(self.pool(), server_id, settings).await?;
        Ok(())
    }

    /// Start a job, or return the existing one when the request is a retry.
    /// The caller has verified that the operator administers this server.
    pub async fn create_job(
        &self,
        server_id: Uuid,
        owner_id: Uuid,
        request: &AiJobRequest,
    ) -> Result<(AiJob, bool), AiError> {
        let prompt = request.prompt.trim();
        if prompt.is_empty() {
            return Err(AiError::EmptyPrompt);
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!(
                "kurbis-ai-operator:{server_id}:{}",
                request.operator_id
            ))
            .execute(&mut *tx)
            .await?;

        let existing: Option<(Uuid, Uuid, String)> =
            sqlx::query_as("SELECT server_id, operator_id, prompt FROM ai_jobs WHERE id = $1")
                .bind(request.job_id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(existing) = existing {
            if existing != (server_id, request.operator_id, prompt.to_owned()) {
                return Err(AiError::JobIdReused);
            }
            let job = job(&mut *tx, server_id, request.job_id)
                .await?
                .ok_or(AiError::JobNotFound)?;
            return Ok((job, false));
        }

        let effective = self.effective(&settings::load(&mut *tx, server_id).await?);
        if !effective.enabled {
            return Err(AiError::Disabled);
        }
        let budgets = Budgets::load(
            &mut tx,
            server_id,
            owner_id,
            effective.server_budget_micros,
            effective.account_budget_micros,
        )
        .await?;
        match budgets.available() {
            (available, _) if available > 0 => {}
            (_, Exhausted::Server) => return Err(AiError::ServerBudgetExhausted),
            (_, Exhausted::Account) => return Err(AiError::AccountBudgetExhausted),
        }
        let active: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS(SELECT 1 FROM ai_jobs
                           WHERE server_id = $1 AND operator_id = $2 AND state IN {ACTIVE_STATES})"
        ))
        .bind(server_id)
        .bind(request.operator_id)
        .fetch_one(&mut *tx)
        .await?;
        if active {
            return Err(AiError::JobActive);
        }

        let continued: Option<Uuid> = if request.new_conversation {
            None
        } else {
            sqlx::query_scalar(
                "SELECT id FROM ai_conversations
                 WHERE server_id = $1 AND operator_id = $2 AND closed_at IS NULL
                   AND model = $3 AND effort = $4 AND message_count < $5
                   AND updated_at > now() - make_interval(mins => $6)
                 ORDER BY updated_at DESC LIMIT 1",
            )
            .bind(server_id)
            .bind(request.operator_id)
            .bind(effective.model.id)
            .bind(effective.effort.as_str())
            .bind(MAX_CONVERSATION_MESSAGES)
            .bind(CONTINUE_WITHIN_MINUTES)
            .fetch_optional(&mut *tx)
            .await?
        };
        let conversation_id = match continued {
            Some(id) => id,
            None => {
                let id = Uuid::new_v4();
                sqlx::query(
                    "INSERT INTO ai_conversations(id, server_id, operator_id, model, effort,
                                                  system_json, tools_json)
                     VALUES($1, $2, $3, $4, $5, $6, $7)",
                )
                .bind(id)
                .bind(server_id)
                .bind(request.operator_id)
                .bind(effective.model.id)
                .bind(effective.effort.as_str())
                .bind(serde_json::Value::from(prompt::SYSTEM_PROMPT).to_string())
                .bind(prompt::tools().to_string())
                .execute(&mut *tx)
                .await?;
                id
            }
        };

        sqlx::query(
            "INSERT INTO ai_jobs(id, server_id, conversation_id, operator_id, operator_name,
                                 prompt, deadline_at)
             VALUES($1, $2, $3, $4, $5, $6, now() + make_interval(mins => $7))",
        )
        .bind(request.job_id)
        .bind(server_id)
        .bind(conversation_id)
        .bind(request.operator_id)
        .bind(request.operator_name.trim())
        .bind(prompt)
        .bind(JOB_DEADLINE_MINUTES)
        .execute(&mut *tx)
        .await?;
        transitions::append_message(
            &mut tx,
            conversation_id,
            "user",
            &prompt::user_turn(request.operator_name.trim(), request.operator_id, prompt)
                .to_string(),
        )
        .await?;
        let job = job(&mut *tx, server_id, request.job_id)
            .await?
            .ok_or(AiError::JobNotFound)?;
        tx.commit().await?;
        self.wake();
        Ok((job, true))
    }
}

/// Cancel an active job; cancelling a finished job returns it unchanged.
pub async fn cancel_job(pool: &PgPool, server_id: Uuid, job_id: Uuid) -> Result<AiJob, AiError> {
    let mut tx = pool.begin().await?;
    if transitions::lock_job(&mut tx, job_id, server_id)
        .await?
        .is_none()
    {
        return Err(AiError::JobNotFound);
    }
    transitions::abort(&mut tx, job_id, AiJobState::Cancelled, "Request cancelled.").await?;
    let job = job(&mut *tx, server_id, job_id)
        .await?
        .ok_or(AiError::JobNotFound)?;
    tx.commit().await?;
    Ok(job)
}
