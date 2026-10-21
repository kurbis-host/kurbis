use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    contracts::console::{ConsoleCommandRequest, ConsoleCommandResponse},
    model::ConsoleCommandRecord,
};

use super::{
    ApiError, AppState,
    actor::{ActorContext, ApiBrowserServerHeaders, ApiJson, ApiPath, ServerContext},
    internal,
};

const CONSOLE_COMMAND_SELECT: &str =
    "SELECT id, server_id, request_id, worker_id, generation, attempt, command,
       status, output, error, claimed_at, completed_at, created_at, updated_at
    FROM console_commands";

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct ServerConsoleCommandPath {
    server_id: Uuid,
    command_id: Uuid,
}

#[derive(Clone, Copy)]
enum ConsoleActorFence {
    Owner(Uuid),
    OwnerOrServerSession {
        user_id: Uuid,
        grant_id: Uuid,
        session_id: Uuid,
    },
    ServerSession {
        grant_id: Uuid,
        session_id: Uuid,
    },
}

pub(super) async fn enqueue_console_command(
    State(state): State<Arc<AppState>>,
    actor: aide::NoApi<ActorContext>,
    server: ServerContext,
    ApiJson(request): ApiJson<ConsoleCommandRequest>,
) -> Result<(StatusCode, Json<ConsoleCommandResponse>), ApiError> {
    let server_id = server.id;
    let request_id = request.request_id;
    let command = validate_console_command(request.command)?;
    let mut tx = state.pool.begin().await?;
    let authority = resolve_console_actor(actor.0, &server)?;
    authorize_console_actor(&mut tx, server_id, authority).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("kurbis-console-request:{server_id}:{request_id}"))
        .execute(&mut *tx)
        .await?;

    // Request IDs make retries safe after a lost HTTP response. Authorization
    // is rechecked first, and reuse with a different command is a conflict.
    let existing = sqlx::query_as::<_, ConsoleCommandRecord>(&format!(
        "{CONSOLE_COMMAND_SELECT} WHERE server_id = $1 AND request_id = $2 FOR UPDATE"
    ))
    .bind(server_id)
    .bind(request_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(existing) = existing {
        if existing.command != command {
            return Err(ApiError::with_code(
                StatusCode::CONFLICT,
                "console_request_id_conflict",
                "request_id was already used for a different console command",
            ));
        }
        tx.commit().await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(ConsoleCommandResponse::from(existing)),
        ));
    }

    let Some((generation, worker_id)) = online_assignment(&mut tx, server_id, authority).await?
    else {
        return Err(ApiError::with_code(
            StatusCode::CONFLICT,
            "console_unavailable",
            "console commands require an online server with a healthy assigned worker",
        ));
    };
    let row = sqlx::query_as::<_, ConsoleCommandRecord>(
        "WITH created AS (
            INSERT INTO console_commands(server_id, request_id, worker_id, generation, command)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id, server_id, request_id, worker_id, generation, attempt, command, status,
                      output, error, claimed_at, completed_at, created_at, updated_at
        )
        SELECT id, server_id, request_id, worker_id, generation, attempt, command, status,
               output, error, claimed_at, completed_at, created_at, updated_at
        FROM created",
    )
    .bind(server_id)
    .bind(request_id)
    .bind(worker_id)
    .bind(generation)
    .bind(command)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(ConsoleCommandResponse::from(row)),
    ))
}

pub(super) async fn get_console_command(
    State(state): State<Arc<AppState>>,
    headers: ApiBrowserServerHeaders,
    ApiPath(path): ApiPath<ServerConsoleCommandPath>,
) -> Result<Json<ConsoleCommandResponse>, ApiError> {
    let server_id = path.server_id;
    internal::require_browser_server_view(&state, &headers, server_id).await?;
    let row = sqlx::query_as::<_, ConsoleCommandRecord>(&format!(
        "{CONSOLE_COMMAND_SELECT} WHERE server_id = $1 AND id = $2"
    ))
    .bind(server_id)
    .bind(path.command_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::server_not_found)?;
    Ok(Json(ConsoleCommandResponse::from(row)))
}

