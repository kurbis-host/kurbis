use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{
    CI_RUNTIME_CPU_MILLIS, CI_RUNTIME_DISK_BYTES, CI_RUNTIME_MEMORY_BYTES, CiRuntimePlacement,
};

#[derive(Debug, sqlx::FromRow)]
pub(super) struct PreviewRuntimeRow {
    pub(super) state: String,
    pub(super) build_id: Uuid,
    pub(super) target_architecture: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct CiRuntimeRow {
    pub(super) preview_id: Uuid,
    pub(super) repository: String,
    pub(super) commit_sha: String,
    pub(super) build_id: Uuid,
    pub(super) artifact_digest: Option<String>,
    pub(super) hostname: String,
    pub(super) worker_id: Uuid,
    pub(super) worker_address: String,
    pub(super) signal_address: String,
    pub(super) generation: i64,
    pub(super) lease_token: Uuid,
    pub(super) java_port: i32,
    pub(super) bedrock_port: i32,
    pub(super) signal_port: i32,
    pub(super) state: String,
    pub(super) active: bool,
    pub(super) world_initialized: bool,
    pub(super) last_activity_at: DateTime<Utc>,
    pub(super) expires_at: DateTime<Utc>,
}
impl TryFrom<CiRuntimeRow> for CiRuntimePlacement {
    type Error = anyhow::Error;
    fn try_from(row: CiRuntimeRow) -> Result<Self> {
        Ok(Self {
            preview_id: row.preview_id,
            repository: row.repository,
            commit_sha: row.commit_sha,
            build_id: row.build_id,
            // A stopped runtime can outlive its artifact: the same commit is
            // rebuilt when the integration patches change. Failing here made
            // the worker's whole runtime listing fail. A start with an empty
            // digest cannot verify an artifact, so it fails for that preview
            // alone.
            artifact_digest: row.artifact_digest.unwrap_or_default(),
            hostname: row.hostname,
            worker_id: row.worker_id,
            worker_address: row.worker_address,
            signal_address: row.signal_address,
            generation: row.generation,
            lease_token: row.lease_token,
            java_port: row.java_port,
            bedrock_port: row.bedrock_port,
            signal_port: row.signal_port,
            state: row.state.parse()?,
            active: row.active,
            world_initialized: row.world_initialized,
            last_activity_at: row.last_activity_at,
            expires_at: row.expires_at,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct ExpiredRuntimeRow {
    preview_id: Uuid,
    repository: String,
    commit_sha: String,
    build_id: Uuid,
    artifact_digest: Option<String>,
    hostname: String,
    worker_id: Uuid,
    worker_address: String,
    signal_address: String,
    generation: i64,
    lease_token: Uuid,
    java_port: i32,
    bedrock_port: i32,
    signal_port: i32,
    state: String,
    active: bool,
    world_initialized: bool,
    last_activity_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    pub(super) capability_hash: Vec<u8>,
    pub(super) stale_cleanup_claim_token: Option<Uuid>,
}
impl ExpiredRuntimeRow {
    pub(super) fn placement(self) -> Result<CiRuntimePlacement> {
        CiRuntimeRow {
            preview_id: self.preview_id,
            repository: self.repository,
            commit_sha: self.commit_sha,
            build_id: self.build_id,
            artifact_digest: self.artifact_digest,
            hostname: self.hostname,
            worker_id: self.worker_id,
            worker_address: self.worker_address,
            signal_address: self.signal_address,
            generation: self.generation,
            lease_token: self.lease_token,
            java_port: self.java_port,
            bedrock_port: self.bedrock_port,
            signal_port: self.signal_port,
            state: self.state,
            active: self.active,
            world_initialized: self.world_initialized,
            last_activity_at: self.last_activity_at,
            expires_at: self.expires_at,
        }
        .try_into()
    }
}

pub(super) async fn load_optional_placement(
    tx: &mut Transaction<'_, Postgres>,
    preview_id: Uuid,
) -> Result<Option<CiRuntimePlacement>> {
    let row = sqlx::query_as::<_, CiRuntimeRow>(
        "SELECT runtime.preview_id, alias.repository, alias.commit_sha, alias.build_id,
                build.artifact_digest, alias.hostname, runtime.worker_id, worker.address AS worker_address,
                worker.signal_address, runtime.generation, runtime.lease_token, runtime.java_port,
                runtime.bedrock_port, runtime.signal_port, runtime.state, runtime.active,
                runtime.world_initialized, runtime.last_activity_at, alias.expires_at
         FROM ci_runtime_placements runtime
         JOIN ci_preview_aliases alias ON alias.id = runtime.preview_id
         JOIN ci_builds build ON build.id = alias.build_id
         JOIN workers worker ON worker.id = runtime.worker_id
         WHERE runtime.preview_id = $1 FOR UPDATE OF runtime",
    )
    .bind(preview_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.map(TryInto::try_into).transpose()
}

pub(super) async fn load_current_placement(
    tx: &mut Transaction<'_, Postgres>,
    preview_id: Uuid,
) -> Result<CiRuntimePlacement> {
    load_optional_placement(tx, preview_id)
        .await?
        .context("CI runtime placement was not persisted")
}

pub(super) async fn worker_alive(
    tx: &mut Transaction<'_, Postgres>,
    worker_id: Uuid,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM workers WHERE id = $1 AND NOT draining
            AND heartbeat_at > now() - interval '30 seconds')",
    )
    .bind(worker_id)
    .fetch_one(&mut **tx)
    .await?)
}

pub(super) async fn lock_worker(tx: &mut Transaction<'_, Postgres>, worker_id: Uuid) -> Result<()> {
    let current: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM workers WHERE id = $1 AND NOT draining
          AND heartbeat_at > now() - interval '30 seconds' FOR UPDATE",
    )
    .bind(worker_id)
    .fetch_optional(&mut **tx)
    .await?;
    if current.is_none() {
        bail!("CI runtime worker became unavailable");
    }
    Ok(())
}

pub(super) async fn select_worker(
    tx: &mut Transaction<'_, Postgres>,
    target_architecture: &str,
) -> Result<Option<(Uuid, i32)>> {
    let row = sqlx::query_as::<_, (Uuid, i32)>(
        "SELECT worker.id, candidates.base_port
         FROM workers worker
         CROSS JOIN LATERAL (
            SELECT candidate.port AS base_port
            FROM generate_series(worker.port_min, worker.port_max - 2) AS candidate(port)
            WHERE NOT EXISTS (SELECT 1 FROM port_leases lease WHERE lease.worker_id = worker.id
                              AND lease.port IN (candidate.port, candidate.port + 1, candidate.port + 2))
              AND NOT EXISTS (SELECT 1 FROM ci_port_leases lease WHERE lease.worker_id = worker.id
                              AND lease.port IN (candidate.port, candidate.port + 1, candidate.port + 2))
            ORDER BY candidate.port LIMIT 1
         ) candidates
         WHERE NOT worker.draining AND worker.signal_address <> ''
           AND worker.supports_ci_previews
           AND worker.runtime_architecture = $4
           AND worker.heartbeat_at > now() - interval '30 seconds'
           AND worker.total_memory_bytes - worker.reserved_memory_bytes >= $1
           AND worker.total_disk_bytes - worker.reserved_disk_bytes >= $2
           AND worker.total_cpu_millis - worker.reserved_cpu_millis >= $3
         ORDER BY (worker.total_memory_bytes - worker.reserved_memory_bytes - $1),
                  (worker.total_disk_bytes - worker.reserved_disk_bytes - $2), worker.id
         FOR UPDATE OF worker SKIP LOCKED LIMIT 1",
    )
    .bind(CI_RUNTIME_MEMORY_BYTES)
    .bind(CI_RUNTIME_DISK_BYTES)
    .bind(CI_RUNTIME_CPU_MILLIS)
    .bind(target_architecture)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row)
}

pub(super) async fn choose_ci_port(
    tx: &mut Transaction<'_, Postgres>,
    worker_id: Uuid,
) -> Result<i32> {
    sqlx::query_scalar(
        "SELECT candidate.port FROM workers worker
         CROSS JOIN LATERAL generate_series(worker.port_min, worker.port_max - 2) AS candidate(port)
         WHERE worker.id = $1
           AND NOT EXISTS (SELECT 1 FROM port_leases lease WHERE lease.worker_id = worker.id
                           AND lease.port IN (candidate.port, candidate.port + 1, candidate.port + 2))
           AND NOT EXISTS (SELECT 1 FROM ci_port_leases lease WHERE lease.worker_id = worker.id
                           AND lease.port IN (candidate.port, candidate.port + 1, candidate.port + 2))
         ORDER BY candidate.port LIMIT 1",
    )
    .bind(worker_id)
    .fetch_optional(&mut **tx)
    .await?
    .context("CI runtime worker has no free port triple")
}

pub(super) async fn reserve_ci_ports(
    tx: &mut Transaction<'_, Postgres>,
    worker_id: Uuid,
    preview_id: Uuid,
    base_port: i32,
) -> Result<()> {
    sqlx::query("DELETE FROM ci_port_leases WHERE preview_id = $1")
        .bind(preview_id)
        .execute(&mut **tx)
        .await?;
    for (offset, purpose) in [(0, "java"), (1, "bedrock"), (2, "signal")] {
        sqlx::query(
            "INSERT INTO ci_port_leases(worker_id, port, preview_id, purpose) VALUES($1,$2,$3,$4)",
        )
        .bind(worker_id)
        .bind(base_port + offset)
        .bind(preview_id)
        .bind(purpose)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

pub(super) async fn reserve_disk(
    tx: &mut Transaction<'_, Postgres>,
    worker_id: Uuid,
) -> Result<()> {
    sqlx::query("UPDATE workers SET reserved_disk_bytes = reserved_disk_bytes + $2 WHERE id = $1")
        .bind(worker_id)
        .bind(CI_RUNTIME_DISK_BYTES)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub(super) async fn reserve_active_capacity(
    tx: &mut Transaction<'_, Postgres>,
    worker_id: Uuid,
) -> Result<()> {
    let reserved = sqlx::query(
        "UPDATE workers SET reserved_memory_bytes = reserved_memory_bytes + $2,
                reserved_cpu_millis = reserved_cpu_millis + $3
         WHERE id = $1 AND total_memory_bytes - reserved_memory_bytes >= $2
           AND total_cpu_millis - reserved_cpu_millis >= $3 AND NOT draining
           AND heartbeat_at > now() - interval '30 seconds'",
    )
    .bind(worker_id)
    .bind(CI_RUNTIME_MEMORY_BYTES)
    .bind(CI_RUNTIME_CPU_MILLIS)
    .execute(&mut **tx)
    .await?;
    if reserved.rows_affected() != 1 {
        bail!("CI runtime worker no longer has active capacity");
    }
    Ok(())
}

pub(super) async fn release_active_capacity(
    tx: &mut Transaction<'_, Postgres>,
    worker_id: Uuid,
) -> Result<()> {
    sqlx::query(
        "UPDATE workers SET reserved_memory_bytes = GREATEST(0, reserved_memory_bytes - $2),
                reserved_cpu_millis = GREATEST(0, reserved_cpu_millis - $3) WHERE id = $1",
    )
    .bind(worker_id)
    .bind(CI_RUNTIME_MEMORY_BYTES)
    .bind(CI_RUNTIME_CPU_MILLIS)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(super) fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}
