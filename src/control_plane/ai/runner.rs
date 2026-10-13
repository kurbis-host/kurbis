//! The job runner: claims queued jobs, calls the model with the call's worst
//! case reserved against both budgets, prices the response into the ledger
//! and moves the job to its next state. A sweep times out tool calls, expired
//! leases and overdue jobs.

use std::time::{Duration, Instant};

use serde_json::value::RawValue;
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::contracts::ai::{AiEffort, AiJobEventKind, AiJobState};

use super::{
    AiGateway,
    budget::{self, Budgets, Exhausted},
    catalog::Model,
    prompt,
    provider::{Block, ModelRequest, ModelResponse, ProviderError, Stop, Turn},
    settings,
    transitions::{self, ACTIVE_STATES, Tx},
};

/// Covers the provider timeout and its retries.
const LEASE_SECONDS: f64 = 900.0;
const MAX_OUTPUT_TOKENS: u32 = 8192;
/// Below this allowance a call is not worth starting: thinking alone can use it.
const MIN_OUTPUT_TOKENS: u32 = 2048;
const MAX_MODEL_CALLS: i32 = 24;
const MAX_TOOL_CALLS: i32 = 64;
/// The lease expiry that fails a job; earlier expiries requeue it.
const MAX_STALLS: i32 = 2;
const TOOL_RESULT_TIMEOUT_SECONDS: f64 = 30.0;
/// Transcript JSON averages well above two bytes per token, so dividing by two
/// overestimates the prompt; the constant covers request fields outside it.
const BYTES_PER_TOKEN: i64 = 2;
const REQUEST_OVERHEAD_BYTES: i64 = 2048;
const TICK: Duration = Duration::from_secs(1);
const SWEEP_INTERVAL: Duration = Duration::from_secs(5);
const RETENTION_INTERVAL: Duration = Duration::from_secs(600);
const CONVERSATION_RETENTION_DAYS: i32 = 30;
const USAGE_RETENTION_DAYS: i32 = 400;
/// Progress text standing in for work a stop interrupted.
const INTERRUPTED: &str = "This part of the response was interrupted before it finished.";

#[derive(Debug, Default)]
pub(super) struct Maintenance {
    last_sweep: Option<Instant>,
    last_retention: Option<Instant>,
}

#[derive(sqlx::FromRow)]
struct QueuedJob {
    id: Uuid,
    server_id: Uuid,
    owner_id: Uuid,
    owner_active: bool,
    conversation_id: Uuid,
    model_calls: i32,
    tool_calls: i32,
    model: String,
    effort: String,
    system_json: String,
    tools_json: String,
    history_bytes: i64,
}

/// A job holding the lease for one model call.
struct Claimed {
    job_id: Uuid,
    server_id: Uuid,
    owner_id: Uuid,
    conversation_id: Uuid,
    lease_token: Uuid,
    turn: i32,
    tool_calls: i32,
    model: Model,
    effort: AiEffort,
    max_tokens: u32,
    system_json: String,
    tools_json: String,
    messages: Vec<(String, String)>,
}

enum Claim {
    Run(Box<Claimed>),
    Skipped,
    Empty,
}

/// A model call the settings and budgets allow, with its worst case.
struct Admission {
    model: Model,
    effort: AiEffort,
    max_tokens: u32,
    reserved_micros: i64,
}

/// What a model response asks the gateway to do next.
#[derive(Debug, PartialEq)]
enum Outcome {
    Tools {
        progress: Vec<String>,
        calls: Vec<(String, String, serde_json::Value)>,
    },
    Answer(String),
    Failed(String),
}

