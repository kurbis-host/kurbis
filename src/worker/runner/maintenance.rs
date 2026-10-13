use std::collections::HashSet;

use super::WorkerRunner;
use super::*;
use crate::contracts::worker::WorkerAssignment;

impl WorkerRunner {
    pub(super) async fn maintain_online_servers(&mut self) -> Result<()> {
        let states = self.control_plane.worker_servers().await?;
        let online = states
            .iter()
            .filter(|state| state.placement.active && state.observed_state == ObservedState::Online)
            .map(|state| (state.placement.server_id, state.placement.generation))
            .collect::<Vec<_>>();
        let idle = states
            .iter()
            .filter(|state| {
                state.placement.generation > 0
                    && state.desired_state == DesiredState::Running
                    && matches!(
                        state.observed_state,
                        ObservedState::Online | ObservedState::Snapshotting
                    )
            })
            .map(|state| state.placement.server_id)
            .collect::<HashSet<_>>();

        self.idle_since.retain(|id, _| idle.contains(id));
        self.last_snapshot
            .retain(|id, _| online.iter().any(|(server_id, _)| server_id == id));

        for (server_id, generation) in online {
            if let Err(error) = self.maintain_online_server(server_id, generation).await {
                tracing::warn!(%server_id, ?error, "failed to maintain online server; continuing with other servers");
            }
        }
        Ok(())
    }

    async fn maintain_online_server(&mut self, server_id: Uuid, generation: i64) -> Result<()> {
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
            clear_players(&self.control_plane, server_id, generation).await?;
            self.idle_since.remove(&server_id);
            self.last_snapshot.remove(&server_id);
            return Ok(());
        }
        let size = persistent_size(&server_dir)?;
        if size > SERVER_STORAGE_BYTES {
            tracing::warn!(%server_id, size, "storage quota exceeded; stopping server");
            self.control_plane
                .command_stop(server_id, generation, "storage_quota")
                .await?;
            return Ok(());
        }

        match self.runtime.player_status(&server_dir).await {
            Ok(status) => {
                match self.runtime.players(&server_dir).await {
                    Ok(players) => {
                        if let Err(error) =
                            sync_players(&self.control_plane, server_id, generation, &players).await
                        {
                            tracing::warn!(%server_id, ?error, "failed to persist player presence");
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%server_id, ?error, "failed to read player presence")
                    }
                }
                let context = self
                    .control_plane
                    .server_context(server_id, WorkerAssignment::Placement { generation })
                    .await?;
                if let Err(error) = sync_runtime_state(
                    &self.control_plane,
                    self.runtime.as_ref(),
                    &context,
                    &server_dir,
                    false,
                )
                .await
                {
                    tracing::warn!(%server_id, ?error, "failed to synchronize panel state");
                }
                let now = Instant::now();
                let idle = self
                    .idle_since
                    .entry(server_id)
                    .or_insert_with(|| IdleTimer::new(&status, now));
                if idle.expired(&status, now, self.config.idle_timeout) {
                    self.control_plane
                        .command_stop(server_id, generation, "idle_timeout")
                        .await?;
                    return Ok(());
                }
            }
            Err(error) => {
                self.idle_since.remove(&server_id);
                tracing::warn!(%server_id, ?error, "failed to read player activity; restarting idle observation");
            }
        }

        match self
            .runtime
            .runtime_stats(server_id, &server_dir, generation)
            .await
        {
            Ok(stats) => {
                if let Err(error) = self
                    .control_plane
                    .sync_runtime_stats(server_id, &stats)
                    .await
                {
                    tracing::warn!(%server_id, ?error, "failed to persist runtime stats");
                }
            }
            Err(error) => {
                tracing::warn!(%server_id, ?error, "failed to read runtime stats");
            }
        }

        let last = self
            .last_snapshot
            .entry(server_id)
            .or_insert_with(|| SnapshotSchedule::new(Instant::now()));
        if last.due(Instant::now(), self.config.snapshot_interval) {
            match self
                .control_plane
                .command_snapshot(server_id, generation, SnapshotReason::Periodic)
                .await
            {
                Ok(_) => last.succeeded(Instant::now()),
                Err(error) => {
                    last.failed(Instant::now());
                    tracing::warn!(%server_id, ?error, "periodic snapshot deferred; retaining the active runtime");
                }
            }
        }
        if let Err(error) = self.collect_logs(server_id, generation).await {
            tracing::warn!(%server_id, ?error, "failed to collect runtime logs");
        }
        Ok(())
    }

    pub(super) async fn collect_logs(&mut self, server_id: Uuid, generation: i64) -> Result<()> {
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
        self.control_plane
            .append_logs(
                server_id,
                generation,
                &lines.into_iter().take(2000).collect::<Vec<_>>(),
            )
            .await?;
        self.last_log_read.insert(server_id, next);
        Ok(())
    }

    pub(super) async fn context_for_operation(
        &self,
        operation: &OperationRecord,
    ) -> Result<WorkerServerContext> {
        self.control_plane
            .server_context(
                operation.server_id,
                WorkerAssignment::Operation {
                    operation_id: operation.id,
                    generation: operation.generation,
                    attempt: operation.attempt,
                },
            )
            .await
    }

    pub(super) async fn reconcile_failed_starts(&self) -> Result<()> {
        for state in self.control_plane.worker_servers().await? {
            let server_id = state.placement.server_id;
            let generation = state.placement.generation;
            let server_dir = self.server_dir(server_id);
            if state.observed_state != ObservedState::Failed {
                continue;
            }
            if state.failed_start {
                match self.runtime.remove_runtime(server_id).await {
                    Ok(()) => {
                        self.control_plane
                            .mark_runtime_lost(
                                server_id,
                                generation,
                                "Server failed to start; its runtime was removed",
                            )
                            .await?
                    }
                    Err(error) => {
                        tracing::error!(%server_id, ?error, "failed-start cleanup failed; preserving placement fence and retrying")
                    }
                }
            }
            if state.failed_snapshot {
                match self.runtime.server_is_running(server_id).await {
                    Ok(true) => match self.runtime.graceful_stop(server_id, &server_dir).await {
                        Ok(()) => {
                            self.control_plane
                                .mark_runtime_lost(
                                    server_id,
                                    generation,
                                    "Snapshot runtime stopped after retry exhaustion",
                                )
                                .await?;
                            clear_players(&self.control_plane, server_id, generation).await?;
                        }
                        Err(error) => {
                            tracing::error!(%server_id, ?error, "failed snapshot runtime cleanup failed; preserving placement fence")
                        }
                    },
                    Ok(false) => {
                        self.control_plane
                            .mark_runtime_lost(
                                server_id,
                                generation,
                                "Snapshot runtime removal confirmed after retry exhaustion",
                            )
                            .await?
                    }
                    Err(error) => {
                        tracing::error!(%server_id, ?error, "could not verify failed snapshot runtime cleanup")
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) async fn gc_stale_local_storage(&self) -> Result<()> {
        let retained = self
            .control_plane
            .retained_server_ids()
            .await?
            .into_iter()
            .collect::<HashSet<_>>();
        for server_id in self.storage.managed_server_ids().await? {
            if !retained.contains(&server_id) {
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
