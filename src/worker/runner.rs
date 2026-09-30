use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, pool::PoolConnection};
use tokio::time::{Instant, sleep};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::{
    config::WorkerConfig,
    control_plane::lifecycle::{
        Lifecycle, claim_operation, complete_start, complete_stop, healthy_worker_template,
        heartbeat_worker, mark_runtime_lost, recover_running_operations, retry_operation,
    },
    model::{
        OperationKind, OperationRecord, PlacementRecord, SERVER_STORAGE_BYTES, SnapshotReason,
    },
    worker::{docker::DockerRuntime, snapshot::SnapshotManager, storage::StorageManager},
};

pub struct WorkerRunner {
    _worker_lease: PoolConnection<Postgres>,
    config: WorkerConfig,
    pool: PgPool,
    lifecycle: Lifecycle,
    docker: DockerRuntime,
    snapshots: SnapshotManager,
    storage: StorageManager,
    idle_since: HashMap<Uuid, Instant>,
    last_snapshot: HashMap<Uuid, Instant>,
    last_log_read: HashMap<Uuid, DateTime<Utc>>,
    last_local_gc: Instant,
}

impl WorkerRunner {
    pub async fn new(config: WorkerConfig, pool: PgPool) -> Result<Self> {
        let worker_lease = acquire_worker_lease(&pool, config.id).await?;
        let recovered = recover_running_operations(&pool, config.id).await?;
        if recovered > 0 {
            tracing::warn!(worker_id = %config.id, recovered, "recovered interrupted worker operations");
        }
        tokio::fs::create_dir_all(config.root.join("servers")).await?;
        let snapshots = SnapshotManager::new(pool.clone(), &config.object_store).await?;
        let docker = DockerRuntime::new(
            config.pumpkin_image.clone(),
            config.host_control_token.clone(),
        );
        let storage = StorageManager::new(config.root.clone(), config.storage_mode);
        Ok(Self {
            _worker_lease: worker_lease,
            lifecycle: Lifecycle::new(pool.clone()),
            config,
            pool,
            docker,
            snapshots,
            storage,
            idle_since: HashMap::new(),
            last_snapshot: HashMap::new(),
            last_log_read: HashMap::new(),
            last_local_gc: Instant::now(),
        })
    }

    pub async fn run(mut self) -> Result<()> {
        self.gc_stale_local_storage().await?;
        self.heartbeat().await?;
        let pool = self.pool.clone();
        let heartbeat_config = self.config.clone();
        tokio::spawn(async move {
            loop {
                if let Err(error) = send_heartbeat(&pool, &heartbeat_config).await {
                    tracing::error!(?error, "worker heartbeat failed");
                }
                sleep(Duration::from_secs(10)).await;
            }
        });

        loop {
            self.reconcile_failed_starts().await?;
            if let Some(op) = claim_operation(&self.pool, self.config.id).await?
                && let Err(error) = self.execute_operation(&op).await
            {
                tracing::error!(operation_id = %op.id, ?error, "operation failed");
                let error = format!("{error:#}");
                if retry_operation(&self.pool, &op, &error).await? {
                    tracing::warn!(operation_id = %op.id, attempt = op.attempt, "operation scheduled for retry");
                } else {
                    tracing::error!(operation_id = %op.id, "operation exhausted retries");
                }
            }
            self.maintain_online_servers().await?;
            if self.last_local_gc.elapsed() >= Duration::from_secs(60) {
                self.gc_stale_local_storage().await?;
                self.last_local_gc = Instant::now();
            }
            sleep(Duration::from_secs(2)).await;
        }
    }

