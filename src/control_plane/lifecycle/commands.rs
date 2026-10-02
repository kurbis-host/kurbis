use anyhow::Result;
use sqlx::{Postgres, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::contracts::lifecycle::ResetMode;
use crate::model::{DesiredState, ObservedState, ServerRecord, SnapshotReason};

use super::{Lifecycle, bail};

#[derive(Debug, Clone)]
pub enum LifecycleActor {
    Owner,
    OwnerUser {
        user_id: Uuid,
    },
    ServerSession {
        server_id: Uuid,
        grant_id: Uuid,
        session_id: Uuid,
    },
    ServerCapability {
        server_id: Uuid,
        token_hash: Vec<u8>,
    },
    Gateway,
    AssignedWorker {
        worker_id: Uuid,
        expected_generation: i64,
        lease_id: Uuid,
    },
}

#[derive(Debug, Error)]
pub enum LifecycleFailure {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unavailable(String),
}

#[derive(Debug, Error)]
pub enum LifecycleCommandError {
    #[error("the lifecycle command target does not exist")]
    NotFound,
    #[error("the lifecycle actor is not permitted to issue this command")]
    Forbidden,
    #[error("the lifecycle command is invalid")]
    Invalid,
    #[error("the lifecycle command conflicts with current server state")]
    Conflict,
    #[error("the lifecycle service is unavailable")]
    Unavailable,
    #[error("the lifecycle service failed")]
    Internal,
}

impl From<anyhow::Error> for LifecycleCommandError {
    fn from(error: anyhow::Error) -> Self {
        match error.downcast_ref::<LifecycleFailure>() {
            Some(failure) => {
                tracing::debug!(failure = ?failure, "lifecycle command rejected");
                match failure {
                    LifecycleFailure::NotFound(_) => Self::NotFound,
                    LifecycleFailure::Forbidden(_) => Self::Forbidden,
                    LifecycleFailure::Invalid(_) => Self::Invalid,
                    LifecycleFailure::Conflict(_) => Self::Conflict,
                    LifecycleFailure::Unavailable(_) => Self::Unavailable,
                }
            }
            None => {
                tracing::error!(error = ?error, "lifecycle service operation failed");
                Self::Internal
            }
        }
    }
}

/// Domain commands accepted by the lifecycle dispatcher.
///
/// The HTTP representation lives in `api::commands`; keeping this enum in the
/// lifecycle module prevents transport details from leaking into state
/// transitions and gives workers and browser owners the same command path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleCommand {
    Start {
        reason: String,
    },
    Stop {
        generation: Option<i64>,
        reason: String,
    },
    Restart,
    Snapshot {
        generation: Option<i64>,
        reason: SnapshotReason,
    },
    Reset {
        mode: ResetMode,
        full_reset_settings: serde_json::Value,
    },
    Delete,
    Restore {
        generation: i64,
    },
}

impl Lifecycle {
    /// Dispatch a lifecycle command for an authenticated actor.
    ///
    /// Worker identity is supplied by authentication middleware and represented
    /// by `LifecycleActor::AssignedWorker`; it is never accepted as a command
    /// field.  The worker generation is checked again by the transactionally
    /// fenced lifecycle methods below.
    pub async fn dispatch_command(
        &self,
        actor: LifecycleActor,
        server_id: Uuid,
        command: LifecycleCommand,
    ) -> std::result::Result<ObservedState, LifecycleCommandError> {
        self.dispatch_command_inner(actor, server_id, command)
            .await
            .map_err(LifecycleCommandError::from)
    }

