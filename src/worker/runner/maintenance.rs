use super::WorkerRunner;
use super::*;

impl WorkerRunner {
    pub(super) async fn maintain_online_servers(&mut self) -> Result<()> {
        let server_placements: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT s.id, p.generation
             FROM servers s
             JOIN placements p ON p.server_id = s.id
             WHERE p.worker_id = $1 AND p.active AND s.observed_state = 'online'",
        )
        .bind(self.config.id)
        .fetch_all(&self.pool)
        .await?;

        let idle_server_placements: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT s.id, p.generation
             FROM servers s
             JOIN placements p ON p.server_id = s.id
             WHERE p.worker_id = $1 AND p.generation = s.generation
               AND s.desired_state = 'running'
               AND s.observed_state IN ('online', 'snapshotting')",
        )
        .bind(self.config.id)
        .fetch_all(&self.pool)
        .await?;

        self.idle_since.retain(|id, _| {
            idle_server_placements
                .iter()
                .any(|(server_id, _)| server_id == id)
        });
        self.last_snapshot.retain(|id, _| {
            server_placements
                .iter()
                .any(|(server_id, _)| server_id == id)
        });
        for (server_id, generation) in server_placements {
            let server_dir = self.server_dir(server_id);
            if !self.runtime.server_is_running(server_id).await? {
                tracing::error!(%server_id, "Pumpkin runtime stopped unexpectedly");
                self.control_plane
                    .mark_runtime_lost(
                        server_id,
                        generation,
                        "Pumpkin runtime stopped unexpectedly",
                    )
                    .await?;
                clear_players(&self.pool, server_id).await?;
                self.idle_since.remove(&server_id);
                self.last_snapshot.remove(&server_id);
                continue;
            }
            let size = persistent_size(&server_dir)?;
            if size > SERVER_STORAGE_BYTES {
                tracing::warn!(%server_id, size, "storage quota exceeded; stopping server");
                self.control_plane
                    .request_stop(server_id, generation, "storage_quota")
                    .await?;
                continue;
            }

            match self.runtime.player_status(&server_dir).await {
                Ok(status) => {
                    match self.runtime.players(&server_dir).await {
                        Ok(players) => sync_players(&self.pool, server_id, &players).await?,
                        Err(error) => {
                            tracing::warn!(%server_id, ?error, "failed to persist player presence");
                        }
                    }
                    if let Err(error) = sync_whitelist(
                        &self.pool,
                        self.runtime.as_ref(),
                        server_id,
                        &server_dir,
                        false,
                    )
                    .await
                    {
                        tracing::warn!(%server_id, ?error, "failed to synchronize whitelist state");
                    }
                    if let Err(error) = sync_operators(
                        &self.pool,
                        self.runtime.as_ref(),
                        server_id,
                        &server_dir,
                        false,
                    )
                    .await
                    {
                        tracing::warn!(%server_id, ?error, "failed to synchronize owner operators");
                    }
                    let now = Instant::now();
                    let idle = self
                        .idle_since
                        .entry(server_id)
                        .or_insert_with(|| IdleTimer::new(&status, now));
                    if idle.expired(&status, now, self.config.idle_timeout) {
                        self.control_plane
                            .request_stop(server_id, generation, "idle_timeout")
                            .await?;
                        continue;
                    }
                }
                Err(error) => {
                    self.idle_since.remove(&server_id);
                    tracing::warn!(%server_id, ?error, "failed to read player activity; restarting idle observation");
                }
            }

            let last = self
                .last_snapshot
                .entry(server_id)
                .or_insert_with(|| SnapshotSchedule::new(Instant::now()));
            if last.due(Instant::now(), self.config.snapshot_interval) {
                let result = self
                    .control_plane
                    .request_snapshot(server_id, generation, SnapshotReason::Periodic)
                    .await;
                let schedule = self
                    .last_snapshot
                    .get_mut(&server_id)
                    .expect("snapshot schedule initialized");
                match result {
                    Ok(_) => {
                        schedule.succeeded(Instant::now());
                    }
                    Err(error) => {
                        schedule.failed(Instant::now());
                        tracing::warn!(%server_id, ?error, "periodic snapshot deferred; retaining the active runtime");
                        tracing::warn!(%server_id, ?error, "periodic snapshot request was rejected");
                    }
                }
            }
            self.collect_logs(server_id).await?;
        }
        Ok(())
    }

    pub(super) async fn collect_logs(&mut self, server_id: Uuid) -> Result<()> {
        let since = self
            .last_log_read
            .get(&server_id)
            .copied()
            .unwrap_or_else(|| Utc::now() - chrono::Duration::seconds(10));
        let next = Utc::now();
        let lines = self
            .runtime
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

    pub(super) async fn placement(&self, server_id: Uuid) -> Result<PlacementRecord> {
        sqlx::query_as::<_, PlacementRecord>(
            "SELECT server_id, worker_id, generation, lease_token, java_port, bedrock_port, signal_port,
                    active, updated_at
             FROM placements WHERE server_id = $1",
        )
        .bind(server_id)
        .fetch_optional(&self.pool)
        .await?
        .context("placement not found")
    }

    pub(super) async fn mark_cached(&self, server_id: Uuid, generation: i64) -> Result<()> {
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

    pub(super) async fn gc_stale_local_storage(&self) -> Result<()> {
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

    pub(super) fn server_dir(&self, server_id: Uuid) -> PathBuf {
        self.storage.path(server_id)
    }
}