fn resolve_console_actor(
    actor: ActorContext,
    server: &ServerContext,
) -> Result<ConsoleActorFence, ApiError> {
    match actor {
        ActorContext::Owner(user) => Ok(ConsoleActorFence::Owner(user.id)),
        ActorContext::OwnerWithServerSessions { user, sessions } => {
            let scope = sessions.iter().find(|scope| scope.server_id == server.id);
            Ok(scope.map_or(ConsoleActorFence::Owner(user.id), |scope| {
                ConsoleActorFence::OwnerOrServerSession {
                    user_id: user.id,
                    grant_id: scope.grant_id,
                    session_id: scope.session_id,
                }
            }))
        }
        ActorContext::ServerSessions(sessions) => {
            let Some(scope) = sessions.iter().find(|scope| scope.server_id == server.id) else {
                return Err(ApiError::server_not_found());
            };
            Ok(ConsoleActorFence::ServerSession {
                grant_id: scope.grant_id,
                session_id: scope.session_id,
            })
        }
        ActorContext::ServerCapability { .. }
        | ActorContext::Worker { .. }
        | ActorContext::Gateway => Err(ApiError::with_code(
            StatusCode::FORBIDDEN,
            "console_command_forbidden",
            "this actor cannot submit server console commands",
        )),
    }
}

async fn authorize_console_actor(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    server_id: Uuid,
    actor: ConsoleActorFence,
) -> Result<(), ApiError> {
    let owner_id: Option<Uuid> =
        sqlx::query_scalar("SELECT owner_id FROM servers WHERE id = $1 FOR UPDATE")
            .bind(server_id)
            .fetch_optional(&mut **tx)
            .await?;
    let Some(owner_id) = owner_id else {
        return Err(ApiError::server_not_found());
    };
    let valid = match actor {
        ConsoleActorFence::Owner(user_id) if user_id == owner_id => {
            user_is_active(tx, user_id).await?
        }
        ConsoleActorFence::OwnerOrServerSession { user_id, .. } if user_id == owner_id => {
            user_is_active(tx, user_id).await?
        }
        ConsoleActorFence::OwnerOrServerSession {
            grant_id,
            session_id,
            ..
        }
        | ConsoleActorFence::ServerSession {
            grant_id,
            session_id,
        } => valid_server_session(tx, server_id, grant_id, session_id).await?,
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(ApiError::server_not_found())
    }
}

async fn user_is_active(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
) -> Result<bool, ApiError> {
    let active: Option<bool> =
        sqlx::query_scalar("SELECT account_status = 'active' FROM users WHERE id = $1 FOR SHARE")
            .bind(user_id)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(active == Some(true))
}