    async fn dispatch_command_inner(
        &self,
        actor: LifecycleActor,
        server_id: Uuid,
        command: LifecycleCommand,
    ) -> Result<ObservedState> {
        match (actor, command) {
            (LifecycleActor::OwnerUser { user_id }, command) => {
                self.dispatch_authorized_actor(
                    LifecycleActor::OwnerUser { user_id },
                    server_id,
                    command,
                )
                .await
            }
            (
                actor @ LifecycleActor::ServerSession {
                    server_id: scope, ..
                },
                command,
            ) => {
                if scope != server_id {
                    bail!(not_found: "server-scoped actor cannot command another server");
                }
                if !matches!(
                    &command,
                    LifecycleCommand::Start { .. }
                        | LifecycleCommand::Stop { .. }
                        | LifecycleCommand::Restart
                        | LifecycleCommand::Snapshot { .. }
                        | LifecycleCommand::Restore { .. }
                        | LifecycleCommand::Reset { .. }
                        | LifecycleCommand::Delete
                ) {
                    bail!(forbidden: "server-scoped actor cannot issue this command");
                }
                self.dispatch_authorized_actor(actor, server_id, command)
                    .await
            }
            (
                actor @ LifecycleActor::ServerCapability {
                    server_id: scope, ..
                },
                command,
            ) => {
                if scope != server_id {
                    bail!(not_found: "server-scoped actor cannot command another server");
                }
                if !matches!(
                    &command,
                    LifecycleCommand::Start { .. }
                        | LifecycleCommand::Stop { .. }
                        | LifecycleCommand::Restart
                        | LifecycleCommand::Snapshot { .. }
                        | LifecycleCommand::Restore { .. }
                ) {
                    bail!(forbidden: "runtime capability cannot issue reset or delete commands");
                }
                self.dispatch_authorized_actor(actor, server_id, command)
                    .await
            }
            (LifecycleActor::Gateway, LifecycleCommand::Start { reason })
                if reason == "gateway_join" =>
            {
                self.admit_start(LifecycleActor::Gateway, server_id, &reason, false)
                    .await?
                    .ok_or_else(|| {
                        LifecycleFailure::Conflict(
                            "gateway start admission returned no state".to_owned(),
                        )
                        .into()
                    })
            }
            (LifecycleActor::Gateway, _) => {
                bail!(forbidden: "gateway actor may only request join starts")
            }
            (
                actor @ LifecycleActor::AssignedWorker {
                    expected_generation,
                    ..
                },
                LifecycleCommand::Stop { generation, reason },
            ) => {
                if generation != Some(expected_generation)
                    || !matches!(reason.as_str(), "idle_timeout" | "storage_quota")
                {
                    bail!("worker lifecycle command is stale or unsupported");
                }
                self.request_stop_for_actor(actor, server_id, &reason).await
            }
            (
                actor @ LifecycleActor::AssignedWorker {
                    expected_generation,
                    ..
                },
                LifecycleCommand::Snapshot { generation, reason },
            ) => {
                if generation != Some(expected_generation) || reason != SnapshotReason::Periodic {
                    bail!("worker lifecycle command is stale or unsupported");
                }
                self.request_snapshot_for_actor(actor, server_id, reason)
                    .await
            }
            (LifecycleActor::AssignedWorker { .. }, _) => {
                bail!(forbidden: "worker actor cannot issue this lifecycle command")
            }
            (LifecycleActor::Owner, command) => {
                self.dispatch_authorized_actor(LifecycleActor::Owner, server_id, command)
                    .await
            }
        }
    }

    async fn dispatch_authorized_actor(
        &self,
        actor: LifecycleActor,
        server_id: Uuid,
        command: LifecycleCommand,
    ) -> Result<ObservedState> {
        match command {
            LifecycleCommand::Start { reason } => self
                .admit_start(actor, server_id, &reason, false)
                .await?
                .ok_or_else(|| {
                    LifecycleFailure::Conflict(
                        "unconditional start admission returned no state".to_owned(),
                    )
                    .into()
                }),
            LifecycleCommand::Stop { reason, .. } => {
                self.request_stop_for_actor(actor, server_id, &reason).await
            }
            LifecycleCommand::Restart => self.request_restart_for_actor(actor, server_id).await,
            LifecycleCommand::Snapshot { reason, .. } => {
                self.request_snapshot_for_actor(actor, server_id, reason)
                    .await
            }
            LifecycleCommand::Reset {
                mode,
                full_reset_settings,
            } => {
                self.request_reset_for_actor(actor, server_id, mode, full_reset_settings)
                    .await
            }
            LifecycleCommand::Delete => self.request_delete_for_actor(actor, server_id).await,
            LifecycleCommand::Restore { generation } => {
                self.request_restore_for_actor(actor, server_id, generation)
                    .await
            }
        }
    }
}

