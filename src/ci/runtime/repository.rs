use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::{CiRuntimeState, RuntimeLease};

pub const CI_RUNTIME_MEMORY_BYTES: i64 = 1024 * 1024 * 1024;
pub const CI_RUNTIME_DISK_BYTES: i64 = crate::model::SERVER_STORAGE_BYTES;
pub const CI_RUNTIME_CPU_MILLIS: i64 = 1000;
pub const CI_RUNTIME_IDLE_SECONDS: i64 = 300;

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
    pub last_activity_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredCiRuntime {
    pub placement: CiRuntimePlacement,
    pub capability_hash: Vec<u8>,
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
               AND build.stage = 'ready' AND build.expires_at > now()
               AND artifact.expires_at > now()
             FOR UPDATE OF alias, build",
        )
        .bind(preview_id)
        .fetch_optional(&mut *tx)
        .await?
        .context("CI preview is not ready or has expired")?;

        if preview.state == "running" {
            let placement = load_current_placement(&mut tx, preview_id).await?;
            tx.commit().await?;
            return Ok(RuntimeLease::AlreadyRunning(placement));
        }
        if !matches!(preview.state.as_str(), "queued" | "ready" | "booting") {
            bail!("CI preview cannot start from state {}", preview.state);
        }

        let old = load_optional_placement(&mut tx, preview_id).await?;
        let (worker_id, base_port, generation) = match old.as_ref() {
            Some(existing) if existing.state == CiRuntimeState::Starting => {
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
            Some(_) => bail!("the CI preview world is pinned to an unavailable worker"),
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
                 signal_port, state, active, last_activity_at, capability_hash)
             VALUES ($1,$2,$3,$4,$5,$6,$7,'starting',false,now(),$8)
             ON CONFLICT (preview_id) DO UPDATE SET
                worker_id = EXCLUDED.worker_id, generation = EXCLUDED.generation,
                lease_token = EXCLUDED.lease_token, java_port = EXCLUDED.java_port,
                bedrock_port = EXCLUDED.bedrock_port, signal_port = EXCLUDED.signal_port,
                state = 'starting', active = false, last_activity_at = now(),
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
                 expires_at = now() + interval '72 hours', updated_at = now() WHERE id = $1",
        )
        .bind(preview_id)
        .bind(generation)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE ci_builds SET expires_at = GREATEST(expires_at, now() + interval '72 hours'), updated_at = now()
             WHERE id = $1",
        )
        .bind(preview.build_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE ci_artifacts artifact SET expires_at = GREATEST(artifact.expires_at, now() + interval '72 hours')
             FROM ci_builds build WHERE build.id = $1 AND artifact.digest = build.artifact_digest",
        )
        .bind(preview.build_id)
        .execute(&mut *tx)
        .await?;
        let placement = load_current_placement(&mut tx, preview_id).await?;
        tx.commit().await?;
        Ok(RuntimeLease::Claimed(placement))
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
               AND lease_token = $4 AND state = 'starting' AND EXISTS (
                    SELECT 1 FROM ci_preview_aliases alias
                    JOIN ci_builds build ON build.id = alias.build_id
                    JOIN ci_artifacts artifact ON artifact.digest = build.artifact_digest
                    WHERE alias.id = $1 AND alias.runtime_generation = $3
                      AND alias.expires_at > now() AND build.expires_at > now()
                      AND artifact.expires_at > now())",
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
            sqlx::query(
                "UPDATE ci_preview_aliases SET state = 'running', updated_at = now()
                 WHERE id = $1 AND runtime_generation = $2 AND state = 'booting'",
            )
            .bind(preview_id)
            .bind(generation)
            .execute(&mut *tx)
            .await?;
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
            "UPDATE ci_runtime_placements SET state = 'stopped', active = false,
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
            "UPDATE ci_preview_aliases SET expires_at = now() + interval '72 hours', updated_at = now()
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
                "UPDATE ci_builds SET expires_at = GREATEST(expires_at, now() + interval '72 hours'), updated_at = now()
                 WHERE id = $1",
            )
            .bind(build_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE ci_artifacts artifact SET expires_at = GREATEST(artifact.expires_at, now() + interval '72 hours')
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
                       runtime.bedrock_port, runtime.signal_port, runtime.state, runtime.active,
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
            "UPDATE ci_runtime_placements SET state = 'stopped', active = false, updated_at = now()
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
        let result = sqlx::query(
            "UPDATE ci_runtime_placements SET state = 'running', updated_at = now()
             WHERE preview_id = $1 AND worker_id = $2 AND generation = $3
               AND lease_token = $4 AND state = 'stopping' AND active",
        )
        .bind(placement.preview_id)
        .bind(placement.worker_id)
        .bind(placement.generation)
        .bind(placement.lease_token)
        .execute(&self.pool)
        .await?;
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
                       runtime.bedrock_port, runtime.signal_port, runtime.state, runtime.active,
                       runtime.last_activity_at, alias.expires_at, runtime.capability_hash",
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
            });
        }
        tx.commit().await?;
        Ok(expired)
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
        let mut tx = self.pool.begin().await?;
        let removed = sqlx::query(
            "DELETE FROM ci_runtime_placements runtime USING ci_preview_aliases alias
             WHERE runtime.preview_id = $1 AND runtime.worker_id = $2
               AND runtime.generation = $3 AND runtime.lease_token = $4
               AND alias.id = runtime.preview_id AND alias.state = 'expired' AND runtime.state = 'deleting'",
        )
        .bind(placement.preview_id)
        .bind(placement.worker_id)
        .bind(placement.generation)
        .bind(placement.lease_token)
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
            .bind(if placement.active { CI_RUNTIME_MEMORY_BYTES } else { 0 })
            .bind(CI_RUNTIME_DISK_BYTES)
            .bind(if placement.active { CI_RUNTIME_CPU_MILLIS } else { 0 })
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
            "UPDATE ci_runtime_placements SET state = 'stopped', active = false, updated_at = now()
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
                    runtime.last_activity_at, alias.expires_at
             FROM ci_runtime_placements runtime
             JOIN ci_preview_aliases alias ON alias.id = runtime.preview_id
             JOIN ci_builds build ON build.id = alias.build_id
             JOIN workers worker ON worker.id = runtime.worker_id
             WHERE runtime.worker_id = $1 AND runtime.state IN ('starting','running','stopping','deleting')
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
               AND alias.expires_at > now() AND build.expires_at > now()
               AND artifact.expires_at > now()",
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
                    runtime.state, runtime.active, runtime.last_activity_at, alias.expires_at
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

