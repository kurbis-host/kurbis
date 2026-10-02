use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    contracts::worker::{WorkerAssignment, WorkerServerContext, WorkerServerState},
    model::{DesiredState, ObservedState},
    pumpkin_control::{PlayerEdition, WaitingPlayer, WhitelistEntry},
};

use super::{ApiError, AppState, worker_auth};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ContextQuery {
    assignment: String,
    generation: i64,
    operation_id: Option<Uuid>,
    export_id: Option<Uuid>,
    attempt: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AcquireLeaseRequest {
    lease_id: Uuid,
    signal_address: std::net::IpAddr,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct AcquireLeaseResponse {
    worker_id: Uuid,
}

pub(super) async fn acquire_lease(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AcquireLeaseRequest>,
) -> Result<Json<AcquireLeaseResponse>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let worker_id = worker_auth::lock_authenticated_worker_id_for_lease(&mut tx, &headers).await?;
    let registered_address: Option<String> =
        sqlx::query_scalar("SELECT signal_address FROM workers WHERE id = $1 FOR UPDATE")
            .bind(worker_id)
            .fetch_optional(&mut *tx)
            .await?;
    let requested_signal_address = request.signal_address.to_string();
    if registered_address.as_deref() != Some(requested_signal_address.as_str())
        || !crate::model::is_private_signal_address(&requested_signal_address)
    {
        return Err(worker_auth::invalid_worker_token());
    }
    let acquired: Option<Uuid> = sqlx::query_scalar(
        "UPDATE worker_credentials SET lease_id = $2,
                lease_expires_at = now() + interval '30 seconds'
         WHERE worker_id = $1 AND revoked_at IS NULL
           AND (lease_id = $2 OR lease_expires_at IS NULL OR lease_expires_at <= now())
         RETURNING worker_id",
    )
    .bind(worker_id)
    .bind(request.lease_id)
    .fetch_optional(&mut *tx)
    .await?;
    if acquired != Some(worker_id) {
        return Err(ApiError::with_code(
            StatusCode::CONFLICT,
            "worker_already_active",
            "another process currently holds this worker identity",
        ));
    }
    sqlx::query("UPDATE workers SET heartbeat_at = now() WHERE id = $1")
        .bind(worker_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(AcquireLeaseResponse { worker_id }))
}

impl ContextQuery {
    pub(super) fn assignment(self) -> Result<WorkerAssignment, ApiError> {
        match self.assignment.as_str() {
            "placement" if self.operation_id.is_none() && self.export_id.is_none() => {
                Ok(WorkerAssignment::Placement {
                    generation: self.generation,
                })
            }
            "operation" if self.export_id.is_none() => Ok(WorkerAssignment::Operation {
                operation_id: self.operation_id.ok_or_else(invalid_assignment)?,
                generation: self.generation,
                attempt: self.attempt.ok_or_else(invalid_assignment)?,
            }),
            "export" if self.operation_id.is_none() => Ok(WorkerAssignment::Export {
                export_id: self.export_id.ok_or_else(invalid_assignment)?,
                generation: self.generation,
                attempt: self.attempt.ok_or_else(invalid_assignment)?,
            }),
            _ => Err(invalid_assignment()),
        }
    }
}

pub(super) async fn retained_servers(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<Uuid>>, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let ids = sqlx::query_scalar(
        "SELECT server_id FROM placements WHERE worker_id = $1 ORDER BY server_id",
    )
    .bind(worker_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(ids))
}

pub(super) async fn servers(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<WorkerServerState>>, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let rows = sqlx::query_as::<_, (Value, DesiredState, ObservedState, bool, bool)>(
        "SELECT jsonb_build_object(
                    'server_id', p.server_id, 'worker_id', p.worker_id,
                    'generation', p.generation, 'lease_token', p.lease_token,
                    'java_port', p.java_port, 'bedrock_port', p.bedrock_port,
                    'signal_port', p.signal_port, 'active', p.active,
                    'updated_at', p.updated_at),
                s.desired_state, s.observed_state,
                EXISTS(SELECT 1 FROM operations o WHERE o.server_id = s.id
                    AND o.generation = p.generation AND o.kind = 'start' AND o.status = 'failed'),
                EXISTS(SELECT 1 FROM operations o WHERE o.server_id = s.id
                    AND o.generation = p.generation AND o.kind = 'snapshot' AND o.status = 'failed')
         FROM placements p JOIN servers s ON s.id = p.server_id
         WHERE p.worker_id = $1 AND p.generation = s.generation
         ORDER BY p.server_id",
    )
    .bind(worker_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(
                |(placement, desired_state, observed_state, failed_start, failed_snapshot)| {
                    let placement =
                        serde_json::from_value(placement).map_err(|_| invalid_assignment())?;
                    Ok(WorkerServerState {
                        placement,
                        desired_state,
                        observed_state,
                        failed_start,
                        failed_snapshot,
                    })
                },
            )
            .collect::<Result<Vec<_>, ApiError>>()?,
    ))
}

