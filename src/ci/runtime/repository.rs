use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

mod rows;
use rows::{
    CiRuntimeRow, ExpiredRuntimeRow, PreviewRuntimeRow, choose_ci_port, constant_time_equal,
    load_current_placement, load_optional_placement, lock_worker, release_active_capacity,
    reserve_active_capacity, reserve_ci_ports, reserve_disk, select_worker, worker_alive,
};

use super::{CiRuntimeState, RuntimeLease};

pub const CI_RUNTIME_MEMORY_BYTES: i64 = 1024 * 1024 * 1024;
pub const CI_RUNTIME_DISK_BYTES: i64 = crate::model::SERVER_STORAGE_BYTES;
pub const CI_RUNTIME_CPU_MILLIS: i64 = 1000;
pub const CI_RUNTIME_IDLE_SECONDS: i64 = 300;
pub const CI_RUNTIME_WORLD_SNAPSHOT_SECONDS: i64 = 600;

#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct CiRuntimePlacement {
    pub preview_id: Uuid,
    pub repository: String,
    pub commit_sha: String,
    pub build_id: Uuid,
    pub artifact_digest: String,
    pub hostname: String,
    pub worker_id: Uuid,
    pub worker_address: String,
    pub signal_address: String,
    pub generation: i64,
    pub lease_token: Uuid,
    pub java_port: i32,
    pub bedrock_port: i32,
    pub signal_port: i32,
    pub state: CiRuntimeState,
    pub active: bool,
    pub world_initialized: bool,
    pub last_activity_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredCiRuntime {
    pub placement: CiRuntimePlacement,
    pub capability_hash: Vec<u8>,
    pub stale_cleanup_claim_token: Option<Uuid>,
}

#[derive(Clone)]
pub struct CiPreviewRuntimeRepository {
    pool: PgPool,
}