    async fn reconcile_failed_starts(&self) -> Result<()> {
        let server_ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT s.id FROM servers s JOIN placements p ON p.server_id = s.id
             WHERE p.worker_id = $1 AND p.active AND s.observed_state = 'failed'
               AND EXISTS(SELECT 1 FROM operations o WHERE o.server_id = s.id
                 AND o.generation = p.generation AND o.kind = 'start' AND o.status = 'failed')",
        )
        .bind(self.config.id)
        .fetch_all(&self.pool)
        .await?;
        for server_id in server_ids {
            match self.docker.abort_start(server_id).await {
                Ok(()) => {
                    mark_runtime_lost(
                        &self.pool,
                        self.config.id,
                        server_id,
                        "Failed-start container removal confirmed; retry from the panel",
                    )
                    .await?
                }
                Err(error) => tracing::error!(%server_id, ?error,
                    "failed-start cleanup failed; preserving placement fence and retrying"),
            }
        }
        Ok(())
    }

    async fn heartbeat(&self) -> Result<()> {
        send_heartbeat(&self.pool, &self.config).await
    }

    async fn execute_operation(&mut self, op: &OperationRecord) -> Result<()> {
        match op.kind {
            OperationKind::Start => self.start(op).await,
            OperationKind::Stop | OperationKind::Restart => self.stop(op).await,
            OperationKind::Snapshot => {
                self.snapshot_running(op.server_id, SnapshotReason::Manual)
                    .await?;
                sqlx::query(
                    "UPDATE operations SET status = 'succeeded', updated_at = now() WHERE id = $1",
                )
                .bind(op.id)
                .execute(&self.pool)
                .await?;
                Ok(())
            }
            OperationKind::Delete => self.delete(op).await,
        }
    }

    async fn delete(&mut self, op: &OperationRecord) -> Result<()> {
        let state: Option<(
            crate::model::DesiredState,
            crate::model::ObservedState,
            bool,
        )> = sqlx::query_as(
            "SELECT s.desired_state, s.observed_state,
                        COALESCE(p.active, false)
                 FROM servers s
                 LEFT JOIN placements p ON p.server_id = s.id
                 WHERE s.id = $1",
        )
        .bind(op.server_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((desired, observed, active)) = state else {
            return Ok(());
        };
        if desired != crate::model::DesiredState::Deleted
            || observed != crate::model::ObservedState::Deleted
        {
            bail!("delete operation claimed before server entered deleted state");
        }
        if active {
            bail!("refusing to delete server data while placement is active");
        }

        self.storage.remove(op.server_id).await?;
        self.snapshots.delete_server_manifests(op.server_id).await?;
        sqlx::query("DELETE FROM servers WHERE id = $1 AND desired_state = 'deleted'")
            .bind(op.server_id)
            .execute(&self.pool)
            .await?;
        self.idle_since.remove(&op.server_id);
        self.last_snapshot.remove(&op.server_id);
        self.last_log_read.remove(&op.server_id);
        Ok(())
    }

    async fn start(&mut self, op: &OperationRecord) -> Result<()> {
        let placement = self.placement(op.server_id).await?;
        if placement.worker_id != self.config.id || placement.generation != op.generation {
            bail!("start operation placement does not match worker or generation");
        }
        let (settings, current_snapshot): (serde_json::Value, Option<i64>) = sqlx::query_as(
            "SELECT settings, current_snapshot_generation FROM servers WHERE id = $1 AND generation = $2",
        )
        .bind(op.server_id)
        .bind(op.generation)
        .fetch_optional(&self.pool)
        .await?
        .context("server generation changed before start")?;

        let server_dir = match self.docker.server_generation(op.server_id).await? {
            Some(generation) if generation == op.generation => {
                self.storage.ensure(op.server_id).await?
            }
            Some(generation) => {
                bail!("refusing to replace storage used by running generation {generation}")
            }
            None => {
                crate::worker::persistence::prepare_server_data(
                    &self.pool,
                    self.config.id,
                    op.server_id,
                    current_snapshot,
                    &self.storage,
                    &self.snapshots,
                )
                .await?
            }
        };
        let storage_bytes = persistent_size(&server_dir)?;
        ensure_storage_limit(storage_bytes)?;

        sqlx::query(
            "UPDATE servers SET observed_state = 'starting', updated_at = now()
             WHERE id = $1 AND generation = $2",
        )
        .bind(op.server_id)
        .bind(op.generation)
        .execute(&self.pool)
        .await?;
        self.docker
            .start(op.server_id, &placement, &server_dir, &settings)
            .await?;
        let storage_bytes = persistent_size(&server_dir)?;
        ensure_storage_limit(storage_bytes)?;
        complete_start(&self.pool, op, storage_bytes).await?;
        self.idle_since.remove(&op.server_id);
        self.last_snapshot.insert(op.server_id, Instant::now());
        Ok(())
    }

    async fn stop(&mut self, op: &OperationRecord) -> Result<()> {
        let placement = self.placement(op.server_id).await?;
        if placement.worker_id != self.config.id || placement.generation != op.generation {
            bail!("stop operation placement does not match worker or generation");
        }
        let server_dir = self.server_dir(op.server_id);
        sqlx::query(
            "UPDATE servers SET observed_state = 'stopping', updated_at = now()
             WHERE id = $1 AND generation = $2",
        )
        .bind(op.server_id)
        .bind(op.generation)
        .execute(&self.pool)
        .await?;

        let mut stage = op
            .payload
            .get("stage")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("requested")
            .to_owned();

        if stage == "requested" {
            if !self.docker.server_is_running(op.server_id).await? {
                bail!("runtime stopped before the clean shutdown save completed");
            }
            self.docker.save(&server_dir).await?;
            self.update_operation_payload(op.id, serde_json::json!({"stage":"saved"}))
                .await?;
            stage = "saved".into();
        }

        if stage == "saved" {
            if self.docker.server_is_running(op.server_id).await? {
                self.docker.graceful_stop(op.server_id, &server_dir).await?;
            }
            self.update_operation_payload(op.id, serde_json::json!({"stage":"stopped"}))
                .await?;
            stage = "stopped".into();
        }

        let (snapshot_generation, snapshot_size) = if stage == "stopped" {
            sqlx::query(
                "UPDATE servers SET observed_state = 'snapshotting', updated_at = now()
                 WHERE id = $1 AND generation = $2",
            )
            .bind(op.server_id)
            .bind(op.generation)
            .execute(&self.pool)
            .await?;
            let snapshot = self
                .snapshots
                .create(op.server_id, &server_dir, SnapshotReason::Shutdown)
                .await?;
            self.mark_cached(op.server_id, snapshot.generation).await?;
            self.update_operation_payload(
                op.id,
                serde_json::json!({
                    "stage":"snapshotted",
                    "snapshot_generation":snapshot.generation,
                    "snapshot_size":snapshot.size_bytes
                }),
            )
            .await?;
            (snapshot.generation, snapshot.size_bytes)
        } else if stage == "snapshotted" {
            let generation = op
                .payload
                .get("snapshot_generation")
                .and_then(serde_json::Value::as_i64)
                .context("snapshotted stop operation is missing snapshot_generation")?;
            let size = op
                .payload
                .get("snapshot_size")
                .and_then(serde_json::Value::as_i64)
                .context("snapshotted stop operation is missing snapshot_size")?;
            (generation, size)
        } else {
            bail!("unknown stop operation stage {stage:?}");
        };

        complete_stop(&self.pool, op, snapshot_generation, snapshot_size).await?;
        self.idle_since.remove(&op.server_id);
        self.last_snapshot.remove(&op.server_id);
        Ok(())
    }

    async fn maintain_online_servers(&mut self) -> Result<()> {
        let server_ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT s.id
             FROM servers s
             JOIN placements p ON p.server_id = s.id
             WHERE p.worker_id = $1 AND p.active AND s.observed_state = 'online'",
        )
        .bind(self.config.id)
        .fetch_all(&self.pool)
        .await?;

        self.idle_since.retain(|id, _| server_ids.contains(id));
        self.last_snapshot.retain(|id, _| server_ids.contains(id));
        for server_id in server_ids {
            let server_dir = self.server_dir(server_id);
            if !self.docker.server_is_running(server_id).await? {
                tracing::error!(%server_id, "Pumpkin container stopped unexpectedly");
                mark_runtime_lost(
                    &self.pool,
                    self.config.id,
                    server_id,
                    "Pumpkin container stopped unexpectedly",
                )
                .await?;
                self.idle_since.remove(&server_id);
                self.last_snapshot.remove(&server_id);
                continue;
            }
            let size = persistent_size(&server_dir)?;
            if size > SERVER_STORAGE_BYTES {
                tracing::warn!(%server_id, size, "storage quota exceeded; stopping server");
                self.lifecycle
                    .request_stop(server_id, "storage_quota")
                    .await?;
                continue;
            }

            match self.docker.player_count(&server_dir).await {
                Ok(0) => {
                    let idle_since = self
                        .idle_since
                        .entry(server_id)
                        .or_insert_with(Instant::now);
                    if idle_since.elapsed() >= self.config.idle_timeout {
                        self.lifecycle
                            .request_stop(server_id, "idle_timeout")
                            .await?;
                        continue;
                    }
                }
                Ok(_) => {
                    self.idle_since.remove(&server_id);
                }
                Err(error) => tracing::warn!(%server_id, ?error, "failed to read player count"),
            }

            let last = self
                .last_snapshot
                .entry(server_id)
                .or_insert_with(Instant::now);
            if last.elapsed() >= self.config.snapshot_interval {
                self.snapshot_running(server_id, SnapshotReason::Periodic)
                    .await?;
                self.last_snapshot.insert(server_id, Instant::now());
            }
            self.collect_logs(server_id).await?;
        }
        Ok(())
    }

    async fn snapshot_running(&self, server_id: Uuid, reason: SnapshotReason) -> Result<i64> {
        let server_dir = self.server_dir(server_id);
        self.docker.save(&server_dir).await?;
        let snapshot = self
            .snapshots
            .create(server_id, &server_dir, reason)
            .await?;
        self.mark_cached(server_id, snapshot.generation).await?;
        Ok(snapshot.generation)
    }

    async fn collect_logs(&mut self, server_id: Uuid) -> Result<()> {
        let since = self
            .last_log_read
            .get(&server_id)
            .copied()
            .unwrap_or_else(|| Utc::now() - chrono::Duration::seconds(10));
        let next = Utc::now();
        let lines = self
            .docker
            .logs_since(server_id, &since.to_rfc3339())
            .await?;
        for line in lines.into_iter().take(2000) {
            sqlx::query("INSERT INTO server_logs(server_id, line) VALUES($1, $2)")
                .bind(server_id)
                .bind(line)
                .execute(&self.pool)
                .await?;
        }
        self.last_log_read.insert(server_id, next);
        Ok(())
    }

    async fn placement(&self, server_id: Uuid) -> Result<PlacementRecord> {
        sqlx::query_as::<_, PlacementRecord>(
            "SELECT server_id, worker_id, generation, lease_token, java_port, bedrock_port,
                    active, updated_at
             FROM placements WHERE server_id = $1",
        )
        .bind(server_id)
        .fetch_optional(&self.pool)
        .await?
        .context("placement not found")
    }

    async fn mark_cached(&self, server_id: Uuid, generation: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO worker_snapshot_cache(worker_id, server_id, generation)
             VALUES($1,$2,$3)
             ON CONFLICT(worker_id, server_id) DO UPDATE
               SET generation = EXCLUDED.generation, updated_at = now()",
        )
        .bind(self.config.id)
        .bind(server_id)
        .bind(generation)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn gc_stale_local_storage(&self) -> Result<()> {
        for server_id in self.storage.managed_server_ids().await? {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM servers WHERE id = $1)")
                    .bind(server_id)
                    .fetch_one(&self.pool)
                    .await?;
            if !exists {
                tracing::info!(%server_id, "removing orphaned worker-local server storage");
                self.storage.remove(server_id).await?;
            }
        }
        Ok(())
    }

    async fn update_operation_payload(
        &self,
        operation_id: Uuid,
        patch: serde_json::Value,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE operations SET payload = payload || $2, updated_at = now() WHERE id = $1",
        )
        .bind(operation_id)
        .bind(patch)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    fn server_dir(&self, server_id: Uuid) -> PathBuf {
        self.storage.path(server_id)
    }
}