pub(super) async fn authorize_actor(
    tx: &mut Transaction<'_, Postgres>,
    server: &ServerRecord,
    actor: LifecycleActor,
    require_active: bool,
) -> Result<()> {
    match actor {
        LifecycleActor::Owner => Ok(()),
        LifecycleActor::Gateway => {
            if server.desired_state == DesiredState::Deleted
                || server.observed_state == ObservedState::Deleted
            {
                bail!(not_found: "server is deleted");
            }
            Ok(())
        }
        LifecycleActor::OwnerUser { user_id } => {
            if server.owner_id != user_id {
                bail!(not_found: "server is not owned by the authenticated account");
            }
            let active: Option<bool> = sqlx::query_scalar(
                "SELECT account_status = 'active' FROM users WHERE id = $1 FOR SHARE",
            )
            .bind(user_id)
            .fetch_optional(&mut **tx)
            .await?;
            if active != Some(true) {
                bail!(forbidden: "account is suspended");
            }
            Ok(())
        }
        LifecycleActor::ServerSession {
            server_id,
            grant_id,
            session_id,
        } => {
            if server_id != server.id {
                bail!(not_found: "server-scoped actor cannot command another server");
            }
            let session: Option<Uuid> = sqlx::query_scalar(
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
            if session.is_none() {
                bail!(forbidden: "server-scoped grant was revoked or is no longer valid");
            }
            Ok(())
        }
        LifecycleActor::ServerCapability {
            server_id,
            token_hash,
        } => {
            if server_id != server.id {
                bail!(not_found: "server-scoped actor cannot command another server");
            }
            let capability: Option<Uuid> = sqlx::query_scalar(
                "SELECT c.server_id
                 FROM server_instance_capabilities c
                 JOIN placements p ON p.server_id = c.server_id AND p.worker_id = c.worker_id
                   AND p.generation = c.placement_generation AND p.active
                 JOIN servers s ON s.id = c.server_id AND s.generation = p.generation
                 JOIN users u ON u.id = s.owner_id
                 WHERE c.server_id = $1 AND c.token_hash = $2
                   AND s.desired_state <> 'deleted' AND u.account_status = 'active'
                 FOR SHARE OF c, p, s, u",
            )
            .bind(server_id)
            .bind(token_hash)
            .fetch_optional(&mut **tx)
            .await?;
            if capability.is_none() {
                bail!(forbidden: "server capability was revoked or is no longer assigned");
            }
            Ok(())
        }
        LifecycleActor::AssignedWorker {
            worker_id,
            expected_generation,
            lease_id,
        } => {
            let current_worker: Option<Uuid> = sqlx::query_scalar(
                "SELECT worker_id FROM worker_credentials
                 WHERE worker_id = $1 AND lease_id = $2
                   AND revoked_at IS NULL AND lease_expires_at > now()
                 FOR SHARE",
            )
            .bind(worker_id)
            .bind(lease_id)
            .fetch_optional(&mut **tx)
            .await?;
            if current_worker != Some(worker_id) {
                bail!("worker process lease is no longer current");
            }
            let placement: Option<(Uuid, i64, bool)> = sqlx::query_as(
                "SELECT worker_id, generation, active FROM placements
                 WHERE server_id = $1 FOR SHARE",
            )
            .bind(server.id)
            .fetch_optional(&mut **tx)
            .await?;
            let (placement_worker_id, placement_generation, placement_active) = placement
                .ok_or_else(|| {
                    LifecycleFailure::Conflict(
                        "server has no worker placement for lifecycle command".to_owned(),
                    )
                })?;
            if server.generation != expected_generation
                || placement_generation != expected_generation
                || placement_worker_id != worker_id
                || (require_active && !placement_active)
            {
                bail!("lifecycle command no longer matches the assigned worker generation");
            }
            Ok(())
        }
    }
}
