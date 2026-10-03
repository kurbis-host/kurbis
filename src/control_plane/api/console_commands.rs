use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    contracts::{console::ConsoleCommandCompletion, worker::WorkerConsoleCommand},
    model::ConsoleCommandStatus,
};

use super::{ApiError, AppState, worker_auth};

const MAX_OUTPUT_LINES: usize = 100;
const MAX_OUTPUT_BYTES: usize = 16 * 1024;
const MAX_ERROR_BYTES: usize = 1_000;

#[derive(Debug, Serialize, sqlx::FromRow)]
struct ClaimedConsoleCommand {
    id: Uuid,
    server_id: Uuid,
    generation: i64,
    attempt: i32,
    command: String,
}

pub(super) async fn claim_console_command(
    State(state): State<std::sync::Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Option<WorkerConsoleCommand>>, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("kurbis-worker-console-claim:{worker_id}"))
        .execute(&mut *tx)
        .await?;

    let command = sqlx::query_as::<_, ClaimedConsoleCommand>(
        "SELECT c.id, c.server_id, c.generation, c.attempt, c.command
         FROM console_commands c
         JOIN servers s ON s.id = c.server_id AND s.generation = c.generation
         JOIN placements p ON p.server_id = s.id AND p.generation = s.generation
           AND p.worker_id = $1 AND p.active
         JOIN workers w ON w.id = p.worker_id AND w.heartbeat_at > now() - interval '30 seconds'
         WHERE c.worker_id = $1 AND c.status = 'queued'
           AND s.desired_state = 'running' AND s.observed_state = 'online'
         ORDER BY c.created_at, c.id
         FOR UPDATE OF c SKIP LOCKED
         LIMIT 1",
    )
    .bind(worker_id)
    .fetch_optional(&mut *tx)
    .await?;
    let result = if let Some(mut command) = command {
        let attempt = sqlx::query_scalar::<_, i32>(
            "UPDATE console_commands SET status = 'running', attempt = attempt + 1,
                    claimed_at = now(), updated_at = now()
             WHERE id = $1 AND worker_id = $2 AND status = 'queued'
             RETURNING attempt",
        )
        .bind(command.id)
        .bind(worker_id)
        .fetch_one(&mut *tx)
        .await?;
        command.attempt = attempt;
        Some(WorkerConsoleCommand {
            id: command.id,
            server_id: command.server_id,
            generation: command.generation,
            attempt,
            command: command.command,
        })
    } else {
        None
    };

    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    tx.commit().await?;
    Ok(Json(result))
}

pub(super) async fn complete_console_command(
    State(state): State<std::sync::Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(command_id): Path<Uuid>,
    Json(completion): Json<ConsoleCommandCompletion>,
) -> Result<StatusCode, ApiError> {
    validate_completion(&completion)?;
    if completion.generation <= 0 || completion.attempt <= 0 {
        return Err(invalid_completion());
    }
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let mut tx = state.pool.begin().await?;
    let locked_worker = worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await?;
    if locked_worker != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }

    let status = if completion.succeeded {
        ConsoleCommandStatus::Succeeded
    } else {
        ConsoleCommandStatus::Failed
    };
    let error = if completion.succeeded {
        None
    } else {
        Some(completion.error.as_deref().unwrap_or("command_failed"))
    };
    let result = sqlx::query(
        "UPDATE console_commands c
         SET status = $5::console_command_status, output = $6, error = $7, completed_at = now(), updated_at = now()
         WHERE c.id = $1 AND c.worker_id = $2 AND c.generation = $3
           AND c.attempt = $4 AND c.status = 'running'
           AND EXISTS (
             SELECT 1 FROM servers s
             JOIN placements p ON p.server_id = s.id AND p.generation = s.generation
               AND p.worker_id = $2 AND p.active
             WHERE s.id = c.server_id AND s.generation = c.generation
               AND s.desired_state = 'running' AND s.observed_state = 'online'
           )",
    )
    .bind(command_id)
    .bind(worker_id)
    .bind(completion.generation)
    .bind(completion.attempt)
    .bind(status)
    .bind(&completion.output)
    .bind(error)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() != 1 {
        return Err(stale_console_command());
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

fn validate_completion(completion: &ConsoleCommandCompletion) -> Result<(), ApiError> {
    let output_bytes = completion.output.iter().map(String::len).sum::<usize>();
    if completion.output.len() > MAX_OUTPUT_LINES
        || output_bytes > MAX_OUTPUT_BYTES
        || completion.output.iter().any(|line| line.len() > 4_096)
        || completion
            .error
            .as_ref()
            .is_some_and(|error| error.is_empty() || error.len() > MAX_ERROR_BYTES)
        || (!completion.succeeded && completion.error.is_none())
    {
        return Err(invalid_completion());
    }
    Ok(())
}

fn invalid_completion() -> ApiError {
    ApiError::with_code(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_console_completion",
        "worker console completion does not match the bounded result contract",
    )
}

fn stale_console_command() -> ApiError {
    ApiError::with_code(
        StatusCode::CONFLICT,
        "stale_console_command",
        "console command is no longer running for the worker's current assignment",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_output_is_bounded_and_failure_requires_error() {
        let completion = ConsoleCommandCompletion {
            generation: 1,
            attempt: 1,
            succeeded: true,
            output: vec!["ok".into()],
            error: None,
        };
        assert!(validate_completion(&completion).is_ok());
        let missing_error = ConsoleCommandCompletion {
            succeeded: false,
            ..completion.clone()
        };
        assert!(validate_completion(&missing_error).is_err());
        let too_many = ConsoleCommandCompletion {
            output: vec!["x".into(); MAX_OUTPUT_LINES + 1],
            ..completion
        };
        assert!(validate_completion(&too_many).is_err());
    }
}