fn interpret(stop: &Stop, blocks: Vec<Block>) -> Outcome {
    let has_tool_use = blocks
        .iter()
        .any(|block| matches!(block, Block::ToolUse { .. }));
    let text = |blocks: &[Block]| {
        blocks
            .iter()
            .filter_map(|block| match block {
                Block::Text { text } => Some(text.trim()),
                _ => None,
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let unexpected = |reason: Option<&str>| {
        Outcome::Failed(format!(
            "The model stopped unexpectedly ({}).",
            reason.unwrap_or("no reason given")
        ))
    };
    match stop {
        Stop::ToolUse if has_tool_use => {
            let mut progress = Vec::new();
            let mut calls = Vec::new();
            for block in blocks {
                match block {
                    Block::Thinking { thinking: text } | Block::Text { text } => {
                        let text = text.trim();
                        if !text.is_empty() && text != INTERRUPTED {
                            progress.push(text.to_owned());
                        }
                    }
                    Block::ToolUse { id, name, input } => calls.push((id, name, input)),
                    Block::ToolResult { .. } | Block::Other => {}
                }
            }
            Outcome::Tools { progress, calls }
        }
        // Any other stop leaves tool calls unanswered, which would make the
        // transcript invalid for a follow-up.
        _ if has_tool_use => {
            Outcome::Failed("The model stopped in the middle of an action.".to_owned())
        }
        Stop::EndTurn => {
            let answer = text(&blocks);
            Outcome::Answer(if answer.is_empty() {
                "Done.".to_owned()
            } else {
                answer
            })
        }
        Stop::MaxTokens => match text(&blocks) {
            answer if answer.is_empty() => {
                Outcome::Failed("The answer did not fit; try asking for less at once.".to_owned())
            }
            answer => Outcome::Answer(format!("{answer} …")),
        },
        Stop::Refusal => Outcome::Failed("The model declined this request.".to_owned()),
        Stop::ToolUse => unexpected(Some("tool use without a tool call")),
        Stop::Other(reason) => unexpected(reason.as_deref()),
    }
}

impl AiGateway {
    /// One supervised cycle: claim and run model calls until no job is
    /// runnable. Failures while calls are in flight are logged instead of
    /// returned, because returning would abort those calls.
    pub async fn run_jobs(&self) -> anyhow::Result<()> {
        let mut calls = JoinSet::new();
        loop {
            if let Err(error) = self.fill(&mut calls).await {
                if calls.is_empty() {
                    return Err(error.into());
                }
                tracing::warn!(?error, "AI runner could not claim jobs");
            }
            if calls.is_empty() {
                return Ok(());
            }
            tokio::select! {
                Some(joined) = calls.join_next() => {
                    if let Err(error) = joined {
                        tracing::error!(?error, "AI model call task failed");
                    }
                }
                () = self.inner.wake.notified() => {}
                () = tokio::time::sleep(TICK) => {}
            }
        }
    }

    async fn fill(&self, calls: &mut JoinSet<()>) -> Result<(), sqlx::Error> {
        self.maintain().await?;
        while calls.len() < self.config().max_concurrent_calls {
            match self.claim().await? {
                Claim::Run(claimed) => {
                    let gateway = self.clone();
                    calls.spawn(async move { gateway.run_turn(*claimed).await });
                }
                Claim::Skipped => {}
                Claim::Empty => break,
            }
        }
        Ok(())
    }

    async fn claim(&self) -> Result<Claim, sqlx::Error> {
        let mut tx = self.pool().begin().await?;
        let job = sqlx::query_as::<_, QueuedJob>(
            "SELECT j.id, j.server_id, s.owner_id, (u.account_status = 'active') AS owner_active,
                    j.conversation_id, j.model_calls, j.tool_calls, c.model, c.effort,
                    c.system_json, c.tools_json,
                    (SELECT COALESCE(SUM(octet_length(m.content_json)), 0)::bigint
                     FROM ai_messages m WHERE m.conversation_id = c.id) AS history_bytes
             FROM ai_jobs j
             JOIN ai_conversations c ON c.id = j.conversation_id
             JOIN servers s ON s.id = j.server_id
             JOIN users u ON u.id = s.owner_id
             WHERE j.state = 'queued'
             ORDER BY j.updated_at, j.id
             LIMIT 1
             FOR UPDATE OF j SKIP LOCKED",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(job) = job else {
            return Ok(Claim::Empty);
        };
        match self.admit(&mut tx, &job).await? {
            Ok(admission) => {
                let lease_token = Uuid::new_v4();
                let turn: i32 = sqlx::query_scalar(
                    "UPDATE ai_jobs
                     SET state = 'running', lease_token = $2,
                         lease_expires_at = now() + make_interval(secs => $3),
                         reserved_micros = $4,
                         reserved_until = now() + make_interval(secs => $3),
                         model_calls = model_calls + 1, updated_at = now()
                     WHERE id = $1
                     RETURNING model_calls",
                )
                .bind(job.id)
                .bind(lease_token)
                .bind(LEASE_SECONDS)
                .bind(admission.reserved_micros)
                .fetch_one(&mut *tx)
                .await?;
                let messages: Vec<(String, String)> = sqlx::query_as(
                    "SELECT role, content_json FROM ai_messages
                     WHERE conversation_id = $1 ORDER BY seq",
                )
                .bind(job.conversation_id)
                .fetch_all(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(Claim::Run(Box::new(Claimed {
                    job_id: job.id,
                    server_id: job.server_id,
                    owner_id: job.owner_id,
                    conversation_id: job.conversation_id,
                    lease_token,
                    turn,
                    tool_calls: job.tool_calls,
                    model: admission.model,
                    effort: admission.effort,
                    max_tokens: admission.max_tokens,
                    system_json: job.system_json,
                    tools_json: job.tools_json,
                    messages,
                })))
            }
            Err(message) => {
                transitions::abort(&mut tx, job.id, AiJobState::Failed, &message).await?;
                tx.commit().await?;
                Ok(Claim::Skipped)
            }
        }
    }

    /// Decide whether a queued job may make its next model call and size that
    /// call's worst case. `Err` carries the reason shown to the operator.
    async fn admit(
        &self,
        tx: &mut Tx<'_>,
        job: &QueuedJob,
    ) -> Result<Result<Admission, String>, sqlx::Error> {
        let effective = self.effective(&settings::load(&mut **tx, job.server_id).await?);
        let catalog = self.catalog();
        let Some(model) = catalog.offered(&job.model) else {
            return Ok(Err(
                "This conversation's model is no longer offered; start a new one with /ai new."
                    .to_owned(),
            ));
        };
        if !job.owner_active {
            return Ok(Err("The server owner's account is suspended.".to_owned()));
        }
        if !effective.enabled {
            return Ok(Err(
                "The AI assistant is turned off for this server.".to_owned()
            ));
        }
        if job.model_calls >= MAX_MODEL_CALLS {
            return Ok(Err(
                "This request needed too many steps and was stopped.".to_owned()
            ));
        }
        budget::lock_account(tx, job.owner_id).await?;
        let budgets = Budgets::load(
            tx,
            job.server_id,
            job.owner_id,
            effective.server_budget_micros,
            effective.account_budget_micros,
        )
        .await?;
        let (available, cap) = budgets.available();
        let prompt_bytes = job.history_bytes
            + job.system_json.len() as i64
            + job.tools_json.len() as i64
            + REQUEST_OVERHEAD_BYTES;
        let prompt_tokens = u64::try_from(prompt_bytes / BYTES_PER_TOKEN).unwrap_or(u64::MAX);
        let max_tokens =
            catalog.affordable_max_tokens(model, prompt_tokens, available, MAX_OUTPUT_TOKENS);
        if max_tokens < MIN_OUTPUT_TOKENS {
            return Ok(Err(match cap {
                Exhausted::Server => {
                    "This server's AI budget for this month is used up; the owner can raise it in the panel."
                }
                Exhausted::Account => "The owner's AI budget for this month is used up.",
            }
            .to_owned()));
        }
        Ok(Ok(Admission {
            model: model.clone(),
            effort: AiEffort::parse(&job.effort).unwrap_or(effective.effort),
            max_tokens,
            reserved_micros: catalog.worst_case_micros(model, prompt_tokens, max_tokens),
        }))
    }

    async fn run_turn(&self, claimed: Claimed) {
        let job_id = claimed.job_id;
        let response = match self.call_model(&claimed).await {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(%job_id, error = %error, "AI model call failed");
                let message = if error
                    .downcast_ref::<ProviderError>()
                    .is_some_and(ProviderError::is_capacity)
                {
                    "The AI model is busy right now; try again in a minute."
                } else {
                    "The AI request failed."
                };
                self.release_and_abort(&claimed, message).await;
                return;
            }
        };
        if let Err(error) = self.settle(&claimed, &response).await {
            // The attempts are logged so a lost ledger write can be recovered.
            tracing::error!(%job_id, ?error, attempts = ?response.attempts, "AI model response could not be recorded");
            self.release_and_abort(&claimed, "The AI gateway hit an internal error.")
                .await;
        }
    }

    async fn call_model(&self, claimed: &Claimed) -> anyhow::Result<ModelResponse> {
        let system = RawValue::from_string(claimed.system_json.clone())?;
        let tools = RawValue::from_string(claimed.tools_json.clone())?;
        let contents = claimed
            .messages
            .iter()
            .map(|(_, content)| RawValue::from_string(content.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        let request = ModelRequest {
            model: &claimed.model,
            max_tokens: claimed.max_tokens,
            effort: claimed.effort,
            system: &system,
            tools: &tools,
            messages: claimed
                .messages
                .iter()
                .zip(&contents)
                .map(|((role, _), content)| Turn { role, content })
                .collect(),
        };
        Ok(self.client().complete(&request).await?)
    }

    /// Whether this runner still owns the job's model call.
    async fn holds_lease(tx: &mut Tx<'_>, claimed: &Claimed) -> Result<bool, sqlx::Error> {
        let lease: Option<Option<Uuid>> = sqlx::query_scalar(
            "SELECT lease_token FROM ai_jobs WHERE id = $1 AND state = 'running' FOR UPDATE",
        )
        .bind(claimed.job_id)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(lease.flatten() == Some(claimed.lease_token))
    }

    async fn settle(&self, claimed: &Claimed, response: &ModelResponse) -> Result<(), sqlx::Error> {
        let outcome = match response.blocks() {
            Ok(blocks) => interpret(&response.stop, blocks),
            Err(error) => {
                tracing::warn!(job_id = %claimed.job_id, %error, "AI response content is unreadable");
                Outcome::Failed("The AI response could not be read.".to_owned())
            }
        };
        let mut tx = self.pool().begin().await?;
        // The call happened, so it is charged even if the job was cancelled
        // or timed out meanwhile.
        let cost = budget::record_usage(
            &mut tx,
            self.catalog(),
            claimed.owner_id,
            claimed.server_id,
            claimed.job_id,
            &response.attempts,
        )
        .await?;
        sqlx::query(
            "UPDATE ai_jobs
             SET cost_micros = cost_micros + $2, reserved_micros = 0, reserved_until = NULL
             WHERE id = $1",
        )
        .bind(claimed.job_id)
        .bind(cost)
        .execute(&mut *tx)
        .await?;
        if !Self::holds_lease(&mut tx, claimed).await? {
            tx.commit().await?;
            return Ok(());
        }
        transitions::append_message(
            &mut tx,
            claimed.conversation_id,
            "assistant",
            response.content.get(),
        )
        .await?;
        let requeued = match outcome {
            Outcome::Tools { progress, calls } => {
                self.start_tool_round(&mut tx, claimed, progress, calls)
                    .await?
            }
            Outcome::Answer(answer) => {
                sqlx::query(
                    "UPDATE ai_jobs
                     SET state = 'succeeded', answer = $2, finished_at = now(), updated_at = now(),
                         lease_token = NULL, lease_expires_at = NULL
                     WHERE id = $1",
                )
                .bind(claimed.job_id)
                .bind(&answer)
                .execute(&mut *tx)
                .await?;
                transitions::insert_event(
                    &mut tx,
                    claimed.job_id,
                    claimed.server_id,
                    AiJobEventKind::Answer,
                    &answer,
                )
                .await?;
                false
            }
            Outcome::Failed(message) => {
                transitions::abort(&mut tx, claimed.job_id, AiJobState::Failed, &message).await?;
                false
            }
        };
        tx.commit().await?;
        if requeued {
            self.wake();
        }
        Ok(())
    }

    /// Record the requested tool calls; refused calls are answered at once.
    /// Returns `true` when no call needs the plugin and the job is queued
    /// again.
    async fn start_tool_round(
        &self,
        tx: &mut Tx<'_>,
        claimed: &Claimed,
        progress: Vec<String>,
        calls: Vec<(String, String, serde_json::Value)>,
    ) -> Result<bool, sqlx::Error> {
        let count = i32::try_from(calls.len()).unwrap_or(i32::MAX);
        if claimed.tool_calls.saturating_add(count) > MAX_TOOL_CALLS {
            transitions::abort(
                tx,
                claimed.job_id,
                AiJobState::Failed,
                "This request needed too many actions and was stopped.",
            )
            .await?;
            return Ok(false);
        }
        for line in progress {
            transitions::insert_event(
                tx,
                claimed.job_id,
                claimed.server_id,
                AiJobEventKind::Progress,
                &line,
            )
            .await?;
        }
        let mut pending = 0;
        for (index, (call_id, name, input)) in calls.into_iter().enumerate() {
            let refusal = prompt::check_tool_call(&name, &input).err();
            pending += usize::from(refusal.is_none());
            sqlx::query(
                "INSERT INTO ai_tool_calls(job_id, call_id, server_id, turn, seq, name, input_json,
                                           state, output, is_error, completed_at)
                 VALUES($1, $2, $3, $4, $5, $6, $7,
                        CASE WHEN $8::text IS NULL THEN 'pending' ELSE 'completed' END,
                        $8, $8 IS NOT NULL,
                        CASE WHEN $8::text IS NULL THEN NULL ELSE now() END)",
            )
            .bind(claimed.job_id)
            .bind(&call_id)
            .bind(claimed.server_id)
            .bind(claimed.turn)
            .bind(claimed.tool_calls + index as i32)
            .bind(name.chars().take(64).collect::<String>())
            .bind(input.to_string())
            .bind(refusal)
            .execute(&mut **tx)
            .await?;
        }
        sqlx::query(
            "UPDATE ai_jobs
             SET state = 'awaiting_tools', tool_calls = tool_calls + $2, updated_at = now(),
                 lease_token = NULL, lease_expires_at = NULL
             WHERE id = $1",
        )
        .bind(claimed.job_id)
        .bind(count)
        .execute(&mut **tx)
        .await?;
        if pending == 0 {
            return transitions::advance_if_complete(tx, claimed.job_id).await;
        }
        Ok(false)
    }

    async fn release_and_abort(&self, claimed: &Claimed, message: &str) {
        let result = async {
            let mut tx = self.pool().begin().await?;
            sqlx::query(
                "UPDATE ai_jobs SET reserved_micros = 0, reserved_until = NULL WHERE id = $1",
            )
            .bind(claimed.job_id)
            .execute(&mut *tx)
            .await?;
            if Self::holds_lease(&mut tx, claimed).await? {
                transitions::abort(&mut tx, claimed.job_id, AiJobState::Failed, message).await?;
            }
            tx.commit().await
        }
        .await;
        if let Err(error) = result {
            // The lease expires and the sweep requeues or fails the job.
            tracing::error!(job_id = %claimed.job_id, ?error, "AI job could not be failed");
        }
    }

    async fn maintain(&self) -> Result<(), sqlx::Error> {
        let (sweep, retention) = {
            let mut maintenance = self.inner.maintenance.lock().await;
            let now = Instant::now();
            let due = |last: Option<Instant>, interval| {
                last.is_none_or(|last| now.duration_since(last) >= interval)
            };
            let sweep = due(maintenance.last_sweep, SWEEP_INTERVAL);
            let retention = due(maintenance.last_retention, RETENTION_INTERVAL);
            if sweep {
                maintenance.last_sweep = Some(now);
            }
            if retention {
                maintenance.last_retention = Some(now);
            }
            (sweep, retention)
        };
        if sweep {
            self.sweep().await?;
        }
        if retention {
            self.retain().await?;
        }
        Ok(())
    }

    async fn sweep(&self) -> Result<(), sqlx::Error> {
        let pool = self.pool();
        let waiting: Vec<Uuid> = sqlx::query_scalar(
            "WITH timed_out AS (
                 UPDATE ai_tool_calls
                 SET state = 'completed', is_error = true, completed_at = now(),
                     output = CASE state WHEN 'pending' THEN $1 ELSE $2 END
                 WHERE state <> 'completed'
                   AND created_at < now() - make_interval(secs => $3)
                 RETURNING job_id
             )
             SELECT DISTINCT job_id FROM timed_out",
        )
        .bind("The server did not pick up this action in time, so it did not run.")
        .bind(
            "The server did not return this tool's result in time; the action may or may not have run.",
        )
        .bind(TOOL_RESULT_TIMEOUT_SECONDS)
        .fetch_all(pool)
        .await?;
        let mut requeued = false;
        for job_id in waiting {
            let mut tx = pool.begin().await?;
            requeued |= transitions::advance_if_complete(&mut tx, job_id).await?;
            tx.commit().await?;
        }

        let mut tx = pool.begin().await?;
        let expired: Vec<(Uuid, i32)> = sqlx::query_as(
            "SELECT id, stalls FROM ai_jobs
             WHERE state = 'running' AND lease_expires_at < now()
             FOR UPDATE SKIP LOCKED",
        )
        .fetch_all(&mut *tx)
        .await?;
        for (job_id, stalls) in expired {
            if stalls + 1 >= MAX_STALLS {
                transitions::abort(
                    &mut tx,
                    job_id,
                    AiJobState::Failed,
                    "The AI gateway stopped while working on this request.",
                )
                .await?;
            } else {
                sqlx::query(
                    "UPDATE ai_jobs
                     SET state = 'queued', stalls = stalls + 1, updated_at = now(),
                         lease_token = NULL, lease_expires_at = NULL
                     WHERE id = $1",
                )
                .bind(job_id)
                .execute(&mut *tx)
                .await?;
                requeued = true;
            }
        }
        let overdue: Vec<Uuid> = sqlx::query_scalar(&format!(
            "SELECT id FROM ai_jobs
             WHERE state IN {ACTIVE_STATES} AND deadline_at < now()
             FOR UPDATE SKIP LOCKED"
        ))
        .fetch_all(&mut *tx)
        .await?;
        for job_id in overdue {
            transitions::abort(
                &mut tx,
                job_id,
                AiJobState::Failed,
                "This request took too long and was stopped.",
            )
            .await?;
        }
        tx.commit().await?;
        if requeued {
            self.wake();
        }
        Ok(())
    }

    async fn retain(&self) -> Result<(), sqlx::Error> {
        let conversations = sqlx::query(&format!(
            "DELETE FROM ai_conversations c
             WHERE c.updated_at < now() - make_interval(days => $1)
               AND NOT EXISTS (SELECT 1 FROM ai_jobs j
                               WHERE j.conversation_id = c.id AND j.state IN {ACTIVE_STATES})"
        ))
        .bind(CONVERSATION_RETENTION_DAYS)
        .execute(self.pool())
        .await?;
        let usage = sqlx::query(
            "DELETE FROM ai_usage WHERE created_at < now() - make_interval(days => $1)",
        )
        .bind(USAGE_RETENTION_DAYS)
        .execute(self.pool())
        .await?;
        if conversations.rows_affected() > 0 || usage.rows_affected() > 0 {
            tracing::info!(
                conversations = conversations.rows_affected(),
                usage_rows = usage.rows_affected(),
                "AI retention removed expired records"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn blocks(value: serde_json::Value) -> Vec<Block> {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn tool_rounds_carry_progress_and_calls_in_order() {
        let outcome = interpret(
            &Stop::ToolUse,
            blocks(json!([
                {"type": "thinking", "thinking": "", "signature": "s1"},
                {"type": "thinking", "thinking": "Checking who is online.", "signature": "s2"},
                {"type": "tool_use", "id": "toolu_1", "name": "list_players", "input": {}},
                {"type": "tool_use", "id": "toolu_2", "name": "list_worlds", "input": {}},
            ])),
        );
        assert_eq!(
            outcome,
            Outcome::Tools {
                progress: vec!["Checking who is online.".to_owned()],
                calls: vec![
                    ("toolu_1".to_owned(), "list_players".to_owned(), json!({})),
                    ("toolu_2".to_owned(), "list_worlds".to_owned(), json!({})),
                ],
            }
        );
    }

    #[test]
    fn answers_join_text_and_failures_explain_themselves() {
        assert_eq!(
            interpret(
                &Stop::EndTurn,
                blocks(json!([
                    {"type": "thinking", "thinking": "", "signature": "s"},
                    {"type": "text", "text": "It is day now. "},
                    {"type": "text", "text": "Enjoy."},
                ]))
            ),
            Outcome::Answer("It is day now.\nEnjoy.".to_owned())
        );
        assert_eq!(
            interpret(&Stop::EndTurn, Vec::new()),
            Outcome::Answer("Done.".to_owned())
        );
        assert!(matches!(
            interpret(&Stop::Refusal, Vec::new()),
            Outcome::Failed(_)
        ));
        assert!(matches!(
            interpret(
                &Stop::MaxTokens,
                blocks(
                    json!([{"type": "tool_use", "id": "t", "name": "list_players", "input": {}}])
                )
            ),
            Outcome::Failed(_)
        ));
        assert_eq!(
            interpret(
                &Stop::MaxTokens,
                blocks(json!([{"type": "text", "text": "Partial"}]))
            ),
            Outcome::Answer("Partial …".to_owned())
        );
        assert_eq!(
            interpret(&Stop::Other(None), Vec::new()),
            Outcome::Failed("The model stopped unexpectedly (no reason given).".to_owned())
        );
        assert!(matches!(
            interpret(&Stop::ToolUse, Vec::new()),
            Outcome::Failed(_)
        ));
    }

    #[test]
    fn interrupted_progress_markers_are_not_shown() {
        let outcome = interpret(
            &Stop::ToolUse,
            blocks(json!([
                {"type": "thinking", "thinking": INTERRUPTED, "signature": "s"},
                {"type": "tool_use", "id": "t", "name": "list_players", "input": {}},
            ])),
        );
        assert!(matches!(outcome, Outcome::Tools { progress, .. } if progress.is_empty()));
    }
}
