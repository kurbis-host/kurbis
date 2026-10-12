use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};

use crate::{
    contracts::lifecycle::{CommandResponse, ServerCommandRequest},
    control_plane::lifecycle::{LifecycleActor, LifecycleCommand},
    model::SnapshotReason,
};

use super::{
    ApiError, AppState,
    actor::{ActorContext, ApiJson, ServerContext},
    lifecycle_error,
    types::ServerSettings,
};

pub const COMMAND_ROUTE: &str = "/api/v1/servers/{server_id}/commands";

impl ServerCommandRequest {
    fn into_domain(self, authority: CommandAuthority) -> Result<LifecycleCommand, ApiError> {
        let command = match (authority, self) {
            (CommandAuthority::Gateway, ServerCommandRequest::Start { reason }) => {
                if reason.as_deref() != Some("gateway_join") {
                    return Err(ApiError::with_code(
                        StatusCode::FORBIDDEN,
                        "gateway_command_not_allowed",
                        "gateway actors may only request join starts",
                    ));
                }
                LifecycleCommand::Start {
                    reason: "gateway_join".to_owned(),
                }
            }
            (CommandAuthority::Gateway, _) => {
                return Err(ApiError::with_code(
                    StatusCode::FORBIDDEN,
                    "gateway_command_not_allowed",
                    "gateway actors may only request join starts",
                ));
            }
            (CommandAuthority::Worker, ServerCommandRequest::Stop { generation, reason }) => {
                let generation = positive_generation(generation)?;
                let reason = reason
                    .ok_or_else(|| invalid_command("worker stop commands require a reason"))?;
                if !matches!(reason.as_str(), "idle_timeout" | "storage_quota") {
                    return Err(invalid_command("worker stop reason is unsupported"));
                }
                LifecycleCommand::Stop {
                    generation: Some(generation),
                    reason,
                }
            }
            (CommandAuthority::Worker, ServerCommandRequest::Snapshot { generation, reason }) => {
                let generation = positive_generation(generation)?;
                if reason != Some(SnapshotReason::Periodic) {
                    return Err(invalid_command("worker snapshot commands must be periodic"));
                }
                LifecycleCommand::Snapshot {
                    generation: Some(generation),
                    reason: SnapshotReason::Periodic,
                }
            }
            (CommandAuthority::Worker, _) => {
                return Err(ApiError::with_code(
                    StatusCode::FORBIDDEN,
                    "worker_command_not_allowed",
                    "workers may only issue fenced stop or periodic snapshot commands",
                ));
            }
            (CommandAuthority::ServerGrant, ServerCommandRequest::Start { reason })
            | (CommandAuthority::ServerCapability, ServerCommandRequest::Start { reason }) => {
                LifecycleCommand::Start {
                    reason: manual_reason(reason)?,
                }
            }
            (CommandAuthority::ServerGrant, ServerCommandRequest::Stop { generation, reason })
            | (
                CommandAuthority::ServerCapability,
                ServerCommandRequest::Stop { generation, reason },
            ) => {
                if generation.is_some() {
                    return Err(invalid_command(
                        "server-scoped commands cannot provide a worker generation",
                    ));
                }
                LifecycleCommand::Stop {
                    generation: None,
                    reason: manual_reason(reason)?,
                }
            }
            (CommandAuthority::ServerGrant, ServerCommandRequest::Restart)
            | (CommandAuthority::ServerCapability, ServerCommandRequest::Restart) => {
                LifecycleCommand::Restart
            }
            (
                CommandAuthority::ServerGrant | CommandAuthority::ServerCapability,
                ServerCommandRequest::Snapshot { generation, reason },
            ) => {
                if generation.is_some() {
                    return Err(invalid_command(
                        "server-scoped commands cannot provide a worker generation",
                    ));
                }
                let reason = reason.unwrap_or(SnapshotReason::Manual);
                if reason != SnapshotReason::Manual {
                    return Err(invalid_command(
                        "server-scoped actors may only request manual snapshots",
                    ));
                }
                LifecycleCommand::Snapshot {
                    generation: None,
                    reason,
                }
            }
            (
                CommandAuthority::ServerGrant | CommandAuthority::ServerCapability,
                ServerCommandRequest::Restore { generation },
            ) => LifecycleCommand::Restore { generation },
            (CommandAuthority::ServerGrant, ServerCommandRequest::Reset { mode }) => {
                LifecycleCommand::Reset {
                    mode,
                    full_reset_settings: ServerSettings::default().into_stored(),
                }
            }
            (CommandAuthority::ServerGrant, ServerCommandRequest::Delete) => {
                LifecycleCommand::Delete
            }
            (CommandAuthority::ServerCapability, _) => {
                return Err(ApiError::with_code(
                    StatusCode::FORBIDDEN,
                    "server_command_not_allowed",
                    "runtime capabilities cannot issue reset or delete commands",
                ));
            }
            (CommandAuthority::AccountOwner, ServerCommandRequest::Start { reason }) => {
                LifecycleCommand::Start {
                    reason: manual_reason(reason)?,
                }
            }
            (CommandAuthority::AccountOwner, ServerCommandRequest::Stop { generation, reason }) => {
                if generation.is_some() {
                    return Err(invalid_command(
                        "owner commands cannot provide a worker generation",
                    ));
                }
                LifecycleCommand::Stop {
                    generation: None,
                    reason: manual_reason(reason)?,
                }
            }
            (CommandAuthority::AccountOwner, ServerCommandRequest::Restart) => {
                LifecycleCommand::Restart
            }
            (
                CommandAuthority::AccountOwner,
                ServerCommandRequest::Snapshot { generation, reason },
            ) => {
                if generation.is_some() {
                    return Err(invalid_command(
                        "owner commands cannot provide a worker generation",
                    ));
                }
                let reason = reason.unwrap_or(SnapshotReason::Manual);
                if reason != SnapshotReason::Manual {
                    return Err(invalid_command("owner snapshot reason is unsupported"));
                }
                LifecycleCommand::Snapshot {
                    generation: None,
                    reason,
                }
            }
            (CommandAuthority::AccountOwner, ServerCommandRequest::Reset { mode }) => {
                LifecycleCommand::Reset {
                    mode,
                    full_reset_settings: ServerSettings::default().into_stored(),
                }
            }
            (CommandAuthority::AccountOwner, ServerCommandRequest::Delete) => {
                LifecycleCommand::Delete
            }
            (CommandAuthority::AccountOwner, ServerCommandRequest::Restore { generation }) => {
                LifecycleCommand::Restore { generation }
            }
        };
        validate_command(&command)?;
        Ok(command)
    }
}

