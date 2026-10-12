//! Job state transitions shared by the HTTP handlers, the plugin sync and the
//! runner. Every function runs inside the caller's transaction.

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::contracts::ai::{AiJobEventKind, AiJobState};

use super::prompt;

pub(super) type Tx<'a> = Transaction<'a, Postgres>;

/// Longest chat line delivered to an operator, in characters.
const MAX_EVENT_CHARS: usize = 4000;

pub(super) const ACTIVE_STATES: &str = "('queued', 'running', 'awaiting_tools')";

/// Append one turn to a conversation transcript.
pub(super) async fn append_message(
    tx: &mut Tx<'_>,
    conversation_id: Uuid,
    role: &str,
    content_json: &str,
) -> Result<(), sqlx::Error> {
    let seq: i32 = sqlx::query_scalar(
        "UPDATE ai_conversations
         SET message_count = message_count + 1, updated_at = now()
         WHERE id = $1
         RETURNING message_count - 1",
    )
    .bind(conversation_id)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO ai_messages(conversation_id, seq, role, content_json)
         VALUES($1, $2, $3, $4)",
    )
    .bind(conversation_id)
    .bind(seq)
    .bind(role)
    .bind(content_json)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Queue a chat line for the job's operator.
///
/// Event ids are the plugin's read cursor, so writers serialize per server
/// until commit: a reader can then never observe a later id before an
/// earlier one becomes visible.
pub(super) async fn insert_event(
    tx: &mut Tx<'_>,
    job_id: Uuid,
    server_id: Uuid,
    kind: AiJobEventKind,
    message: &str,
) -> Result<(), sqlx::Error> {
    let message = message.trim();
    if message.is_empty() {
        return Ok(());
    }
    let message = if message.chars().count() > MAX_EVENT_CHARS {
        let mut shortened = message
            .chars()
            .take(MAX_EVENT_CHARS - 1)
            .collect::<String>();
        shortened.push('…');
        shortened
    } else {
        message.to_owned()
    };
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("kurbis-ai-events:{server_id}"))
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "INSERT INTO ai_job_events(job_id, server_id, kind, message)
         VALUES($1, $2, $3, $4)",
    )
    .bind(job_id)
    .bind(server_id)
    .bind(kind.as_str())
    .bind(message)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// End an active job as failed or cancelled. Its open tool calls are closed,
/// its conversation stops accepting follow-ups (the transcript may end in an
/// unanswered tool call) and the operator is told why. Returns `false` when
/// the job had already finished.
pub(super) async fn abort(
    tx: &mut Tx<'_>,
    job_id: Uuid,
    state: AiJobState,
    message: &str,
) -> Result<bool, sqlx::Error> {
    debug_assert!(matches!(state, AiJobState::Failed | AiJobState::Cancelled));
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(&format!(
        "UPDATE ai_jobs
         SET state = $2, error = $3, finished_at = now(), updated_at = now(),
             lease_token = NULL, lease_expires_at = NULL
         WHERE id = $1 AND state IN {ACTIVE_STATES}
         RETURNING server_id, conversation_id"
    ))
    .bind(job_id)
    .bind(state.as_str())
    .bind(message)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((server_id, conversation_id)) = row else {
        return Ok(false);
    };
    sqlx::query(
        "UPDATE ai_conversations SET closed_at = now() WHERE id = $1 AND closed_at IS NULL",
    )
    .bind(conversation_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE ai_tool_calls
         SET state = 'completed', output = $2, is_error = true, completed_at = now()
         WHERE job_id = $1 AND state <> 'completed'",
    )
    .bind(job_id)
    .bind(message)
    .execute(&mut **tx)
    .await?;
    insert_event(tx, job_id, server_id, AiJobEventKind::Error, message).await?;
    Ok(true)
}

/// When every tool call of the job's current turn has a result, answer them
/// in one user turn and queue the job for its next model call. The job row
/// lock makes a concurrent sync and sweep advance a turn only once.
pub(super) async fn advance_if_complete(
    tx: &mut Tx<'_>,
    job_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let job: Option<(Uuid, i32)> = sqlx::query_as(
        "SELECT conversation_id, model_calls FROM ai_jobs
         WHERE id = $1 AND state = 'awaiting_tools'
         FOR UPDATE",
    )
    .bind(job_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((conversation_id, turn)) = job else {
        return Ok(false);
    };
    let calls: Vec<(String, Option<String>, bool)> = sqlx::query_as(
        "SELECT call_id, output, is_error FROM ai_tool_calls
         WHERE job_id = $1 AND turn = $2
         ORDER BY seq",
    )
    .bind(job_id)
    .bind(turn)
    .fetch_all(&mut **tx)
    .await?;
    let mut results = Vec::with_capacity(calls.len());
    for (call_id, output, is_error) in calls {
        let Some(output) = output else {
            return Ok(false);
        };
        results.push(prompt::tool_result(&call_id, &output, is_error));
    }
    append_message(
        tx,
        conversation_id,
        "user",
        &serde_json::Value::Array(results).to_string(),
    )
    .await?;
    sqlx::query(
        "UPDATE ai_jobs SET state = 'queued', updated_at = now()
         WHERE id = $1 AND state = 'awaiting_tools'",
    )
    .bind(job_id)
    .execute(&mut **tx)
    .await?;
    Ok(true)
}

/// Lock a job row for a state change.
pub(super) async fn lock_job(
    tx: &mut Tx<'_>,
    job_id: Uuid,
    server_id: Uuid,
) -> Result<Option<AiJobState>, sqlx::Error> {
    let state: Option<String> =
        sqlx::query_scalar("SELECT state FROM ai_jobs WHERE id = $1 AND server_id = $2 FOR UPDATE")
            .bind(job_id)
            .bind(server_id)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(state.as_deref().and_then(AiJobState::parse))
}