async fn valid_server_session(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    server_id: Uuid,
    grant_id: Uuid,
    session_id: Uuid,
) -> Result<bool, ApiError> {
    let valid: Option<Uuid> = sqlx::query_scalar(
        "SELECT ss.id
         FROM server_sessions ss
         JOIN server_session_grants g ON g.id = ss.grant_id
         JOIN servers s ON s.id = ss.server_id
         JOIN users u ON u.id = s.owner_id
         WHERE ss.id = $1 AND ss.grant_id = $2 AND ss.server_id = $3
           AND s.desired_state <> 'deleted' AND u.account_status = 'active'
         FOR SHARE OF ss, g, s, u",
    )
    .bind(session_id)
    .bind(grant_id)
    .bind(server_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(valid == Some(session_id))
}

async fn online_assignment(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    server_id: Uuid,
    actor: ConsoleActorFence,
) -> Result<Option<(i64, Uuid)>, ApiError> {
    let assignment = match actor {
        ConsoleActorFence::Owner(user_id) => {
            owner_online_assignment(tx, server_id, user_id).await?
        }
        ConsoleActorFence::OwnerOrServerSession {
            user_id,
            grant_id,
            session_id,
        } => {
            if let Some(assignment) = owner_online_assignment(tx, server_id, user_id).await? {
                Some(assignment)
            } else {
                session_online_assignment(tx, server_id, grant_id, session_id).await?
            }
        }
        ConsoleActorFence::ServerSession {
            grant_id,
            session_id,
        } => session_online_assignment(tx, server_id, grant_id, session_id).await?,
    };
    Ok(assignment)
}

async fn owner_online_assignment(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    server_id: Uuid,
    user_id: Uuid,
) -> Result<Option<(i64, Uuid)>, ApiError> {
    Ok(sqlx::query_as::<_, (i64, Uuid)>(
        "SELECT s.generation, p.worker_id
         FROM servers s
         JOIN users u ON u.id = s.owner_id AND u.account_status = 'active'
         JOIN placements p ON p.server_id = s.id AND p.generation = s.generation AND p.active
         JOIN workers w ON w.id = p.worker_id
         JOIN worker_credentials c ON c.worker_id = w.id AND c.revoked_at IS NULL
           AND c.lease_id IS NOT NULL AND c.lease_expires_at > now()
         WHERE s.id = $1 AND u.id = $2 AND s.desired_state = 'running'
           AND s.observed_state = 'online' AND w.heartbeat_at > now() - interval '30 seconds'
         FOR UPDATE OF s, p, u",
    )
    .bind(server_id)
    .bind(user_id)
    .fetch_optional(&mut **tx)
    .await?)
}

async fn session_online_assignment(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    server_id: Uuid,
    grant_id: Uuid,
    session_id: Uuid,
) -> Result<Option<(i64, Uuid)>, ApiError> {
    Ok(sqlx::query_as::<_, (i64, Uuid)>(
        "SELECT s.generation, p.worker_id
         FROM servers s
         JOIN users u ON u.id = s.owner_id AND u.account_status = 'active'
         JOIN server_sessions ss ON ss.server_id = s.id
         JOIN server_session_grants g ON g.id = ss.grant_id
         JOIN placements p ON p.server_id = s.id AND p.generation = s.generation AND p.active
         JOIN workers w ON w.id = p.worker_id
         JOIN worker_credentials c ON c.worker_id = w.id AND c.revoked_at IS NULL
           AND c.lease_id IS NOT NULL AND c.lease_expires_at > now()
         WHERE s.id = $1 AND ss.id = $2 AND ss.grant_id = $3
           AND s.desired_state = 'running' AND s.observed_state = 'online'
           AND w.heartbeat_at > now() - interval '30 seconds'
         FOR UPDATE OF s, p, ss, g, u",
    )
    .bind(server_id)
    .bind(session_id)
    .bind(grant_id)
    .fetch_optional(&mut **tx)
    .await?)
}

impl From<ConsoleCommandRecord> for ConsoleCommandResponse {
    fn from(record: ConsoleCommandRecord) -> Self {
        Self {
            id: record.id,
            server_id: record.server_id,
            request_id: record.request_id,
            generation: record.generation,
            attempt: record.attempt,
            command: record.command,
            status: record.status,
            output: record.output,
            error: record.error,
            claimed_at: record.claimed_at,
            completed_at: record.completed_at,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

fn validate_console_command(command: String) -> Result<String, ApiError> {
    let trimmed = command.trim();
    let normalized = trimmed.trim_start_matches('/').trim();
    if normalized.is_empty() || normalized.len() > 512 || command.chars().any(char::is_control) {
        return Err(ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_console_command",
            "command must be 1–512 bytes and contain no control characters",
        ));
    }
    Ok(normalized.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_commands_are_trimmed_bounded_and_reject_control_characters() {
        assert_eq!(
            validate_console_command("  //say hello  ".into()).unwrap(),
            "say hello"
        );
        for invalid in ["".to_owned(), " \t ".to_owned(), "say\nhello".to_owned()] {
            assert!(validate_console_command(invalid).is_err());
        }
        assert!(validate_console_command("x".repeat(513)).is_err());
    }
}