pub(super) async fn server_context(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path(server_id): Path<Uuid>,
    Query(query): Query<ContextQuery>,
) -> Result<Json<WorkerServerContext>, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let assignment = query.assignment()?;
    let generation = assignment_generation(assignment);
    validate_assignment(&state.pool, worker_id, server_id, assignment).await?;

    let row = sqlx::query_as::<
        _,
        (
            Value,
            DesiredState,
            ObservedState,
            Value,
            Option<i64>,
            bool,
            i64,
            i64,
            i64,
            i64,
        ),
    >(
        "SELECT jsonb_build_object(
                    'server_id', p.server_id, 'worker_id', p.worker_id,
                    'generation', p.generation, 'lease_token', p.lease_token,
                    'java_port', p.java_port, 'bedrock_port', p.bedrock_port,
                    'signal_port', p.signal_port, 'active', p.active,
                    'updated_at', p.updated_at),
                s.desired_state, s.observed_state, s.settings, s.current_snapshot_generation,
                COALESCE((s.settings->>'whitelist')::boolean, false),
                COALESCE(w.revision, 0), COALESCE(w.applied_revision, -1),
                COALESCE(o.revision, 1), COALESCE(o.applied_revision, 0)
         FROM servers s JOIN placements p ON p.server_id = s.id
         LEFT JOIN server_whitelist_state w ON w.server_id = s.id
         LEFT JOIN server_operator_state o ON o.server_id = s.id
         WHERE s.id = $1 AND s.generation = $2 AND p.worker_id = $3 AND p.generation = $2",
    )
    .bind(server_id)
    .bind(generation)
    .bind(worker_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(invalid_assignment)?;

    let entries: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT player_id, name FROM whitelist_entries WHERE server_id = $1 ORDER BY lower(name), player_id",
    )
    .bind(server_id)
    .fetch_all(&state.pool)
    .await?;
    let owners: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT DISTINCT ON (i.minecraft_player_id) i.minecraft_player_id, i.display_name
         FROM servers s JOIN identities i ON i.user_id = s.owner_id
         WHERE s.id = $1 AND i.provider IN ('minecraft_java', 'minecraft_bedrock')
           AND i.minecraft_player_id IS NOT NULL
         ORDER BY i.minecraft_player_id, i.created_at, i.id",
    )
    .bind(server_id)
    .fetch_all(&state.pool)
    .await?;
    let placement = serde_json::from_value(row.0).map_err(|_| invalid_assignment())?;
    Ok(Json(WorkerServerContext {
        placement,
        desired_state: row.1,
        observed_state: row.2,
        settings: row.3,
        current_snapshot_generation: row.4,
        whitelist_enabled: row.5,
        whitelist_revision: row.6,
        whitelist_applied_revision: row.7,
        operator_revision: row.8,
        operator_applied_revision: row.9,
        whitelist_entries: entries
            .into_iter()
            .map(|(id, name)| WhitelistEntry { id, name })
            .collect(),
        owners: owners
            .into_iter()
            .map(|(id, name)| WhitelistEntry { id, name })
            .collect(),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlayersSyncRequest {
    generation: i64,
    players: Vec<WaitingPlayer>,
}

pub(super) async fn sync_players(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path(server_id): Path<Uuid>,
    Json(request): Json<PlayersSyncRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let mut tx = state.pool.begin().await?;
    lock_current_placement(&mut tx, worker_id, server_id, request.generation).await?;
    let ids: Vec<Uuid> = request.players.iter().map(|player| player.id).collect();
    sqlx::query(
        "DELETE FROM server_players WHERE server_id = $1 AND NOT (player_id = ANY($2::uuid[]))",
    )
    .bind(server_id)
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    for player in request.players {
        let edition = match player.edition {
            PlayerEdition::Java => "java",
            PlayerEdition::Bedrock => "bedrock",
        };
        sqlx::query(
            "INSERT INTO server_players(server_id, player_id, name, edition)
             VALUES($1,$2,$3,$4) ON CONFLICT(server_id, player_id) DO UPDATE
             SET name = EXCLUDED.name, edition = EXCLUDED.edition, last_seen_at = now()",
        )
        .bind(server_id)
        .bind(player.id)
        .bind(player.name)
        .bind(edition)
        .execute(&mut *tx)
        .await?;
    }
    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LogSyncRequest {
    generation: i64,
    lines: Vec<String>,
}

pub(super) async fn append_logs(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path(server_id): Path<Uuid>,
    Json(request): Json<LogSyncRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    if request.lines.len() > 2_000 || request.lines.iter().any(|line| line.len() > 16_384) {
        return Err(ApiError::with_code(
            StatusCode::PAYLOAD_TOO_LARGE,
            "worker_payload_too_large",
            "worker log batch exceeds its size limit",
        ));
    }
    let mut tx = state.pool.begin().await?;
    lock_current_placement(&mut tx, worker_id, server_id, request.generation).await?;
    for line in request.lines {
        sqlx::query("INSERT INTO server_logs(server_id, line) VALUES($1, $2)")
            .bind(server_id)
            .bind(line)
            .execute(&mut *tx)
            .await?;
    }
    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RevisionSyncRequest {
    generation: i64,
    revision: i64,
}

pub(super) async fn mark_whitelist_applied(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path(server_id): Path<Uuid>,
    Json(request): Json<RevisionSyncRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let mut tx = state.pool.begin().await?;
    lock_current_placement(&mut tx, worker_id, server_id, request.generation).await?;
    sqlx::query(
        "UPDATE server_whitelist_state SET applied_revision = $2
         WHERE server_id = $1 AND revision = $2",
    )
    .bind(server_id)
    .bind(request.revision)
    .execute(&mut *tx)
    .await?;
    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn mark_operators_applied(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Path(server_id): Path<Uuid>,
    Json(request): Json<RevisionSyncRequest>,
) -> Result<StatusCode, ApiError> {
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let mut tx = state.pool.begin().await?;
    lock_current_placement(&mut tx, worker_id, server_id, request.generation).await?;
    sqlx::query(
        "UPDATE server_operator_state SET applied_revision = $2
         WHERE server_id = $1 AND revision = $2",
    )
    .bind(server_id)
    .bind(request.revision)
    .execute(&mut *tx)
    .await?;
    if worker_auth::lock_authenticated_worker_id(&mut tx, &headers).await? != worker_id {
        return Err(worker_auth::invalid_worker_token());
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn lock_current_placement(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    worker_id: Uuid,
    server_id: Uuid,
    generation: i64,
) -> Result<(), ApiError> {
    if generation <= 0 {
        return Err(invalid_assignment());
    }
    let assignment: Option<Uuid> = sqlx::query_scalar(
        "SELECT p.server_id FROM placements p JOIN servers s ON s.id = p.server_id
         WHERE p.server_id = $1 AND p.worker_id = $2 AND p.generation = $3 AND s.generation = $3
         FOR SHARE OF p, s",
    )
    .bind(server_id)
    .bind(worker_id)
    .bind(generation)
    .fetch_optional(&mut **tx)
    .await?;
    if assignment != Some(server_id) {
        return Err(invalid_assignment());
    }
    Ok(())
}

pub(super) async fn validate_assignment(
    pool: &PgPool,
    worker_id: Uuid,
    server_id: Uuid,
    assignment: WorkerAssignment,
) -> Result<(), ApiError> {
    let allowed: bool = match assignment {
        WorkerAssignment::Placement { generation } => {
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM placements p JOIN servers s ON s.id = p.server_id WHERE p.server_id=$1 AND p.worker_id=$2 AND p.generation=$3 AND s.generation=$3)")
                .bind(server_id).bind(worker_id).bind(generation).fetch_one(pool).await?
        }
        WorkerAssignment::Operation { operation_id, generation, attempt } => {
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM operations o JOIN servers s ON s.id=o.server_id WHERE o.id=$1 AND o.server_id=$2 AND o.worker_id=$3 AND o.generation=$4 AND o.attempt=$5 AND o.status='running' AND s.generation=$4)")
                .bind(operation_id).bind(server_id).bind(worker_id).bind(generation).bind(attempt).fetch_one(pool).await?
        }
        WorkerAssignment::Export { export_id, generation, attempt } => {
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM world_exports e WHERE e.id=$1 AND e.server_id=$2 AND e.worker_id=$3 AND e.source_generation=$4 AND e.attempt=$5 AND e.status='running')")
                .bind(export_id).bind(server_id).bind(worker_id).bind(generation).bind(attempt).fetch_one(pool).await?
        }
    };
    if !allowed {
        return Err(invalid_assignment());
    }
    Ok(())
}

fn assignment_generation(assignment: WorkerAssignment) -> i64 {
    match assignment {
        WorkerAssignment::Placement { generation }
        | WorkerAssignment::Operation { generation, .. }
        | WorkerAssignment::Export { generation, .. } => generation,
    }
}

fn invalid_assignment() -> ApiError {
    ApiError::with_code(
        StatusCode::CONFLICT,
        "worker_assignment_invalid",
        "worker assignment is stale or outside this worker's authority",
    )
}