#[derive(Debug, Clone, Copy)]
enum CommandAuthority {
    AccountOwner,
    Worker,
    ServerGrant,
    ServerCapability,
    Gateway,
}

fn positive_generation(generation: Option<i64>) -> Result<i64, ApiError> {
    generation
        .filter(|generation| *generation > 0)
        .ok_or_else(|| invalid_command("worker commands require a positive generation"))
}

fn manual_reason(reason: Option<String>) -> Result<String, ApiError> {
    match reason.as_deref() {
        None | Some("manual") => Ok("manual".to_owned()),
        _ => Err(invalid_command(
            "owner and server-scoped commands use the manual reason",
        )),
    }
}

fn validate_command(command: &LifecycleCommand) -> Result<(), ApiError> {
    match command {
        LifecycleCommand::Start { reason } | LifecycleCommand::Stop { reason, .. } => {
            if reason.is_empty() || reason.len() > 64 {
                return Err(invalid_command(
                    "command reason must be between 1 and 64 bytes",
                ));
            }
        }
        LifecycleCommand::Snapshot { reason, .. }
            if *reason != SnapshotReason::Manual && *reason != SnapshotReason::Periodic =>
        {
            return Err(invalid_command("unsupported snapshot reason"));
        }
        LifecycleCommand::Restore { generation } if *generation < 1 => {
            return Err(invalid_command("snapshot generation must be positive"));
        }
        _ => {}
    }
    Ok(())
}

fn invalid_command(message: &str) -> ApiError {
    ApiError::with_code(StatusCode::UNPROCESSABLE_ENTITY, "invalid_command", message)
}

pub(super) async fn dispatch_command(
    State(state): State<Arc<AppState>>,
    actor: aide::NoApi<ActorContext>,
    server: ServerContext,
    ApiJson(request): ApiJson<ServerCommandRequest>,
) -> Result<(StatusCode, Json<CommandResponse>), ApiError> {
    let actor = actor.0;
    let server_id = server.id;
    let authority = if actor.is_gateway() {
        CommandAuthority::Gateway
    } else if actor.worker_id().is_some() {
        CommandAuthority::Worker
    } else if actor
        .owner()
        .is_some_and(|owner| owner.id == server.record.owner_id)
    {
        CommandAuthority::AccountOwner
    } else if actor.server_session_scope(server_id).is_some() {
        CommandAuthority::ServerGrant
    } else if actor.server_capability(server_id).is_some() {
        CommandAuthority::ServerCapability
    } else {
        return Err(ApiError::server_not_found());
    };
    let command = request.into_domain(authority)?;
    let lifecycle_actor = match authority {
        CommandAuthority::Gateway => LifecycleActor::Gateway,
        CommandAuthority::AccountOwner => LifecycleActor::OwnerUser {
            user_id: actor.owner().expect("owner authority carries user").id,
        },
        CommandAuthority::Worker => {
            let worker_id = actor.worker_id().expect("worker authority carries id");
            let generation = match &command {
                LifecycleCommand::Stop {
                    generation: Some(generation),
                    ..
                }
                | LifecycleCommand::Snapshot {
                    generation: Some(generation),
                    ..
                } => *generation,
                _ => return Err(invalid_command("worker command generation is missing")),
            };
            LifecycleActor::AssignedWorker {
                worker_id,
                expected_generation: generation,
                lease_id: actor
                    .worker_lease_id()
                    .expect("worker authority carries lease id"),
            }
        }
        CommandAuthority::ServerGrant | CommandAuthority::ServerCapability => {
            if let Some(scope) = actor.server_session_scope(server_id) {
                LifecycleActor::ServerSession {
                    server_id,
                    grant_id: scope.grant_id,
                    session_id: scope.session_id,
                }
            } else if let Some(token_hash) = actor.server_capability(server_id) {
                LifecycleActor::ServerCapability {
                    server_id,
                    token_hash: token_hash.to_vec(),
                }
            } else {
                return Err(ApiError::server_not_found());
            }
        }
    };
    let observed_state = state
        .lifecycle
        .dispatch_command(lifecycle_actor, server_id, command)
        .await
        .map_err(lifecycle_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(CommandResponse { observed_state }),
    ))
}