#[derive(Debug, sqlx::FromRow)]
struct PreviewRuntimeRow {
    state: String,
    build_id: Uuid,
    target_architecture: String,
}

#[derive(Debug, sqlx::FromRow)]
struct CiRuntimeRow {
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
    last_activity_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}
impl TryFrom<CiRuntimeRow> for CiRuntimePlacement {
    type Error = anyhow::Error;
    fn try_from(row: CiRuntimeRow) -> Result<Self> {
        Ok(Self {
            preview_id: row.preview_id,
            repository: row.repository,
            commit_sha: row.commit_sha,
            build_id: row.build_id,
            artifact_digest: row
                .artifact_digest
                .context("ready CI runtime has no artifact digest")?,
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
            last_activity_at: row.last_activity_at,
            expires_at: row.expires_at,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ExpiredRuntimeRow {
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
    last_activity_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    capability_hash: Vec<u8>,
}
impl ExpiredRuntimeRow {
    fn placement(self) -> Result<CiRuntimePlacement> {
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
            last_activity_at: self.last_activity_at,
            expires_at: self.expires_at,
        }
        .try_into()
    }
}

async fn load_optional_placement(
    tx: &mut Transaction<'_, Postgres>,
    preview_id: Uuid,
) -> Result<Option<CiRuntimePlacement>> {
    let row = sqlx::query_as::<_, CiRuntimeRow>(
        "SELECT runtime.preview_id, alias.repository, alias.commit_sha, alias.build_id,
                build.artifact_digest, alias.hostname, runtime.worker_id, worker.address AS worker_address,
                worker.signal_address, runtime.generation, runtime.lease_token, runtime.java_port,
                runtime.bedrock_port, runtime.signal_port, runtime.state, runtime.active,
                runtime.last_activity_at, alias.expires_at
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

async fn load_current_placement(
    tx: &mut Transaction<'_, Postgres>,
    preview_id: Uuid,
) -> Result<CiRuntimePlacement> {
    load_optional_placement(tx, preview_id)
        .await?
        .context("CI runtime placement was not persisted")
}

async fn worker_alive(tx: &mut Transaction<'_, Postgres>, worker_id: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM workers WHERE id = $1 AND NOT draining
            AND heartbeat_at > now() - interval '30 seconds')",
    )
    .bind(worker_id)
    .fetch_one(&mut **tx)
    .await?)
}

async fn lock_worker(tx: &mut Transaction<'_, Postgres>, worker_id: Uuid) -> Result<()> {
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

async fn select_worker(
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

async fn choose_ci_port(tx: &mut Transaction<'_, Postgres>, worker_id: Uuid) -> Result<i32> {
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

async fn reserve_ci_ports(
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

async fn reserve_disk(tx: &mut Transaction<'_, Postgres>, worker_id: Uuid) -> Result<()> {
    sqlx::query("UPDATE workers SET reserved_disk_bytes = reserved_disk_bytes + $2 WHERE id = $1")
        .bind(worker_id)
        .bind(CI_RUNTIME_DISK_BYTES)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn reserve_active_capacity(
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

async fn release_active_capacity(
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

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
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