async fn acquire_worker_lease(pool: &PgPool, worker_id: Uuid) -> Result<PoolConnection<Postgres>> {
    let mut connection = pool.acquire().await?;
    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1, 0))")
        .bind(format!("kurbis-worker:{worker_id}"))
        .fetch_one(&mut *connection)
        .await?;
    if !acquired {
        bail!("another worker process already owns WORKER_ID {worker_id}");
    }
    Ok(connection)
}

async fn send_heartbeat(pool: &PgPool, config: &WorkerConfig) -> Result<()> {
    let worker = healthy_worker_template(
        config.id,
        config.name.clone(),
        config.address.clone(),
        config.total_memory_bytes,
        config.total_disk_bytes,
        config.total_cpu_millis,
        config.port_min,
        config.port_max,
    );
    heartbeat_worker(pool, &worker).await
}

fn persistent_size(root: &Path) -> Result<i64> {
    if !root.exists() {
        return Ok(0);
    }
    let mut bytes = 0_i64;
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(root)?;
        if is_runtime_file(relative) {
            continue;
        }
        bytes = bytes
            .checked_add(i64::try_from(entry.metadata()?.len())?)
            .context("storage size overflow")?;
    }
    Ok(bytes)
}

fn is_runtime_file(relative: &Path) -> bool {
    let name = relative
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or_default();
    matches!(
        name,
        "pumpkin.toml" | "advanced_config.toml" | "nethernet-key.der"
    ) || relative
        .components()
        .next()
        .is_some_and(|v| v.as_os_str() == "logs")
        || relative
            .components()
            .next()
            .is_some_and(|v| v.as_os_str() == "run")
}

fn ensure_storage_limit(bytes: i64) -> Result<()> {
    if bytes > SERVER_STORAGE_BYTES {
        bail!("server storage quota exceeded: {bytes} > {SERVER_STORAGE_BYTES}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_limit_rejects_overage() {
        assert!(ensure_storage_limit(SERVER_STORAGE_BYTES).is_ok());
        assert!(ensure_storage_limit(SERVER_STORAGE_BYTES + 1).is_err());
    }
}
