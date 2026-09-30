use super::WorkerRunner;
use super::*;

impl WorkerRunner {
    pub(super) async fn run_operations(&mut self) -> Result<()> {
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
            if self.last_object_gc.elapsed() >= Duration::from_secs(3600) {
                match self.snapshots.collect_garbage().await {
                    Ok(report) => {
                        tracing::info!(?report, "object-store garbage collection complete")
                    }
                    Err(error) => {
                        tracing::warn!(?error, "object-store garbage collection deferred")
                    }
                }
                self.last_object_gc = Instant::now();
            }
            sleep(Duration::from_secs(2)).await;
        }
    }

    pub(super) async fn reconcile_failed_starts(&self) -> Result<()> {
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
            match self.runtime.abort_start(server_id).await {
                Ok(()) => {
                    mark_runtime_lost(
                        &self.pool,
                        self.config.id,
                        server_id,
                        "Failed-start runtime removal confirmed; retry from the panel",
                    )
                    .await?
                }
                Err(error) => tracing::error!(%server_id, ?error,
                    "failed-start cleanup failed; preserving placement fence and retrying"),
            }
        }
        Ok(())
    }

    pub(super) async fn heartbeat(&self) -> Result<()> {
        send_heartbeat(&self.pool, &self.config).await
    }

    pub(super) async fn execute_operation(&mut self, op: &OperationRecord) -> Result<()> {
        match op.kind {
            OperationKind::Start => self.start(op).await,
            OperationKind::Stop | OperationKind::Restart => self.stop(op).await,
            OperationKind::Snapshot => {
                let snapshot = self
                    .snapshot_running(op.server_id, SnapshotReason::Manual)
                    .await?;
                complete_snapshot(&self.pool, op, snapshot).await?;
                Ok(())
            }
            OperationKind::Restore => self.restore(op).await,
            OperationKind::Delete => self.delete(op).await,
        }
    }

    pub(super) async fn restore(&mut self, op: &OperationRecord) -> Result<()> {
        let placement = self.placement(op.server_id).await?;
        if placement.worker_id != self.config.id || placement.generation != op.generation {
            bail!("restore operation placement does not match worker or generation");
        }
        if placement.active || self.runtime.server_is_running(op.server_id).await? {
            bail!("snapshot restore requires an inactive runtime placement");
        }
        let target_generation = op
            .payload
            .get("target_generation")
            .and_then(serde_json::Value::as_i64)
            .context("restore operation is missing target_generation")?;
        let mut stage = op
            .payload
            .get("stage")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("requested")
            .to_owned();
        let server_dir = self.storage.ensure(op.server_id).await?;

        if stage == "requested" {
            self.storage.reset(op.server_id).await?;
            self.snapshots
                .restore_generation(op.server_id, target_generation, &server_dir)
                .await?;
            update_operation_payload(&self.pool, op, serde_json::json!({"stage":"restored"}))
                .await?;
            stage = "restored".into();
        }

        let (snapshot_generation, snapshot_size) = if stage == "restored" {
            let snapshot = self
                .snapshots
                .create(op.server_id, &server_dir, SnapshotReason::Restore)
                .await?;
            self.mark_cached(op.server_id, snapshot.generation).await?;
            update_operation_payload(
                &self.pool,
                op,
                serde_json::json!({
                    "stage":"snapshotted",
                    "snapshot_generation":snapshot.generation,
                    "snapshot_size":snapshot.size_bytes,
                }),
            )
            .await?;
            (snapshot.generation, snapshot.size_bytes)
        } else if stage == "snapshotted" {
            let generation = op
                .payload
                .get("snapshot_generation")
                .and_then(serde_json::Value::as_i64)
                .context("snapshotted restore is missing snapshot_generation")?;
            let size = op
                .payload
                .get("snapshot_size")
                .and_then(serde_json::Value::as_i64)
                .context("snapshotted restore is missing snapshot_size")?;
            (generation, size)
        } else {
            bail!("unknown restore operation stage {stage:?}");
        };

        complete_restore(
            &self.pool,
            op,
            target_generation,
            snapshot_generation,
            snapshot_size,
        )
        .await?;
        clear_players(&self.pool, op.server_id).await?;
        self.idle_since.remove(&op.server_id);
        self.last_snapshot.remove(&op.server_id);
        Ok(())
    }

    pub(super) async fn delete(&mut self, op: &OperationRecord) -> Result<()> {
        validate_delete(&self.pool, op).await?;

        self.storage.remove(op.server_id).await?;
        self.snapshots.delete_server_manifests(op.server_id).await?;
        complete_delete(&self.pool, op).await?;
        self.idle_since.remove(&op.server_id);
        self.last_snapshot.remove(&op.server_id);
        self.last_log_read.remove(&op.server_id);
        Ok(())
    }

    pub(super) async fn start(&mut self, op: &OperationRecord) -> Result<()> {
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

        let server_dir = match self.runtime.server_generation(op.server_id).await? {
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
        write_whitelist_file(&self.pool, op.server_id, &server_dir).await?;

        sqlx::query(
            "UPDATE servers SET observed_state = 'starting', updated_at = now()
             WHERE id = $1 AND generation = $2",
        )
        .bind(op.server_id)
        .bind(op.generation)
        .execute(&self.pool)
        .await?;
        self.runtime
            .start(op.server_id, &placement, &server_dir, &settings)
            .await?;
        sync_whitelist(
            &self.pool,
            self.runtime.as_ref(),
            op.server_id,
            &server_dir,
            true,
        )
        .await?;
        let storage_bytes = persistent_size(&server_dir)?;
        ensure_storage_limit(storage_bytes)?;
        complete_start(&self.pool, op, storage_bytes).await?;
        self.idle_since.remove(&op.server_id);
        self.last_snapshot
            .insert(op.server_id, SnapshotSchedule::new(Instant::now()));
        Ok(())
    }

    pub(super) async fn stop(&mut self, op: &OperationRecord) -> Result<()> {
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
            if !self.runtime.server_is_running(op.server_id).await? {
                bail!("runtime stopped before the clean shutdown save completed");
            }
            self.runtime.save(&server_dir).await?;
            update_operation_payload(&self.pool, op, serde_json::json!({"stage":"saved"})).await?;
            stage = "saved".into();
        }

        if stage == "saved" {
            if self.runtime.server_is_running(op.server_id).await? {
                self.runtime
                    .graceful_stop(op.server_id, &server_dir)
                    .await?;
            }
            update_operation_payload(&self.pool, op, serde_json::json!({"stage":"stopped"}))
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
            update_operation_payload(
                &self.pool,
                op,
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
        clear_players(&self.pool, op.server_id).await?;
        self.idle_since.remove(&op.server_id);
        self.last_snapshot.remove(&op.server_id);
        Ok(())
    }
}
