use axum::http::{HeaderMap, Method, StatusCode};
use serde::Deserialize;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::snapshot::{ObjectKind, WorkerObjectKey};

use super::super::{ApiError, worker_auth};

#[derive(Debug, Deserialize)]
#[serde(tag = "assignment", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::control_plane::api) enum AssignmentQuery {
    Placement {
        generation: String,
    },
    Operation {
        operation_id: String,
        generation: String,
        attempt: String,
    },
    Export {
        export_id: String,
        generation: String,
        attempt: String,
    },
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Assignment {
    Placement {
        generation: i64,
    },
    Operation {
        operation_id: Uuid,
        generation: i64,
        attempt: i32,
    },
    Export {
        export_id: Uuid,
        generation: i64,
        attempt: i32,
    },
}

impl Assignment {
    pub(super) fn from_query(query: AssignmentQuery) -> Result<Self, ApiError> {
        match query {
            AssignmentQuery::Placement { generation } => Ok(Self::Placement {
                generation: parse_generation(&generation)?,
            }),
            AssignmentQuery::Operation {
                operation_id,
                generation,
                attempt,
            } => Ok(Self::Operation {
                operation_id: operation_id.parse().map_err(|_| invalid_assignment())?,
                generation: parse_generation(&generation)?,
                attempt: parse_attempt(&attempt)?,
            }),
            AssignmentQuery::Export {
                export_id,
                generation,
                attempt,
            } => Ok(Self::Export {
                export_id: export_id.parse().map_err(|_| invalid_assignment())?,
                generation: parse_generation(&generation)?,
                attempt: parse_attempt(&attempt)?,
            }),
        }
    }

    pub(super) fn generation(self) -> i64 {
        match self {
            Self::Placement { generation }
            | Self::Operation { generation, .. }
            | Self::Export { generation, .. } => generation,
        }
    }
}

pub(super) async fn validate_key_authority(
    pool: &PgPool,
    worker_id: Uuid,
    server_id: Uuid,
    assignment: Assignment,
    key: &WorkerObjectKey,
    method: &Method,
) -> Result<(), ApiError> {
    let allowed = match assignment {
        Assignment::Placement { generation } => {
            let current: bool = sqlx::query_scalar(
                "SELECT EXISTS(
                    SELECT 1 FROM placements p JOIN servers s ON s.id = p.server_id
                    WHERE p.server_id = $1 AND p.worker_id = $2
                      AND p.generation = $3 AND s.generation = $3
                )",
            )
            .bind(server_id)
            .bind(worker_id)
            .bind(generation)
            .fetch_one(pool)
            .await?;
            current && is_read(method) && is_world_object(key.kind())
        }
        Assignment::Operation {
            operation_id,
            generation,
            attempt,
        } => {
            let operation: Option<(String, bool)> = sqlx::query_as(
                "SELECT o.kind::text,
                        EXISTS(SELECT 1 FROM placements p WHERE p.server_id=s.id AND p.worker_id=$3 AND p.generation=$4)
                 FROM operations o JOIN servers s ON s.id=o.server_id
                 WHERE o.id=$1 AND o.server_id=$2 AND o.worker_id=$3 AND o.status='running'
                   AND o.generation=$4 AND o.attempt=$5 AND s.generation=$4",
            ).bind(operation_id).bind(server_id).bind(worker_id).bind(generation).bind(attempt).fetch_optional(pool).await?;
            operation.is_some_and(|(kind, has_placement)| {
                (matches!(kind.as_str(), "reset" | "delete") || has_placement)
                    && operation_allows_object(&kind, key.kind(), method)
            })
        }
        Assignment::Export {
            export_id,
            generation,
            attempt: requested_attempt,
        } => {
            let active_attempt: Option<(i32, String)> = sqlx::query_as(
                "SELECT e.attempt, e.stage FROM world_exports e
                 WHERE e.id=$1 AND e.server_id=$2 AND e.worker_id=$3 AND e.status='running'
                   AND e.source_generation=$4 AND e.attempt=$5",
            )
            .bind(export_id)
            .bind(server_id)
            .bind(worker_id)
            .bind(generation)
            .bind(requested_attempt)
            .fetch_optional(pool)
            .await?;
            active_attempt.is_some_and(|(attempt, stage)| {
                export_allows_object(export_id, attempt, &stage, key, method)
            })
        }
    };
    if allowed {
        Ok(())
    } else {
        Err(object_not_available())
    }
}