impl CiPreviewRuntimeRepository {
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Atomically acquires or resumes a preview runtime. An existing worker owns
    /// the preview's durable world for its full TTL; idle starts retain that
    /// worker/disk reservation but rotate both the placement lease and generation.
    pub async fn claim_start(
        &self,
        preview_id: Uuid,
        capability_hash: &[u8],
    ) -> Result<RuntimeLease> {
        if capability_hash.len() != 32 {
            bail!("CI runtime capability hash must be 32 bytes");
        }
        let mut tx = self.pool.begin().await?;
        let preview = sqlx::query_as::<_, PreviewRuntimeRow>(
            "SELECT alias.state, alias.build_id, build.target_architecture
             FROM ci_preview_aliases alias
             JOIN ci_builds build ON build.id = alias.build_id
             JOIN ci_artifacts artifact ON artifact.digest = build.artifact_digest
             WHERE alias.id = $1 AND alias.expires_at > now()
               AND build.stage = 'ready' AND (build.expires_at IS NULL OR build.expires_at > now())
               AND (artifact.expires_at IS NULL OR artifact.expires_at > now())
             FOR UPDATE OF alias, build",
        )
        .bind(preview_id)
        .fetch_optional(&mut *tx)
        .await?
        .context("CI preview is not ready or has expired")?;

        if preview.state == "running" {
            let placement = load_current_placement(&mut tx, preview_id).await?;
            if worker_alive(&mut tx, placement.worker_id).await? {
                tx.commit().await?;
                return Ok(RuntimeLease::AlreadyRunning(placement));
            }
            // A running alias is not proof that its worker is still alive. Fall
            // through to the fenced replacement path, which requires a durable
            // world checkpoint before changing worker ownership.
        }
        if !matches!(
            preview.state.as_str(),
            "queued" | "ready" | "booting" | "running" | "stopping"
        ) {
            bail!("CI preview cannot start from state {}", preview.state);
        }

        let old = load_optional_placement(&mut tx, preview_id).await?;
        if preview.state == "stopping"
            && let Some(existing) = old.as_ref()
            && worker_alive(&mut tx, existing.worker_id).await?
        {
            tx.commit().await?;
            return Ok(RuntimeLease::Stopping(existing.clone()));
        }
        let (worker_id, base_port, generation) = match old.as_ref() {
            Some(existing)
                if existing.state == CiRuntimeState::Starting
                    && worker_alive(&mut tx, existing.worker_id).await? =>
            {
                tx.commit().await?;
                return Ok(RuntimeLease::Starting(existing.clone()));
            }
            Some(existing) if worker_alive(&mut tx, existing.worker_id).await? => {
                lock_worker(&mut tx, existing.worker_id).await?;
                if existing.active {
                    bail!("CI runtime generation is active while preview is not running");
                }
                let port = choose_ci_port(&mut tx, existing.worker_id).await?;
                reserve_ci_ports(&mut tx, existing.worker_id, preview_id, port).await?;
                reserve_active_capacity(&mut tx, existing.worker_id).await?;
                (existing.worker_id, port, existing.generation + 1)
            }
            Some(existing) => {
                let (has_snapshot, world_initialized): (bool, bool) = sqlx::query_as(
                    "SELECT EXISTS(SELECT 1 FROM ci_world_snapshots WHERE preview_id=$1),
                            world_initialized
                     FROM ci_runtime_placements WHERE preview_id=$1",
                )
                .bind(preview_id)
                .fetch_one(&mut *tx)
                .await?;
                // A world is safe to discard only until it has a published
                // checkpoint. mark_world_initialized records a generation-fenced
                // bootstrap in progress; it must not make a partial, never-routable
                // world look like durable player data.
                let safe_cold_start = !world_initialized
                    && matches!(
                        existing.state,
                        CiRuntimeState::Starting
                            | CiRuntimeState::Stopping
                            | CiRuntimeState::Stopped
                    );
                if !has_snapshot && !safe_cold_start {
                    // A previously started world may not be replaced by an
                    // empty directory; wait for a valid durable checkpoint.
                    tx.commit().await?;
                    return Ok(RuntimeLease::NoWorker);
                }
                // Reassignment is safe only after a complete last-known-good
                // snapshot has been published. Find and lock a replacement before
                // releasing the previous worker's reservation; when capacity is
                // unavailable, leave accounting untouched for a later retry.
                let candidate = select_worker(&mut tx, &preview.target_architecture).await?;
                let Some((worker_id, port)) = candidate else {
                    tx.commit().await?;
                    return Ok(RuntimeLease::NoWorker);
                };
                if existing.state != CiRuntimeState::Stopped {
                    release_active_capacity(&mut tx, existing.worker_id).await?;
                }
                sqlx::query("UPDATE workers SET reserved_disk_bytes=GREATEST(0,reserved_disk_bytes-$2) WHERE id=$1")
                    .bind(existing.worker_id)
                    .bind(CI_RUNTIME_DISK_BYTES)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("DELETE FROM ci_port_leases WHERE preview_id=$1")
                    .bind(preview_id)
                    .execute(&mut *tx)
                    .await?;
                reserve_ci_ports(&mut tx, worker_id, preview_id, port).await?;
                reserve_disk(&mut tx, worker_id).await?;
                reserve_active_capacity(&mut tx, worker_id).await?;
                (worker_id, port, existing.generation + 1)
            }
            None => {
                let worker = select_worker(&mut tx, &preview.target_architecture).await?;
                let Some((worker_id, port)) = worker else {
                    tx.commit().await?;
                    return Ok(RuntimeLease::NoWorker);
                };
                reserve_ci_ports(&mut tx, worker_id, preview_id, port).await?;
                reserve_disk(&mut tx, worker_id).await?;
                reserve_active_capacity(&mut tx, worker_id).await?;
                (worker_id, port, 1)
            }
        };
        let lease_token = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO ci_runtime_placements
                (preview_id, worker_id, generation, lease_token, java_port, bedrock_port,
                 signal_port, state, active, capacity_reserved, last_activity_at, capability_hash)
             VALUES ($1,$2,$3,$4,$5,$6,$7,'starting',false,true,now(),$8)
             ON CONFLICT (preview_id) DO UPDATE SET
                worker_id = EXCLUDED.worker_id, generation = EXCLUDED.generation,
                lease_token = EXCLUDED.lease_token, java_port = EXCLUDED.java_port,
                bedrock_port = EXCLUDED.bedrock_port, signal_port = EXCLUDED.signal_port,
                state = 'starting', active = false, capacity_reserved = true, last_activity_at = now(),
                bootstrap_pending = false, snapshot_requested_at = NULL,
                capability_hash = EXCLUDED.capability_hash, updated_at = now()",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .bind(base_port)
        .bind(base_port + 1)
        .bind(base_port + 2)
        .bind(capability_hash)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE ci_preview_aliases SET state = 'booting', runtime_generation = $2,
                 expires_at = now() + interval '336 hours', updated_at = now() WHERE id = $1",
        )
        .bind(preview_id)
        .bind(generation)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE ci_builds SET expires_at = CASE WHEN retention_policy = 'permanent' THEN NULL ELSE GREATEST(COALESCE(expires_at, now()), now() + interval '336 hours') END, updated_at = now()
             WHERE id = $1",
        )
        .bind(preview.build_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE ci_artifacts artifact SET expires_at = CASE WHEN artifact.expires_at IS NULL THEN NULL ELSE GREATEST(artifact.expires_at, now() + interval '336 hours') END
             FROM ci_builds build WHERE build.id = $1 AND artifact.digest = build.artifact_digest",
        )
        .bind(preview.build_id)
        .execute(&mut *tx)
        .await?;
        let placement = load_current_placement(&mut tx, preview_id).await?;
        tx.commit().await?;
        Ok(RuntimeLease::Claimed(placement))
    }

    pub async fn mark_world_initialized(
        &self,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
    ) -> Result<bool> {
        // This callback is sent before Pumpkin can mutate the local volume. It
        // opens a bootstrap snapshot slot; world_initialized remains false until
        // a complete checkpoint has been durably published.
        let result = sqlx::query(
            "UPDATE ci_runtime_placements SET bootstrap_pending=true, updated_at=now()
             WHERE preview_id=$1 AND worker_id=$2 AND generation=$3 AND lease_token=$4
               AND state='starting' AND NOT active AND NOT world_initialized",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// A successful start must still match the current worker lease and
    /// generation. Stale workers cannot bring an old runtime back into routing.
    pub async fn mark_running(
        &self,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            "UPDATE ci_runtime_placements SET state = 'running', active = true,
                    last_activity_at = now(), updated_at = now()
             WHERE preview_id = $1 AND worker_id = $2 AND generation = $3
               AND lease_token = $4 AND state = 'starting' AND NOT active
               AND world_initialized AND EXISTS (
                    SELECT 1 FROM ci_preview_aliases alias
                    JOIN ci_builds build ON build.id = alias.build_id
                    JOIN ci_artifacts artifact ON artifact.digest = build.artifact_digest
                    WHERE alias.id = $1 AND alias.runtime_generation = $3
                      AND alias.state = 'booting' AND alias.expires_at > now()
                      AND (build.expires_at IS NULL OR build.expires_at > now()) AND (artifact.expires_at IS NULL OR artifact.expires_at > now()))
               AND NOT bootstrap_pending",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if updated {
            let alias_updated = sqlx::query(
                "UPDATE ci_preview_aliases SET state = 'running', updated_at = now()
                 WHERE id = $1 AND runtime_generation = $2 AND state = 'booting'",
            )
            .bind(preview_id)
            .bind(generation)
            .execute(&mut *tx)
            .await?
            .rows_affected()
                == 1;
            if !alias_updated {
                tx.rollback().await?;
                return Ok(false);
            }
        }
        tx.commit().await?;
        Ok(updated)
    }

    pub async fn mark_start_failed(
        &self,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "UPDATE ci_runtime_placements SET state = 'stopped', active = false, capacity_reserved = false,
                    updated_at = now()
             WHERE preview_id = $1 AND worker_id = $2 AND generation = $3
               AND lease_token = $4 AND state = 'starting'",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if result {
            release_active_capacity(&mut tx, worker_id).await?;
            sqlx::query("DELETE FROM ci_port_leases WHERE preview_id = $1")
                .bind(preview_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE ci_preview_aliases SET state = 'ready', updated_at = now()
                 WHERE id = $1 AND runtime_generation = $2 AND state = 'booting'",
            )
            .bind(preview_id)
            .bind(generation)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(result)
    }

    pub async fn update_preview_settings(
        &self,
        preview_id: Uuid,
        settings: &serde_json::Value,
    ) -> Result<bool> {
        if !settings.is_object() || serde_json::to_vec(settings)?.len() > 4096 {
            bail!("CI preview settings must be a small JSON object");
        }
        let result = sqlx::query(
            "UPDATE ci_preview_aliases SET settings = $2, updated_at = now()
             WHERE id = $1 AND expires_at > now() AND state NOT IN ('expired','failed')",
        )
        .bind(preview_id)
        .bind(settings)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn preview_settings(&self, preview_id: Uuid) -> Result<Option<serde_json::Value>> {
        Ok(sqlx::query_scalar(
            "SELECT settings FROM ci_preview_aliases WHERE id = $1 AND expires_at > now()",
        )
        .bind(preview_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn preview_reported_players(
        &self,
        preview_id: Uuid,
    ) -> Result<Vec<serde_json::Value>> {
        Ok(sqlx::query_scalar("SELECT runtime.reported_players FROM ci_runtime_placements runtime JOIN ci_preview_aliases alias ON alias.id = runtime.preview_id WHERE runtime.preview_id = $1 AND alias.expires_at > now()")
            .bind(preview_id).fetch_optional(&self.pool).await?.unwrap_or_default())
    }

    pub async fn report_players(
        &self,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
        players: &serde_json::Value,
    ) -> Result<bool> {
        if !players.is_array() || serde_json::to_vec(players)?.len() > 64 * 1024 {
            bail!("invalid bounded CI player presence report");
        }
        let result = sqlx::query(
            "UPDATE ci_runtime_placements runtime SET reported_players = $5,
                    last_activity_at = CASE WHEN jsonb_array_length($5) > 0 THEN now() ELSE last_activity_at END,
                    updated_at = now()
             FROM ci_preview_aliases alias WHERE runtime.preview_id = $1 AND alias.id = runtime.preview_id
               AND runtime.worker_id = $2 AND runtime.generation = $3 AND runtime.lease_token = $4
               AND runtime.active AND runtime.state = 'running' AND alias.state = 'running'
               AND alias.expires_at > now()",
        )
        .bind(preview_id).bind(worker_id).bind(generation).bind(lease_token).bind(players)
        .execute(&self.pool).await?;
        if result.rows_affected() == 1
            && players
                .as_array()
                .is_some_and(|players| !players.is_empty())
        {
            self.touch_player_activity(preview_id).await?;
        }
        Ok(result.rows_affected() == 1)
    }

    pub async fn request_stop(&self, preview_id: Uuid) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "UPDATE ci_runtime_placements SET state = 'stopping', updated_at = now()
             WHERE preview_id = $1 AND active AND state = 'running' AND EXISTS (
                SELECT 1 FROM ci_preview_aliases alias WHERE alias.id = $1 AND alias.state = 'running' AND alias.expires_at > now())",
        ).bind(preview_id).execute(&mut *tx).await?;
        if result.rows_affected() == 1 {
            sqlx::query("UPDATE ci_preview_aliases SET state = 'stopping', updated_at = now() WHERE id = $1 AND state = 'running'")
                .bind(preview_id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(result.rows_affected() == 1)
    }

    /// Player traffic extends both preview and artifact retention and becomes
    /// the authoritative idle timer input.
    pub async fn touch_player_activity(&self, preview_id: Uuid) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let build_id: Option<Uuid> = sqlx::query_scalar(
            "UPDATE ci_preview_aliases SET expires_at = now() + interval '336 hours', updated_at = now()
             WHERE id = $1 AND state = 'running' AND expires_at > now() RETURNING build_id",
        )
        .bind(preview_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(build_id) = build_id {
            sqlx::query(
                "UPDATE ci_runtime_placements SET last_activity_at = now(), updated_at = now()
                 WHERE preview_id = $1 AND active AND state = 'running'",
            )
            .bind(preview_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE ci_builds SET expires_at = CASE WHEN retention_policy = 'permanent' THEN NULL ELSE GREATEST(COALESCE(expires_at, now()), now() + interval '336 hours') END, updated_at = now()
                 WHERE id = $1",
            )
            .bind(build_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE ci_artifacts artifact SET expires_at = CASE WHEN artifact.expires_at IS NULL THEN NULL ELSE GREATEST(artifact.expires_at, now() + interval '336 hours') END
                 FROM ci_builds build WHERE build.id = $1 AND artifact.digest = build.artifact_digest",
            )
            .bind(build_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(true);
        }
        tx.commit().await?;
        Ok(false)
    }

    /// Claim idle runtimes using SKIP LOCKED to allow multiple maintenance
    /// processes without duplicate stops.
    pub async fn claim_idle(
        &self,
        idle_for: Duration,
        limit: i64,
    ) -> Result<Vec<CiRuntimePlacement>> {
        if idle_for <= Duration::zero() || !(1..=128).contains(&limit) {
            bail!("invalid CI idle sweep bounds");
        }
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query_as::<_, CiRuntimeRow>(
            "WITH candidates AS (
                SELECT runtime.preview_id FROM ci_runtime_placements runtime
                JOIN ci_preview_aliases alias ON alias.id = runtime.preview_id
                WHERE runtime.active AND runtime.state = 'running'
                  AND runtime.last_activity_at <= now() - $1::interval
                  AND alias.expires_at > now()
                ORDER BY runtime.last_activity_at
                LIMIT $2 FOR UPDATE OF runtime SKIP LOCKED
             )
             UPDATE ci_runtime_placements runtime SET state = 'stopping', updated_at = now()
             FROM candidates, ci_preview_aliases alias, ci_builds build, workers worker
             WHERE runtime.preview_id = candidates.preview_id AND alias.id = runtime.preview_id
               AND build.id = alias.build_id AND worker.id = runtime.worker_id
             RETURNING runtime.preview_id, alias.repository, alias.commit_sha, alias.build_id,
                       build.artifact_digest, alias.hostname, runtime.worker_id,
                       worker.address AS worker_address, worker.signal_address,
                       runtime.generation, runtime.lease_token, runtime.java_port,
                       runtime.bedrock_port, runtime.signal_port, runtime.state, runtime.active, runtime.world_initialized,
                       runtime.last_activity_at, alias.expires_at",
        )
        .bind(format!("{} seconds", idle_for.num_seconds()))
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        let ids = rows.iter().map(|row| row.preview_id).collect::<Vec<_>>();
        if !ids.is_empty() {
            sqlx::query("UPDATE ci_preview_aliases SET state = 'stopping', updated_at = now() WHERE id = ANY($1) AND state = 'running'")
                .bind(&ids).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Return the stopped preview to its built-ready state while preserving its
    /// durable worker world. Active quota and port leases are released.
    pub async fn complete_idle_stop(
        &self,
        placement: &CiRuntimePlacement,
        stopped: bool,
    ) -> Result<bool> {
        if !stopped {
            bail!("cannot release a CI runtime placement before worker teardown succeeds");
        }
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            "UPDATE ci_runtime_placements SET state = 'stopped', active = false, capacity_reserved = false, snapshot_requested_at = NULL, updated_at = now()
             WHERE preview_id = $1 AND worker_id = $2 AND generation = $3
               AND lease_token = $4 AND state = 'stopping' AND active",
        )
        .bind(placement.preview_id)
        .bind(placement.worker_id)
        .bind(placement.generation)
        .bind(placement.lease_token)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if updated {
            release_active_capacity(&mut tx, placement.worker_id).await?;
            sqlx::query("DELETE FROM ci_port_leases WHERE preview_id = $1")
                .bind(placement.preview_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE ci_preview_aliases SET state = 'ready', updated_at = now()
                 WHERE id = $1 AND runtime_generation = $2 AND state IN ('running', 'stopping')",
            )
            .bind(placement.preview_id)
            .bind(placement.generation)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(updated)
    }

    pub async fn release_idle_claim(&self, placement: &CiRuntimePlacement) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "UPDATE ci_runtime_placements SET state = 'running', updated_at = now()
             WHERE preview_id = $1 AND worker_id = $2 AND generation = $3
               AND lease_token = $4 AND state = 'stopping' AND active",
        )
        .bind(placement.preview_id)
        .bind(placement.worker_id)
        .bind(placement.generation)
        .bind(placement.lease_token)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 1 {
            sqlx::query(
                "UPDATE ci_preview_aliases SET state='running',updated_at=now()
                 WHERE id=$1 AND runtime_generation=$2 AND state='stopping'",
            )
            .bind(placement.preview_id)
            .bind(placement.generation)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(result.rows_affected() == 1)
    }

    /// Mark expired aliases and return their exact worker/generation leases for
    /// forced teardown. Database metadata is retained until the worker confirms
    /// deletion, so retries cannot orphan a local world.
    pub async fn claim_expired(
        &self,
        now: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<ExpiredCiRuntime>> {
        if !(1..=128).contains(&limit) {
            bail!("invalid CI expiry sweep limit");
        }
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query_as::<_, ExpiredRuntimeRow>(
            "WITH candidates AS (
                SELECT alias.id FROM ci_preview_aliases alias
                JOIN ci_runtime_placements runtime ON runtime.preview_id = alias.id
                WHERE alias.expires_at <= $1 AND runtime.state <> 'deleting'
                ORDER BY alias.expires_at LIMIT $2
                FOR UPDATE OF alias, runtime SKIP LOCKED
             )
             UPDATE ci_runtime_placements runtime SET state = 'deleting', updated_at = now()
             FROM candidates, ci_preview_aliases alias, ci_builds build, workers worker
             WHERE runtime.preview_id = candidates.id AND alias.id = runtime.preview_id
               AND build.id = alias.build_id AND worker.id = runtime.worker_id
             RETURNING runtime.preview_id, alias.repository, alias.commit_sha, alias.build_id,
                       build.artifact_digest, alias.hostname, runtime.worker_id,
                       worker.address AS worker_address, worker.signal_address,
                       runtime.generation, runtime.lease_token, runtime.java_port,
                       runtime.bedrock_port, runtime.signal_port, runtime.state, runtime.active, runtime.world_initialized,
                       runtime.last_activity_at, alias.expires_at, runtime.capability_hash,
                       NULL::uuid AS stale_cleanup_claim_token",
        )
        .bind(now)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE ci_preview_aliases SET state = 'expired', updated_at = now()
             WHERE expires_at <= $1 AND id IN (SELECT preview_id FROM ci_runtime_placements WHERE state = 'deleting')",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        let mut expired = Vec::with_capacity(rows.len());
        for row in rows {
            let capability_hash = row.capability_hash.clone();
            expired.push(ExpiredCiRuntime {
                placement: row.placement()?,
                capability_hash,
                stale_cleanup_claim_token: None,
            });
        }
        tx.commit().await?;
        Ok(expired)
    }

    /// Claim expired deletion tombstones whose owning worker has been offline.
    /// The object store can then remove the shared recovery point and finish the
    /// durable deletion without waiting for that worker to return. Its local
    /// quota volume is treated as an orphan and removed on that worker's next
    /// authenticated reconciliation.
    pub async fn claim_stale_expired(&self, limit: i64) -> Result<Vec<ExpiredCiRuntime>> {
        if !(1..=128).contains(&limit) {
            bail!("invalid stale CI deletion sweep limit");
        }
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query_as::<_, ExpiredRuntimeRow>(
            "WITH candidates AS (
                SELECT runtime.preview_id FROM ci_runtime_placements runtime
                JOIN ci_preview_aliases alias ON alias.id=runtime.preview_id
                JOIN workers worker ON worker.id=runtime.worker_id
                WHERE runtime.state='deleting' AND alias.state='expired'
                  AND worker.heartbeat_at <= now()-interval '30 seconds'
                  AND (runtime.stale_cleanup_claimed_at IS NULL
                       OR runtime.stale_cleanup_claimed_at <= now()-interval '10 minutes')
                ORDER BY worker.heartbeat_at, alias.expires_at, runtime.preview_id
                LIMIT $1 FOR UPDATE OF runtime SKIP LOCKED
             )
             UPDATE ci_runtime_placements runtime
             SET stale_cleanup_claimed_at=now(), stale_cleanup_claim_token=gen_random_uuid(), updated_at=now()
             FROM candidates, ci_preview_aliases alias, ci_builds build, workers worker
             WHERE runtime.preview_id=candidates.preview_id AND alias.id=runtime.preview_id
               AND build.id=alias.build_id AND worker.id=runtime.worker_id
             RETURNING runtime.preview_id, alias.repository, alias.commit_sha, alias.build_id,
                       build.artifact_digest, alias.hostname, runtime.worker_id,
                       worker.address AS worker_address, worker.signal_address,
                       runtime.generation, runtime.lease_token, runtime.java_port,
                       runtime.bedrock_port, runtime.signal_port, runtime.state, runtime.active, runtime.world_initialized,
                       runtime.last_activity_at, alias.expires_at, runtime.capability_hash,
                       runtime.stale_cleanup_claim_token",
        )
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        let mut expired = Vec::with_capacity(rows.len());
        for row in rows {
            let capability_hash = row.capability_hash.clone();
            let stale_cleanup_claim_token = row.stale_cleanup_claim_token;
            expired.push(ExpiredCiRuntime {
                placement: row.placement()?,
                capability_hash,
                stale_cleanup_claim_token,
            });
        }
        tx.commit().await?;
        Ok(expired)
    }

    pub async fn release_stale_expiry_claim(
        &self,
        placement: &CiRuntimePlacement,
        claim_token: Uuid,
    ) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE ci_runtime_placements SET stale_cleanup_claimed_at=NULL,
                    stale_cleanup_claim_token=NULL, updated_at=now()
             WHERE preview_id=$1 AND worker_id=$2 AND generation=$3 AND lease_token=$4
               AND state='deleting' AND stale_cleanup_claim_token=$5",
        )
        .bind(placement.preview_id)
        .bind(placement.worker_id)
        .bind(placement.generation)
        .bind(placement.lease_token)
        .bind(claim_token)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn release_expiry_claim(&self, placement: &CiRuntimePlacement) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE ci_runtime_placements SET state = CASE WHEN active THEN 'stopping' ELSE 'stopped' END,
                    updated_at = now()
             WHERE preview_id = $1 AND worker_id = $2 AND generation = $3
               AND lease_token = $4 AND state = 'deleting'",
        )
        .bind(placement.preview_id)
        .bind(placement.worker_id)
        .bind(placement.generation)
        .bind(placement.lease_token)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Delete the placement only after matching the expired runtime generation
    /// was stopped and its isolated world removed on its owning worker.
    pub async fn complete_expiry(&self, placement: &CiRuntimePlacement) -> Result<bool> {
        self.complete_expiry_with_claim(placement, None).await
    }

    pub async fn complete_stale_expiry(
        &self,
        placement: &CiRuntimePlacement,
        claim_token: Uuid,
    ) -> Result<bool> {
        self.complete_expiry_with_claim(placement, Some(claim_token))
            .await
    }

    async fn complete_expiry_with_claim(
        &self,
        placement: &CiRuntimePlacement,
        stale_cleanup_claim_token: Option<Uuid>,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let capacity_reserved: bool = sqlx::query_scalar(
            "SELECT capacity_reserved FROM ci_runtime_placements
             WHERE preview_id=$1 AND worker_id=$2 AND generation=$3
               AND lease_token=$4 AND state='deleting'
               AND stale_cleanup_claim_token IS NOT DISTINCT FROM $5 FOR UPDATE",
        )
        .bind(placement.preview_id)
        .bind(placement.worker_id)
        .bind(placement.generation)
        .bind(placement.lease_token)
        .bind(stale_cleanup_claim_token)
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or(false);
        let removed = sqlx::query(
            "DELETE FROM ci_runtime_placements runtime USING ci_preview_aliases alias
             WHERE runtime.preview_id = $1 AND runtime.worker_id = $2
               AND runtime.generation = $3 AND runtime.lease_token = $4
               AND alias.id = runtime.preview_id AND alias.state = 'expired' AND runtime.state = 'deleting'
               AND runtime.stale_cleanup_claim_token IS NOT DISTINCT FROM $5",
        )
        .bind(placement.preview_id)
        .bind(placement.worker_id)
        .bind(placement.generation)
        .bind(placement.lease_token)
        .bind(stale_cleanup_claim_token)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if removed {
            sqlx::query("DELETE FROM ci_port_leases WHERE preview_id = $1")
                .bind(placement.preview_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE workers SET reserved_memory_bytes = GREATEST(0, reserved_memory_bytes - $2),
                    reserved_disk_bytes = GREATEST(0, reserved_disk_bytes - $3),
                    reserved_cpu_millis = GREATEST(0, reserved_cpu_millis - $4)
                 WHERE id = $1",
            )
            .bind(placement.worker_id)
            .bind(if capacity_reserved { CI_RUNTIME_MEMORY_BYTES } else { 0 })
            .bind(CI_RUNTIME_DISK_BYTES)
            .bind(if capacity_reserved { CI_RUNTIME_CPU_MILLIS } else { 0 })
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM ci_preview_aliases WHERE id = $1 AND state = 'expired'")
                .bind(placement.preview_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "DELETE FROM ci_builds build WHERE build.id = $1
                 AND NOT EXISTS (SELECT 1 FROM ci_preview_aliases alias WHERE alias.build_id = build.id)",
            )
            .bind(placement.build_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(removed)
    }

    /// Validate a raw runtime capability without ever returning the stored
    /// digest. Comparison is constant-time and bound to an active generation.
    pub async fn mark_runtime_lost(
        &self,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "UPDATE ci_runtime_placements SET state = 'stopped', active = false, capacity_reserved = false, snapshot_requested_at = NULL, updated_at = now()
             WHERE preview_id = $1 AND worker_id = $2 AND generation = $3
               AND lease_token = $4 AND state = 'running' AND active",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .execute(&mut *tx)
        .await?;
        let changed = result.rows_affected() == 1;
        if changed {
            release_active_capacity(&mut tx, worker_id).await?;
            sqlx::query("DELETE FROM ci_port_leases WHERE preview_id = $1")
                .bind(preview_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE ci_preview_aliases SET state = 'ready', updated_at = now() WHERE id = $1 AND runtime_generation = $2 AND state = 'running'")
                .bind(preview_id).bind(generation).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(changed)
    }

    pub async fn worker_assignments(&self, worker_id: Uuid) -> Result<Vec<CiRuntimePlacement>> {
        let rows = sqlx::query_as::<_, CiRuntimeRow>(
            "SELECT runtime.preview_id, alias.repository, alias.commit_sha, alias.build_id,
                    build.artifact_digest, alias.hostname, runtime.worker_id, worker.address AS worker_address,
                    worker.signal_address, runtime.generation, runtime.lease_token, runtime.java_port,
                    runtime.bedrock_port, runtime.signal_port, runtime.state, runtime.active,
                    runtime.world_initialized, runtime.last_activity_at, alias.expires_at
             FROM ci_runtime_placements runtime
             JOIN ci_preview_aliases alias ON alias.id = runtime.preview_id
             JOIN ci_builds build ON build.id = alias.build_id
             JOIN workers worker ON worker.id = runtime.worker_id
             WHERE runtime.worker_id = $1 AND runtime.state IN ('starting','running','stopping','stopped','deleting')
               AND (runtime.state <> 'deleting' OR runtime.stale_cleanup_claim_token IS NULL)
             ORDER BY runtime.preview_id",
        )
        .bind(worker_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn replace_capability_hash(
        &self,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
        capability_hash: &[u8],
    ) -> Result<bool> {
        if capability_hash.len() != 32 {
            bail!("CI runtime capability hash must be 32 bytes");
        }
        let result = sqlx::query(
            "UPDATE ci_runtime_placements SET capability_hash = $5, updated_at = now()
             WHERE preview_id = $1 AND worker_id = $2 AND generation = $3
               AND lease_token = $4 AND state = 'starting' AND NOT active",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .bind(capability_hash)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Locks the exact assignment and artifact metadata while one response chunk
    /// is released. Assignment rotation, alias expiry refresh, or artifact
    /// cleanup cannot commit until the caller releases the transfer guard.
    pub(crate) async fn lock_artifact_download(
        tx: &mut Transaction<'_, Postgres>,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
    ) -> Result<Option<(String, i64, String)>> {
        Ok(sqlx::query_as(
            "SELECT artifact.object_key, artifact.size_bytes, artifact.digest
             FROM ci_runtime_placements runtime
             JOIN ci_preview_aliases alias ON alias.id = runtime.preview_id
             JOIN ci_builds build ON build.id = alias.build_id
             JOIN ci_artifacts artifact ON artifact.digest = build.artifact_digest
             WHERE runtime.preview_id = $1 AND runtime.worker_id = $2
               AND runtime.generation = $3 AND runtime.lease_token = $4
               AND runtime.state = 'starting' AND NOT runtime.active
               AND alias.expires_at > now() AND (build.expires_at IS NULL OR build.expires_at > now())
               AND (artifact.expires_at IS NULL OR artifact.expires_at > now())
             FOR SHARE OF runtime, alias, build, artifact",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .fetch_optional(&mut **tx)
        .await?)
    }

    pub async fn artifact_object_key(
        &self,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
    ) -> Result<Option<(String, i64, String)>> {
        Ok(sqlx::query_as(
            "SELECT artifact.object_key, artifact.size_bytes, artifact.digest
             FROM ci_runtime_placements runtime
             JOIN ci_preview_aliases alias ON alias.id = runtime.preview_id
             JOIN ci_builds build ON build.id = alias.build_id
             JOIN ci_artifacts artifact ON artifact.digest = build.artifact_digest
             WHERE runtime.preview_id = $1 AND runtime.worker_id = $2
               AND runtime.generation = $3 AND runtime.lease_token = $4
               AND runtime.state = 'starting' AND NOT runtime.active
               AND alias.expires_at > now() AND (build.expires_at IS NULL OR build.expires_at > now())
               AND (artifact.expires_at IS NULL OR artifact.expires_at > now())",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn capability_valid(
        &self,
        preview_id: Uuid,
        generation: i64,
        capability: &str,
    ) -> Result<bool> {
        if capability.is_empty() || capability.len() > 128 {
            return Ok(false);
        }
        let actual = blake3::hash(capability.as_bytes());
        let expected: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT runtime.capability_hash FROM ci_runtime_placements runtime
             JOIN ci_preview_aliases alias ON alias.id = runtime.preview_id
             WHERE runtime.preview_id = $1 AND runtime.generation = $2
               AND runtime.active AND runtime.state = 'running'
               AND alias.runtime_generation = $2 AND alias.state = 'running'
               AND alias.expires_at > now()",
        )
        .bind(preview_id)
        .bind(generation)
        .fetch_optional(&self.pool)
        .await?;
        let Some(expected) = expected else {
            return Ok(false);
        };
        Ok(constant_time_equal(&expected, actual.as_bytes()))
    }

    pub async fn current(&self, preview_id: Uuid) -> Result<Option<CiRuntimePlacement>> {
        let row = sqlx::query_as::<_, CiRuntimeRow>(
            "SELECT runtime.preview_id, alias.repository, alias.commit_sha, alias.build_id,
                    build.artifact_digest, alias.hostname, runtime.worker_id, worker.address AS worker_address,
                    worker.signal_address, runtime.generation,
                    runtime.lease_token, runtime.java_port, runtime.bedrock_port, runtime.signal_port,
                    runtime.state, runtime.active, runtime.world_initialized, runtime.last_activity_at, alias.expires_at
             FROM ci_runtime_placements runtime
             JOIN ci_preview_aliases alias ON alias.id = runtime.preview_id
             JOIN ci_builds build ON build.id = alias.build_id
             JOIN workers worker ON worker.id = runtime.worker_id WHERE runtime.preview_id = $1",
        )
        .bind(preview_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(TryInto::try_into).transpose()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct CiWorldSnapshot {
    pub object_key: String,
    pub size_bytes: i64,
    pub checksum: String,
    pub source_generation: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct CiWorldSnapshotGarbageClaim {
    pub object_key: String,
    pub claim_token: Uuid,
}

fn validate_world_snapshot_key(preview_id: Uuid, object_key: &str) -> Result<()> {
    let parsed = object_store::path::Path::parse(object_key)?;
    if parsed.as_ref() != object_key
        || !object_key.starts_with(&format!("ci-previews/{preview_id}/worlds/"))
    {
        bail!("CI world snapshot object key is outside its preview namespace");
    }
    Ok(())
}

impl CiPreviewRuntimeRepository {
    /// Reclaim running previews whose worker heartbeat has expired, but only
    /// when a durable world checkpoint makes cross-worker recovery safe. The
    /// generation/lease rotation in claim_start fences a recovered old worker.
    pub async fn reconcile_stale_workers(&self, limit: i64) -> Result<u64> {
        if !(1..=128).contains(&limit) {
            bail!("invalid stale CI worker reconciliation limit");
        }
        let preview_ids = sqlx::query_scalar::<_, Uuid>(
            "SELECT runtime.preview_id
             FROM ci_runtime_placements runtime
             JOIN ci_preview_aliases alias ON alias.id=runtime.preview_id
             JOIN workers worker ON worker.id=runtime.worker_id
             WHERE runtime.state IN ('starting','running','stopping')
               AND alias.expires_at > now()
               AND worker.heartbeat_at <= now()-interval '30 seconds'
               AND (
                    (runtime.state IN ('starting','stopping') AND NOT runtime.world_initialized
                     AND NOT runtime.bootstrap_pending AND alias.state IN ('booting','stopping'))
                    OR (runtime.world_initialized AND alias.state IN ('booting','running','stopping')
                        AND EXISTS (
                            SELECT 1 FROM ci_world_snapshots snapshot
                            WHERE snapshot.preview_id=runtime.preview_id
                        ))
               )
             ORDER BY worker.heartbeat_at, runtime.preview_id LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let mut reclaimed = 0;
        for preview_id in preview_ids {
            let capability = rand::random::<[u8; 32]>();
            let hash = blake3::hash(&capability);
            match self.claim_start(preview_id, hash.as_bytes()).await {
                Ok(RuntimeLease::Claimed(_)) => reclaimed += 1,
                Ok(
                    RuntimeLease::Starting(_)
                    | RuntimeLease::Stopping(_)
                    | RuntimeLease::AlreadyRunning(_)
                    | RuntimeLease::NoWorker,
                ) => {}
                // One preview that cannot be rehomed, for example because its
                // alias moved to a build that is not ready, must not stop the
                // sweep for every other preview.
                Err(error) => {
                    tracing::warn!(%preview_id, ?error, "could not rehome a CI preview from a stale worker");
                }
            }
        }
        Ok(reclaimed)
    }

    /// Request a bounded periodic world checkpoint so an unavailable worker can
    /// be replaced without losing more than one cadence window of CI progress.
    pub async fn request_due_world_snapshots(&self, limit: i64) -> Result<u64> {
        if !(1..=128).contains(&limit) {
            bail!("invalid CI world snapshot sweep limit");
        }
        let result = sqlx::query(
            "WITH candidates AS (
                SELECT runtime.preview_id FROM ci_runtime_placements runtime
                JOIN ci_preview_aliases alias ON alias.id=runtime.preview_id
                LEFT JOIN ci_world_snapshots snapshot ON snapshot.preview_id=runtime.preview_id
                WHERE runtime.active AND runtime.state='running' AND alias.state='running'
                  AND alias.expires_at > now() AND runtime.snapshot_requested_at IS NULL
                  AND (snapshot.preview_id IS NULL OR snapshot.created_at <= now() - $2::interval)
                ORDER BY snapshot.created_at NULLS FIRST, runtime.preview_id
                LIMIT $1 FOR UPDATE OF runtime SKIP LOCKED
             )
             UPDATE ci_runtime_placements runtime SET snapshot_requested_at=now(),updated_at=now()
             FROM candidates WHERE runtime.preview_id=candidates.preview_id",
        )
        .bind(limit)
        .bind(format!("{CI_RUNTIME_WORLD_SNAPSHOT_SECONDS} seconds"))
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn request_world_snapshot(&self, preview_id: Uuid) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE ci_runtime_placements SET snapshot_requested_at = COALESCE(snapshot_requested_at, now()), updated_at = now()
             WHERE preview_id = $1 AND state = 'running' AND active
               AND EXISTS (SELECT 1 FROM ci_preview_aliases alias WHERE alias.id = $1 AND alias.state = 'running' AND alias.expires_at > now())",
        )
        .bind(preview_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn world_snapshot_requested(
        &self,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
    ) -> Result<bool> {
        sqlx::query_scalar(
            "SELECT snapshot_requested_at IS NOT NULL FROM ci_runtime_placements
             WHERE preview_id = $1 AND worker_id = $2 AND generation = $3 AND lease_token = $4
               AND state = 'running' AND active",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .fetch_optional(&self.pool)
        .await?
        .context("CI snapshot request has a stale runtime assignment")
    }

    pub async fn complete_world_snapshot_request(
        &self,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
    ) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE ci_runtime_placements SET snapshot_requested_at=NULL, updated_at=now()
             WHERE preview_id=$1 AND worker_id=$2 AND generation=$3 AND lease_token=$4
               AND state='running' AND active AND snapshot_requested_at IS NOT NULL
               AND EXISTS (SELECT 1 FROM ci_world_snapshots WHERE preview_id=$1)",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn bootstrap_world_snapshot_allowed(
        &self,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
    ) -> Result<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM ci_runtime_placements runtime
             JOIN ci_preview_aliases alias ON alias.id=runtime.preview_id
             WHERE runtime.preview_id=$1 AND runtime.worker_id=$2 AND runtime.generation=$3
               AND runtime.lease_token=$4 AND runtime.state='starting' AND NOT runtime.active
               AND runtime.bootstrap_pending AND NOT runtime.world_initialized
               AND alias.state='booting'
               AND NOT EXISTS (SELECT 1 FROM ci_world_snapshots snapshot WHERE snapshot.preview_id=$1))",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .fetch_one(&self.pool)
        .await?)
    }

    /// Locks the assignment and snapshot record through delivery of one body
    /// chunk so a stop, reassignment, or snapshot replacement cannot race the
    /// authorization check and chunk release.
    pub(crate) async fn lock_world_snapshot_download(
        tx: &mut Transaction<'_, Postgres>,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
    ) -> Result<Option<CiWorldSnapshot>> {
        Ok(sqlx::query_as::<_, CiWorldSnapshot>(
            "SELECT snapshot.object_key, snapshot.size_bytes, snapshot.checksum,
                    snapshot.source_generation
             FROM ci_runtime_placements runtime
             JOIN ci_preview_aliases alias ON alias.id = runtime.preview_id
             JOIN ci_world_snapshots snapshot ON snapshot.preview_id = runtime.preview_id
             WHERE runtime.preview_id = $1 AND runtime.worker_id = $2
               AND runtime.generation = $3 AND runtime.lease_token = $4
               AND ((runtime.state = 'starting' AND NOT runtime.active)
                    OR (runtime.state = 'stopping' AND runtime.active))
               AND alias.expires_at > now()
             FOR SHARE OF runtime, alias, snapshot",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .fetch_optional(&mut **tx)
        .await?)
    }

    pub async fn world_snapshot_metadata(
        &self,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
    ) -> Result<Option<CiWorldSnapshot>> {
        let authorized: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM ci_runtime_placements WHERE preview_id=$1 AND worker_id=$2
             AND generation=$3 AND lease_token=$4 AND ((state='starting' AND NOT active) OR (state='stopping' AND active)))",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .fetch_one(&self.pool)
        .await?;
        if !authorized {
            bail!("CI world snapshot request has a stale worker assignment");
        }
        Ok(sqlx::query_as::<_, CiWorldSnapshot>(
            "SELECT object_key, size_bytes, checksum, source_generation
             FROM ci_world_snapshots WHERE preview_id=$1",
        )
        .bind(preview_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn publish_world_snapshot(
        &self,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
        object_key: &str,
        size_bytes: i64,
        checksum: &str,
    ) -> Result<()> {
        if !(1..=CI_RUNTIME_DISK_BYTES).contains(&size_bytes)
            || checksum.len() != 64
            || !checksum
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            bail!("CI world snapshot metadata is invalid");
        }
        validate_world_snapshot_key(preview_id, object_key)?;
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query_as::<_, (String, bool, bool)>(
            "SELECT runtime.state::text, runtime.snapshot_requested_at IS NOT NULL,
                    runtime.bootstrap_pending AND NOT runtime.world_initialized AND NOT EXISTS (
                        SELECT 1 FROM ci_world_snapshots snapshot WHERE snapshot.preview_id=runtime.preview_id
                    )
             FROM ci_runtime_placements runtime JOIN ci_preview_aliases alias ON alias.id=runtime.preview_id
             WHERE runtime.preview_id=$1 AND runtime.worker_id=$2 AND runtime.generation=$3
               AND runtime.lease_token=$4
               AND ((runtime.state='stopping' AND alias.state IN ('stopping','expired'))
                    OR (runtime.state='running' AND runtime.active AND runtime.snapshot_requested_at IS NOT NULL
                        AND alias.state='running')
                    OR (runtime.state='starting' AND NOT runtime.active AND alias.state='booting'
                        AND runtime.bootstrap_pending AND NOT runtime.world_initialized
                        AND NOT EXISTS (
                            SELECT 1 FROM ci_world_snapshots snapshot WHERE snapshot.preview_id=runtime.preview_id
                        )))
             FOR UPDATE OF runtime",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((state, requested_at, bootstrap_checkpoint)) = row else {
            bail!("CI world snapshot worker assignment is stale");
        };
        if state != "stopping"
            && !(state == "running" && requested_at)
            && !(state == "starting" && bootstrap_checkpoint)
        {
            bail!("CI world snapshot is not requested for this runtime state");
        }
        let old: Option<String> = sqlx::query_scalar(
            "SELECT object_key FROM ci_world_snapshots WHERE preview_id=$1 FOR UPDATE",
        )
        .bind(preview_id)
        .fetch_optional(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO ci_world_snapshots(preview_id, object_key, size_bytes, checksum, source_generation)
             VALUES($1,$2,$3,$4,$5)
             ON CONFLICT(preview_id) DO UPDATE SET object_key=EXCLUDED.object_key,
                size_bytes=EXCLUDED.size_bytes, checksum=EXCLUDED.checksum,
                source_generation=EXCLUDED.source_generation, created_at=now()",
        )
        .bind(preview_id)
        .bind(object_key)
        .bind(size_bytes)
        .bind(checksum)
        .bind(generation)
        .execute(&mut *tx)
        .await?;
        if state == "starting" {
            let checkpoint_published = sqlx::query(
                "UPDATE ci_runtime_placements SET world_initialized=true, bootstrap_pending=false,
                        updated_at=now()
                 WHERE preview_id=$1 AND worker_id=$2 AND generation=$3 AND lease_token=$4
                   AND state='starting' AND NOT active AND bootstrap_pending
                   AND NOT world_initialized",
            )
            .bind(preview_id)
            .bind(worker_id)
            .bind(generation)
            .bind(lease_token)
            .execute(&mut *tx)
            .await?
            .rows_affected()
                == 1;
            if !checkpoint_published {
                bail!("CI bootstrap checkpoint lost its generation fence");
            }
        }
        if let Some(old_key) = old.filter(|old_key| old_key != object_key) {
            validate_world_snapshot_key(preview_id, &old_key)?;
            sqlx::query(
                "INSERT INTO ci_world_snapshot_gc(object_key) VALUES($1)
                 ON CONFLICT(object_key) DO NOTHING",
            )
            .bind(old_key)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn enqueue_world_snapshot_garbage(
        &self,
        preview_id: Uuid,
        object_key: &str,
    ) -> Result<()> {
        validate_world_snapshot_key(preview_id, object_key)?;
        sqlx::query(
            "INSERT INTO ci_world_snapshot_gc(object_key) VALUES($1)
             ON CONFLICT(object_key) DO NOTHING",
        )
        .bind(object_key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn claim_world_snapshot_garbage(
        &self,
        limit: i64,
    ) -> Result<Vec<CiWorldSnapshotGarbageClaim>> {
        if !(1..=128).contains(&limit) {
            bail!("invalid CI world snapshot GC claim limit");
        }
        let mut tx = self.pool.begin().await?;
        let claims = sqlx::query_as::<_, CiWorldSnapshotGarbageClaim>(
            "WITH candidates AS (
                SELECT object_key FROM ci_world_snapshot_gc
                WHERE claim_token IS NULL OR claimed_at <= now()-interval '10 minutes'
                ORDER BY queued_at, object_key
                LIMIT $1 FOR UPDATE SKIP LOCKED
             )
             UPDATE ci_world_snapshot_gc garbage
             SET claim_token=gen_random_uuid(), claimed_at=now()
             FROM candidates
             WHERE garbage.object_key=candidates.object_key
             RETURNING garbage.object_key, garbage.claim_token",
        )
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(claims)
    }

    pub async fn world_snapshot_object_is_current(&self, object_key: &str) -> Result<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM ci_world_snapshots WHERE object_key=$1)",
        )
        .bind(object_key)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn complete_world_snapshot_garbage(
        &self,
        object_key: &str,
        claim_token: Uuid,
    ) -> Result<bool> {
        let result =
            sqlx::query("DELETE FROM ci_world_snapshot_gc WHERE object_key=$1 AND claim_token=$2")
                .bind(object_key)
                .bind(claim_token)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn release_world_snapshot_garbage(
        &self,
        object_key: &str,
        claim_token: Uuid,
    ) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE ci_world_snapshot_gc SET claim_token=NULL, claimed_at=NULL,
                    attempts=attempts+1, last_error='object deletion failed'
             WHERE object_key=$1 AND claim_token=$2",
        )
        .bind(object_key)
        .bind(claim_token)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn world_snapshot_for_deletion(
        &self,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
    ) -> Result<Option<CiWorldSnapshot>> {
        let authorized: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM ci_runtime_placements WHERE preview_id=$1 AND worker_id=$2
             AND generation=$3 AND lease_token=$4 AND state='deleting' AND stale_cleanup_claimed_at IS NULL)",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .fetch_one(&self.pool)
        .await?;
        if !authorized {
            bail!("CI world snapshot deletion assignment is stale");
        }
        Ok(sqlx::query_as::<_, CiWorldSnapshot>(
            "SELECT object_key, size_bytes, checksum, source_generation
             FROM ci_world_snapshots WHERE preview_id=$1",
        )
        .bind(preview_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn world_snapshot_for_stale_deletion(
        &self,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
        stale_cleanup_claim_token: Uuid,
    ) -> Result<Option<CiWorldSnapshot>> {
        let authorized: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM ci_runtime_placements WHERE preview_id=$1 AND worker_id=$2
             AND generation=$3 AND lease_token=$4 AND state='deleting'
             AND stale_cleanup_claim_token=$5)",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .bind(stale_cleanup_claim_token)
        .fetch_one(&self.pool)
        .await?;
        if !authorized {
            bail!("CI stale world snapshot deletion claim is stale");
        }
        Ok(sqlx::query_as::<_, CiWorldSnapshot>(
            "SELECT object_key, size_bytes, checksum, source_generation
             FROM ci_world_snapshots WHERE preview_id=$1",
        )
        .bind(preview_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn complete_world_snapshot_deletion(
        &self,
        preview_id: Uuid,
        worker_id: Uuid,
        generation: i64,
        lease_token: Uuid,
        object_key: &str,
    ) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM ci_world_snapshots snapshot USING ci_runtime_placements runtime
             WHERE snapshot.preview_id=$1 AND snapshot.object_key=$5
               AND runtime.preview_id=$1 AND runtime.worker_id=$2 AND runtime.generation=$3
               AND runtime.lease_token=$4 AND runtime.state='deleting' AND runtime.stale_cleanup_claim_token IS NULL",
        )
        .bind(preview_id)
        .bind(worker_id)
        .bind(generation)
        .bind(lease_token)
        .bind(object_key)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }
}
