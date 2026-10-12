//! The plugin's poll: it returns tool results, receives the tool calls it must
//! run and the chat lines it must show, all in one short request.

use std::collections::BTreeMap;

use sqlx::PgPool;
use uuid::Uuid;

use crate::contracts::ai::{
    AiActiveJob, AiJobEvent, AiJobEventKind, AiJobState, AiSyncRequest, AiSyncResponse,
    AiToolCallRequest, AiToolResult,
};

use super::transitions::{self, ACTIVE_STATES};

const MAX_EVENTS_PER_SYNC: i64 = 32;

/// Apply returned results and collect new work. Returns `true` in the second
/// field when a job became ready for its next model call.
pub async fn sync(
    pool: &PgPool,
    server_id: Uuid,
    request: AiSyncRequest,
) -> Result<(AiSyncResponse, bool), sqlx::Error> {
    let mut tx = pool.begin().await?;

    let mut results_by_job = BTreeMap::<Uuid, Vec<AiToolResult>>::new();
    for result in request.results {
        results_by_job
            .entry(result.job_id)
            .or_default()
            .push(result);
    }
    let mut advanced = false;
    for (job_id, results) in results_by_job {
        // Results only count while the job still waits for them; late results
        // of a cancelled or timed-out call are dropped.
        if transitions::lock_job(&mut tx, job_id, server_id).await?
            != Some(AiJobState::AwaitingTools)
        {
            continue;
        }
        for result in results {
            sqlx::query(
                "UPDATE ai_tool_calls
                 SET state = 'completed', output = $3, is_error = $4, completed_at = now()
                 WHERE job_id = $1 AND call_id = $2 AND state = 'delivered'",
            )
            .bind(job_id)
            .bind(&result.call_id)
            .bind(&result.output)
            .bind(result.is_error)
            .execute(&mut *tx)
            .await?;
        }
        advanced |= transitions::advance_if_complete(&mut tx, job_id).await?;
    }

    // Read before calls and events: a job that finishes after this query still
    // counts as active, so the plugin syncs once more and receives its last
    // chat line instead of going idle before it.
    let active: Vec<(Uuid, Uuid, String)> = sqlx::query_as(&format!(
        "SELECT id, operator_id, state FROM ai_jobs
         WHERE server_id = $1 AND state IN {ACTIVE_STATES}
         ORDER BY created_at"
    ))
    .bind(server_id)
    .fetch_all(&mut *tx)
    .await?;

    let rows: Vec<(Uuid, String, Uuid, String, String)> = sqlx::query_as(
        "WITH delivered AS (
             UPDATE ai_tool_calls c
             SET state = 'delivered', delivered_at = now()
             FROM ai_jobs j
             WHERE c.server_id = $1 AND c.state = 'pending'
               AND j.id = c.job_id AND j.state = 'awaiting_tools'
             RETURNING c.job_id, c.call_id, j.operator_id, c.name, c.input_json, c.seq
         )
         SELECT job_id, call_id, operator_id, name, input_json
         FROM delivered ORDER BY job_id, seq",
    )
    .bind(server_id)
    .fetch_all(&mut *tx)
    .await?;
    let calls = rows
        .into_iter()
        .map(
            |(job_id, call_id, operator_id, name, input)| AiToolCallRequest {
                job_id,
                call_id,
                operator_id,
                name,
                input: serde_json::from_str(&input).unwrap_or_else(|_| serde_json::json!({})),
            },
        )
        .collect::<Vec<_>>();

    // A fresh plugin skips the lines of finished requests but replays those
    // of requests still running, which a restarted server never showed or
    // showed only in part.
    let after = match request.after_event_id {
        Some(after) => after,
        None => {
            sqlx::query_scalar(&format!(
                "SELECT COALESCE(
                     (SELECT min(e.id) - 1 FROM ai_job_events e
                      JOIN ai_jobs j ON j.id = e.job_id
                      WHERE e.server_id = $1 AND j.state IN {ACTIVE_STATES}),
                     (SELECT max(id) FROM ai_job_events WHERE server_id = $1),
                     0)"
            ))
            .bind(server_id)
            .fetch_one(&mut *tx)
            .await?
        }
    };
    let rows: Vec<(i64, Uuid, Uuid, String, String)> = sqlx::query_as(
        "SELECT e.id, e.job_id, j.operator_id, e.kind, e.message
         FROM ai_job_events e JOIN ai_jobs j ON j.id = e.job_id
         WHERE e.server_id = $1 AND e.id > $2
         ORDER BY e.id LIMIT $3",
    )
    .bind(server_id)
    .bind(after)
    .bind(MAX_EVENTS_PER_SYNC)
    .fetch_all(&mut *tx)
    .await?;
    let cursor = rows.last().map_or(after, |row| row.0);
    let events = rows
        .into_iter()
        .filter_map(|(id, job_id, operator_id, kind, message)| {
            Some(AiJobEvent {
                id,
                job_id,
                operator_id,
                kind: AiJobEventKind::parse(&kind)?,
                message,
            })
        })
        .collect();

    tx.commit().await?;

    Ok((
        AiSyncResponse {
            calls,
            events,
            cursor,
            active_jobs: active
                .into_iter()
                .filter_map(|(job_id, operator_id, state)| {
                    Some(AiActiveJob {
                        job_id,
                        operator_id,
                        state: AiJobState::parse(&state)?,
                    })
                })
                .collect(),
        },
        advanced,
    ))
}