pub(super) async fn validate_key_authority_locked(
    tx: &mut Transaction<'_, Postgres>,
    worker_id: Uuid,
    headers: &HeaderMap,
    server_id: Uuid,
    assignment: Assignment,
    key: &WorkerObjectKey,
    method: &Method,
) -> Result<(), ApiError> {
    let allowed = match assignment {
        Assignment::Placement { generation } => {
            let current_generation: Option<i64> =
                sqlx::query_scalar("SELECT generation FROM servers WHERE id=$1 FOR SHARE")
                    .bind(server_id)
                    .fetch_optional(&mut **tx)
                    .await?;
            let placement: Option<(Uuid, i64)> = sqlx::query_as(
                "SELECT worker_id,generation FROM placements WHERE server_id=$1 FOR SHARE",
            )
            .bind(server_id)
            .fetch_optional(&mut **tx)
            .await?;
            current_generation == Some(generation)
                && placement.is_some_and(|(owner, placed_generation)| {
                    owner == worker_id && placed_generation == generation
                })
                && is_read(method)
                && is_world_object(key.kind())
        }
        Assignment::Operation {
            operation_id,
            generation,
            attempt,
        } => {
            let current_generation: Option<i64> =
                sqlx::query_scalar("SELECT generation FROM servers WHERE id=$1 FOR SHARE")
                    .bind(server_id)
                    .fetch_optional(&mut **tx)
                    .await?;
            let operation: Option<String> = sqlx::query_scalar(
                "SELECT kind::text FROM operations WHERE id=$1 AND server_id=$2 AND worker_id=$3
                 AND status='running' AND generation=$4 AND attempt=$5 FOR SHARE",
            )
            .bind(operation_id)
            .bind(server_id)
            .bind(worker_id)
            .bind(generation)
            .bind(attempt)
            .fetch_optional(&mut **tx)
            .await?;
            let Some(kind) = operation else {
                return Err(object_not_available());
            };
            let has_current_placement = if matches!(kind.as_str(), "reset" | "delete") {
                true
            } else {
                let placement: Option<(Uuid, i64)> = sqlx::query_as(
                    "SELECT worker_id,generation FROM placements WHERE server_id=$1 FOR SHARE",
                )
                .bind(server_id)
                .fetch_optional(&mut **tx)
                .await?;
                placement.is_some_and(|(owner, placed_generation)| {
                    owner == worker_id && placed_generation == generation
                })
            };
            current_generation == Some(generation)
                && has_current_placement
                && operation_allows_object(&kind, key.kind(), method)
        }
        Assignment::Export {
            export_id,
            generation,
            attempt: requested_attempt,
        } => {
            sqlx::query("SELECT id FROM servers WHERE id=$1 FOR SHARE")
                .bind(server_id)
                .fetch_optional(&mut **tx)
                .await?;
            let active_attempt: Option<(i32, String)> = sqlx::query_as(
                "SELECT attempt,stage FROM world_exports WHERE id=$1 AND server_id=$2 AND worker_id=$3
                 AND status='running' AND source_generation=$4 AND attempt=$5 FOR SHARE",
            ).bind(export_id).bind(server_id).bind(worker_id).bind(generation).bind(requested_attempt).fetch_optional(&mut **tx).await?;
            active_attempt.is_some_and(|(attempt, stage)| {
                export_allows_object(export_id, attempt, &stage, key, method)
            })
        }
    };
    if !allowed {
        return Err(object_not_available());
    }
    let locked_worker_id = worker_auth::lock_authenticated_worker_id(tx, headers).await?;
    if locked_worker_id != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    Ok(())
}

pub(super) fn invalid_assignment() -> ApiError {
    ApiError::with_code(
        StatusCode::BAD_REQUEST,
        "invalid_object_assignment",
        "object assignment is invalid",
    )
}

fn parse_generation(value: &str) -> Result<i64, ApiError> {
    value.parse::<i64>().map_err(|_| invalid_assignment())
}

fn parse_attempt(value: &str) -> Result<i32, ApiError> {
    value
        .parse::<i32>()
        .ok()
        .filter(|attempt| *attempt > 0)
        .ok_or_else(invalid_assignment)
}

fn object_not_available() -> ApiError {
    ApiError::with_code(
        StatusCode::NOT_FOUND,
        "object_not_found",
        "object is not available to this worker",
    )
}

fn operation_allows_object(kind: &str, object_kind: ObjectKind, method: &Method) -> bool {
    match object_kind {
        ObjectKind::Chunk => {
            let can_read = matches!(
                kind,
                "start" | "snapshot" | "stop" | "restart" | "restore" | "reset"
            );
            let can_write = matches!(kind, "snapshot" | "stop" | "restart" | "restore" | "reset");
            (can_read && is_read(method)) || (can_write && *method == Method::PUT)
        }
        ObjectKind::Manifest { .. } => {
            let can_read = matches!(
                kind,
                "start" | "snapshot" | "stop" | "restart" | "restore" | "reset" | "delete"
            );
            let can_write = matches!(kind, "snapshot" | "stop" | "restart" | "restore" | "reset");
            let can_delete = can_write || kind == "delete";
            (can_read && is_read(method))
                || (can_write && *method == Method::PUT)
                || (can_delete && *method == Method::DELETE)
        }
        ObjectKind::Export { .. } => false,
    }
}

fn export_allows_object(
    export_id: Uuid,
    current_attempt: i32,
    stage: &str,
    key: &WorkerObjectKey,
    method: &Method,
) -> bool {
    match key.kind() {
        ObjectKind::Chunk | ObjectKind::Manifest { .. } => is_read(method),
        ObjectKind::Export {
            export_id: key_export_id,
            attempt,
        } => {
            key_export_id == export_id
                && attempt == current_attempt
                && (*method != Method::PUT || stage == "archiving")
                && matches!(
                    *method,
                    Method::GET | Method::HEAD | Method::PUT | Method::DELETE
                )
        }
    }
}

fn is_read(method: &Method) -> bool {
    *method == Method::GET || *method == Method::HEAD
}

fn is_world_object(kind: ObjectKind) -> bool {
    matches!(kind, ObjectKind::Chunk | ObjectKind::Manifest { .. })
}
