use super::WorkerRunner;
use super::*;
use crate::contracts::{
    lifecycle::ResetMode,
    worker::{OperationPayload, OperationProgress, OperationStage},
};

impl WorkerRunner {
    pub(super) async fn run_operations(&mut self) -> Result<()> {
        loop {
            self.reconcile_failed_starts().await?;
            if let Some(ci_runtime) = &self.ci_runtime
                && let Err(error) = ci_runtime.reconcile().await
            {
                tracing::error!(?error, "CI preview worker reconciliation failed");
            }
            if let Some(op) = self.control_plane.claim_operation().await?
                && let Err(error) = self.execute_operation(&op).await
            {
                tracing::error!(operation_id = %op.id, ?error, "operation failed");
                let error = format!("{error:#}");
                match self.control_plane.retry_operation(&op, &error).await? {
                    OperationRetryOutcome::Scheduled => {
                        tracing::warn!(operation_id = %op.id, attempt = op.attempt, "operation scheduled for retry");
                    }
                    OperationRetryOutcome::Exhausted => {
                        tracing::error!(operation_id = %op.id, "operation exhausted retries");
                    }
                    OperationRetryOutcome::NoLongerCurrent => {
                        tracing::debug!(operation_id = %op.id, "operation was already finalized or superseded; continuing worker reconciliation");
                    }
                }
            }
            if let Some(command) = self.control_plane.claim_console_command().await?
                && let Err(error) = self.execute_console_command(&command).await
            {
                tracing::warn!(console_command_id = %command.id, ?error, "console command completion failed");
            }
            if let Err(error) = self.maintain_online_servers().await {
                tracing::warn!(?error, "failed to maintain online servers");
            }
            self.spawn_world_export_if_idle().await?;
            if self.last_local_gc.elapsed() >= Duration::from_secs(60) {
                self.gc_stale_local_storage().await?;
                self.last_local_gc = Instant::now();
            }
            sleep(Duration::from_secs(2)).await;
        }
    }

    pub(super) async fn heartbeat(&self) -> Result<()> {
        send_heartbeat(&self.control_plane).await
    }

    pub(super) async fn execute_operation(&mut self, op: &OperationRecord) -> Result<()> {
        let payload: OperationPayload = serde_json::from_value(op.payload.clone())
            .context("worker operation payload does not match its contract")?;
        if !payload.valid_for(op.kind) {
            bail!("worker operation payload has an invalid stage or field combination");
        }
        let snapshots =
            self.snapshots
                .for_operation(op.server_id, op.id, op.generation, op.attempt);
        match op.kind {
            OperationKind::Start => self.start(op, &payload, &snapshots).await,
            OperationKind::Stop | OperationKind::Restart => {
                self.stop(op, &payload, &snapshots).await
            }
            OperationKind::Snapshot => self.snapshot(op, &payload, &snapshots).await,
            OperationKind::Restore => self.restore(op, &payload, &snapshots).await,
            OperationKind::Reset => self.reset(op, &payload, &snapshots).await,
            OperationKind::Delete => self.delete(op, &payload, &snapshots).await,
        }
    }

    async fn execute_console_command(
        &self,
        command: &crate::contracts::worker::WorkerConsoleCommand,
    ) -> Result<()> {
        let execution = async {
            let context = self
                .control_plane
                .server_context(
                    command.server_id,
                    crate::contracts::worker::WorkerAssignment::Placement {
                        generation: command.generation,
                    },
                )
                .await?;
            if context.placement.worker_id != self.config.id
                || context.placement.generation != command.generation
                || !context.placement.active
                || context.observed_state != ObservedState::Online
                || context.desired_state != DesiredState::Running
            {
                bail!("console command assignment is stale or no longer online");
            }
            if self.runtime.server_generation(command.server_id).await? != Some(command.generation)
            {
                bail!("console command runtime generation does not match its assignment");
            }
            self.runtime
                .execute_command(&self.server_dir(command.server_id), &command.command)
                .await
        }
        .await;

        match execution {
            Ok(output) => {
                let mut transcript = Vec::with_capacity(output.len() + 1);
                transcript.push(format!("> /{}", command.command));
                transcript.extend(output.iter().cloned());
                if let Err(error) = self
                    .control_plane
                    .append_logs(command.server_id, command.generation, &transcript)
                    .await
                {
                    tracing::warn!(
                        console_command_id = %command.id,
                        ?error,
                        "console command completed but transcript could not be published to live logs"
                    );
                }
                self.control_plane
                    .complete_console_command(command, true, output, None)
                    .await
            }
            Err(error) => {
                let message = format!("{error:#}");
                let failure_line = format!("! /{} failed: {message}", command.command);
                if let Err(log_error) = self
                    .control_plane
                    .append_logs(command.server_id, command.generation, &[failure_line])
                    .await
                {
                    tracing::debug!(
                        console_command_id = %command.id,
                        ?log_error,
                        "failed console command transcript could not be published"
                    );
                }
                self.control_plane
                    .complete_console_command(command, false, Vec::new(), Some(&message))
                    .await?;
                Err(error)
            }
        }
    }

    async fn snapshot(
        &mut self,
        op: &OperationRecord,
        payload: &OperationPayload,
        snapshots: &SnapshotManager,
    ) -> Result<()> {
        let reason: SnapshotReason = serde_json::from_value(serde_json::Value::String(
            payload
                .reason
                .clone()
                .context("snapshot operation is missing its reason")?,
        ))?;
        let context = self.context_for_operation(op).await?;
        let placement = context.placement.clone();
        if placement.worker_id != self.config.id || placement.generation != op.generation {
            bail!("snapshot operation placement does not match worker or generation");
        }
        let server_dir = self.storage.ensure(op.server_id).await?;
        let mut stage = payload.stage;

        if stage == OperationStage::Deferred {
            self.control_plane.begin_snapshot_retry(op).await?;
            stage = OperationStage::Requested;
        }

        if stage == OperationStage::Requested {
            if self.runtime.server_is_running(op.server_id).await? {
                if let Some(idle) = self.idle_since.get_mut(&op.server_id) {
                    let status = self.runtime.player_status(&server_dir).await?;
                    idle.observe(&status, Instant::now());
                }
                let socket = self.runtime.socket_path(&server_dir)?;
                let runtime_control = self.runtime.control_for(&server_dir).await?;
                for player in self.runtime.players(&server_dir).await? {
                    let _ = runtime_control
                        .message(
                            &socket,
                            player.id,
                            "The server is saving a world snapshot. You will be disconnected briefly.",
                        )
                        .await;
                }
                self.runtime.save(&server_dir).await?;
            }
            self.control_plane
                .update_operation_payload(op, OperationProgress::stage(OperationStage::Saved))
                .await?;
            stage = OperationStage::Saved;
        }

        if stage == OperationStage::Saved {
            if self.runtime.server_is_running(op.server_id).await? {
                self.runtime
                    .graceful_stop(op.server_id, &server_dir)
                    .await?;
            }
            self.control_plane
                .deactivate_placement_after_stop(op)
                .await?;
            self.control_plane
                .update_operation_payload(op, OperationProgress::stage(OperationStage::Stopped))
                .await?;
            stage = OperationStage::Stopped;
        }

        let (snapshot_generation, snapshot_size) = if stage == OperationStage::Stopped {
            if self.runtime.server_is_running(op.server_id).await? {
                bail!("refusing to snapshot while the runtime is active");
            }
            let snapshot = match snapshots.create(op.server_id, &server_dir, reason).await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    self.resume_snapshot_runtime(op, op.server_id, &placement, &server_dir)
                        .await
                        .context("could not resume runtime after snapshot storage failure")?;
                    self.control_plane
                        .defer_snapshot_operation(op, &format!("Snapshot upload failed: {error:#}"))
                        .await?;
                    return Err(error).context("snapshot upload failed; runtime resumed for retry");
                }
            };
            self.control_plane
                .publish_snapshot(op.server_id, op, snapshot.generation, snapshot.size_bytes)
                .await?;
            self.control_plane
                .update_operation_payload(
                    op,
                    OperationProgress::snapshot(
                        OperationStage::Snapshotted,
                        snapshot.generation,
                        snapshot.size_bytes,
                    ),
                )
                .await?;
            stage = OperationStage::Snapshotted;
            (snapshot.generation, snapshot.size_bytes)
        } else if matches!(
            stage,
            OperationStage::Snapshotted | OperationStage::Restarted
        ) {
            (
                payload
                    .snapshot_generation
                    .context("snapshot stage is missing its generation")?,
                payload
                    .snapshot_size
                    .context("snapshot stage is missing its size")?,
            )
        } else {
            bail!("snapshot operation reached invalid stage {stage:?}");
        };

        if stage == OperationStage::Snapshotted {
            self.resume_snapshot_runtime(op, op.server_id, &placement, &server_dir)
                .await?;
            self.control_plane
                .update_operation_payload(op, OperationProgress::stage(OperationStage::Restarted))
                .await?;
            stage = OperationStage::Restarted;
        }

        if stage != OperationStage::Restarted {
            bail!("snapshot operation did not reach its restart stage");
        }
        ensure_storage_limit(snapshot_size)?;
        self.control_plane
            .complete_snapshot(op, snapshot_generation)
            .await?;
        Ok(())
    }

    async fn resume_snapshot_runtime(
        &mut self,
        op: &OperationRecord,
        server_id: Uuid,
        placement: &PlacementRecord,
        server_dir: &std::path::Path,
    ) -> Result<()> {
        if placement.worker_id != self.config.id {
            bail!("snapshot runtime restart belongs to another worker");
        }
        let context = self.context_for_operation(op).await?;
        if !self.runtime.server_is_running(server_id).await? {
            write_whitelist_file(&context, server_dir).await?;
            write_operator_file(&context, server_dir).await?;
        }
        self.ensure_runtime_ready(server_id, placement, server_dir, &context.settings)
            .await?;
        sync_whitelist(
            &self.control_plane,
            self.runtime.as_ref(),
            &context,
            server_dir,
            true,
        )
        .await?;
        sync_operators(
            &self.control_plane,
            self.runtime.as_ref(),
            &context,
            server_dir,
            true,
        )
        .await?;
        let status = self.runtime.player_status(server_dir).await?;
        if let Some(idle) = self.idle_since.get_mut(&server_id) {
            idle.after_snapshot_restart(&status, Instant::now());
        }
        Ok(())
    }

    async fn ensure_runtime_ready(
        &self,
        server_id: Uuid,
        placement: &PlacementRecord,
        server_dir: &std::path::Path,
        settings: &serde_json::Value,
    ) -> Result<()> {
        if !self.runtime.server_is_running(server_id).await? {
            let api_capability = self
                .server_access
                .prepare_plugin(server_id, placement.generation, server_dir)
                .await?;
            self.runtime
                .start(
                    server_id,
                    placement,
                    server_dir,
                    settings,
                    api_capability.as_deref(),
                )
                .await?;
        }
        self.runtime
            .wait_ready(server_id, server_dir, Duration::from_secs(120))
            .await?;
        Ok(())
    }

    pub(super) async fn restore(
        &mut self,
        op: &OperationRecord,
        payload: &OperationPayload,
        snapshots: &SnapshotManager,
    ) -> Result<()> {
        let context = self.context_for_operation(op).await?;
        let placement = context.placement.clone();
        if placement.worker_id != self.config.id || placement.generation != op.generation {
            bail!("restore operation placement does not match worker or generation");
        }
        if placement.active || self.runtime.server_is_running(op.server_id).await? {
            bail!("snapshot restore requires an inactive runtime placement");
        }
        let target_generation = payload
            .target_generation
            .context("restore operation is missing target_generation")?;
        let mut stage = payload.stage;
        let server_dir = self.storage.ensure(op.server_id).await?;

        if stage == OperationStage::Requested {
            self.storage.reset(op.server_id).await?;
            snapshots
                .restore_generation(op.server_id, target_generation, &server_dir)
                .await?;
            write_whitelist_file(&context, &server_dir).await?;
            write_operator_file(&context, &server_dir).await?;
            self.control_plane
                .update_operation_payload(op, OperationProgress::stage(OperationStage::Restored))
                .await?;
            stage = OperationStage::Restored;
        }

        let (snapshot_generation, snapshot_size) = if stage == OperationStage::Restored {
            let snapshot = snapshots
                .create(op.server_id, &server_dir, SnapshotReason::Restore)
                .await?;
            self.control_plane
                .publish_snapshot(op.server_id, op, snapshot.generation, snapshot.size_bytes)
                .await?;
            self.control_plane
                .update_operation_payload(
                    op,
                    OperationProgress::snapshot(
                        OperationStage::Snapshotted,
                        snapshot.generation,
                        snapshot.size_bytes,
                    ),
                )
                .await?;
            (snapshot.generation, snapshot.size_bytes)
        } else if stage == OperationStage::Snapshotted {
            let generation = payload
                .snapshot_generation
                .context("snapshotted restore is missing snapshot_generation")?;
            let size = payload
                .snapshot_size
                .context("snapshotted restore is missing snapshot_size")?;
            (generation, size)
        } else {
            bail!("restore operation reached invalid stage {stage:?}");
        };

        self.control_plane
            .complete_restore(op, target_generation, snapshot_generation, snapshot_size)
            .await?;
        self.idle_since.remove(&op.server_id);
        self.last_snapshot.remove(&op.server_id);
        Ok(())
    }

    pub(super) async fn reset(
        &mut self,
        op: &OperationRecord,
        payload: &OperationPayload,
        snapshots: &SnapshotManager,
    ) -> Result<()> {
        let mode = payload.mode.context("reset operation is missing mode")?;
        let was_running = payload
            .was_running
            .context("reset operation is missing prior server state")?;
        let mut stage = payload.stage;
        let context = self.context_for_operation(op).await?;
        let generation = context.placement.generation;
        let current_snapshot = context.current_snapshot_generation;
        if generation != op.generation {
            bail!("reset operation generation is stale");
        }
        let server_dir = self.storage.ensure(op.server_id).await?;

        if !was_running {
            crate::worker::persistence::prepare_server_data(
                op.server_id,
                current_snapshot,
                &self.storage,
                snapshots,
            )
            .await?;
        } else {
            let placement = context.placement.clone();
            if placement.worker_id != self.config.id || placement.generation != op.generation {
                bail!("running reset does not match the current worker placement");
            }
        }

        if stage == OperationStage::Requested {
            if was_running && self.runtime.server_is_running(op.server_id).await? {
                let socket = self.runtime.socket_path(&server_dir)?;
                let runtime_control = self.runtime.control_for(&server_dir).await?;
                for player in self.runtime.players(&server_dir).await? {
                    let _ = runtime_control
                        .message(
                            &socket,
                            player.id,
                            "This server is resetting now. You will be disconnected briefly.",
                        )
                        .await;
                }
                self.runtime.save(&server_dir).await?;
            }
            self.control_plane
                .update_operation_payload(op, OperationProgress::stage(OperationStage::Saved))
                .await?;
            stage = OperationStage::Saved;
        }

        if stage == OperationStage::Saved {
            if was_running && self.runtime.server_is_running(op.server_id).await? {
                self.runtime
                    .graceful_stop(op.server_id, &server_dir)
                    .await?;
            }
            if was_running {
                self.control_plane
                    .deactivate_placement_after_stop(op)
                    .await?;
            }
            self.control_plane
                .update_operation_payload(op, OperationProgress::stage(OperationStage::Stopped))
                .await?;
            stage = OperationStage::Stopped;
        }

        if stage == OperationStage::Stopped {
            if self.runtime.server_is_running(op.server_id).await? {
                bail!("refusing to snapshot reset data while the runtime is active");
            }
            let backup = snapshots
                .create(op.server_id, &server_dir, SnapshotReason::Reset)
                .await?;
            self.control_plane
                .publish_snapshot(op.server_id, op, backup.generation, backup.size_bytes)
                .await?;
            self.control_plane
                .update_operation_payload(op, OperationProgress::backed_up(backup.generation))
                .await?;
            stage = OperationStage::BackedUp;
        }

        if stage == OperationStage::BackedUp {
            if mode == ResetMode::Full {
                self.storage.reset(op.server_id).await?;
            } else if mode == ResetMode::World {
                remove_reset_world(&server_dir.join("world")).await?;
            }
            clear_players(&self.control_plane, op.server_id, op.generation).await?;
            write_whitelist_file(&context, &server_dir).await?;
            write_operator_file(&context, &server_dir).await?;
            self.control_plane
                .update_operation_payload(op, OperationProgress::stage(OperationStage::Cleared))
                .await?;
            stage = OperationStage::Cleared;
        }

        if stage == OperationStage::Cleared {
            if was_running {
                let context = self.context_for_operation(op).await?;
                let placement = context.placement.clone();
                if placement.worker_id != self.config.id || placement.generation != op.generation {
                    bail!("reset restart does not match the worker placement");
                }
                write_whitelist_file(&context, &server_dir).await?;
                write_operator_file(&context, &server_dir).await?;
                self.ensure_runtime_ready(op.server_id, &placement, &server_dir, &context.settings)
                    .await?;
                sync_whitelist(
                    &self.control_plane,
                    self.runtime.as_ref(),
                    &context,
                    &server_dir,
                    true,
                )
                .await?;
                sync_operators(
                    &self.control_plane,
                    self.runtime.as_ref(),
                    &context,
                    &server_dir,
                    true,
                )
                .await?;
                self.runtime.save(&server_dir).await?;
                self.runtime
                    .graceful_stop(op.server_id, &server_dir)
                    .await?;
            }
            self.control_plane
                .update_operation_payload(op, OperationProgress::stage(OperationStage::Initialized))
                .await?;
            stage = OperationStage::Initialized;
        }

        let (snapshot_generation, snapshot_size) = if stage == OperationStage::Initialized {
            let snapshot = snapshots
                .create(op.server_id, &server_dir, SnapshotReason::Reset)
                .await?;
            self.control_plane
                .publish_snapshot(op.server_id, op, snapshot.generation, snapshot.size_bytes)
                .await?;
            self.control_plane
                .update_operation_payload(
                    op,
                    OperationProgress::snapshot(
                        OperationStage::Snapshotted,
                        snapshot.generation,
                        snapshot.size_bytes,
                    ),
                )
                .await?;
            (snapshot.generation, snapshot.size_bytes)
        } else if stage == OperationStage::Snapshotted {
            (
                payload
                    .snapshot_generation
                    .context("reset snapshot stage is missing its generation")?,
                payload
                    .snapshot_size
                    .context("reset snapshot stage is missing its size")?,
            )
        } else {
            bail!("reset operation reached invalid stage {stage:?}");
        };

        if was_running {
            let context = self.context_for_operation(op).await?;
            let placement = context.placement.clone();
            if !self.runtime.server_is_running(op.server_id).await? {
                write_whitelist_file(&context, &server_dir).await?;
                write_operator_file(&context, &server_dir).await?;
            }
            self.ensure_runtime_ready(op.server_id, &placement, &server_dir, &context.settings)
                .await?;
            sync_whitelist(
                &self.control_plane,
                self.runtime.as_ref(),
                &context,
                &server_dir,
                true,
            )
            .await?;
            sync_operators(
                &self.control_plane,
                self.runtime.as_ref(),
                &context,
                &server_dir,
                true,
            )
            .await?;
        }
        ensure_storage_limit(snapshot_size)?;
        self.control_plane
            .complete_reset(op, snapshot_generation, snapshot_size, was_running)
            .await?;
        self.idle_since.remove(&op.server_id);
        self.last_snapshot.remove(&op.server_id);
        Ok(())
    }

    pub(super) async fn delete(
        &mut self,
        op: &OperationRecord,
        _payload: &OperationPayload,
        snapshots: &SnapshotManager,
    ) -> Result<()> {
        self.control_plane.validate_delete(op).await?;

        self.storage.remove(op.server_id).await?;
        snapshots.delete_server_manifests(op).await?;
        self.server_access.remove_capability(op.server_id)?;
        self.control_plane.complete_delete(op).await?;
        self.idle_since.remove(&op.server_id);
        self.last_snapshot.remove(&op.server_id);
        self.last_log_read.remove(&op.server_id);
        Ok(())
    }

    pub(super) async fn start(
        &mut self,
        op: &OperationRecord,
        payload: &OperationPayload,
        snapshots: &SnapshotManager,
    ) -> Result<()> {
        let context = self.context_for_operation(op).await?;
        let placement = context.placement.clone();
        if placement.worker_id != self.config.id || placement.generation != op.generation {
            bail!("start operation placement does not match worker or generation");
        }
        let settings = context.settings.clone();
        let current_snapshot = context.current_snapshot_generation;

        let server_dir = match self.runtime.server_generation(op.server_id).await? {
            Some(generation) if generation == op.generation => {
                self.storage.ensure(op.server_id).await?
            }
            Some(generation) => {
                bail!("refusing to replace storage used by running generation {generation}")
            }
            None => {
                crate::worker::persistence::prepare_server_data(
                    op.server_id,
                    current_snapshot,
                    &self.storage,
                    snapshots,
                )
                .await?
            }
        };
        let storage_bytes = persistent_size(&server_dir)?;
        ensure_storage_limit(storage_bytes)?;
        write_whitelist_file(&context, &server_dir).await?;
        write_operator_file(&context, &server_dir).await?;

        let api_capability = self
            .server_access
            .prepare_plugin(op.server_id, placement.generation, &server_dir)
            .await?;
        if payload.stage == OperationStage::Requested {
            self.control_plane
                .update_operation_payload(op, OperationProgress::stage(OperationStage::Starting))
                .await?;
        }
        self.runtime
            .start(
                op.server_id,
                &placement,
                &server_dir,
                &settings,
                api_capability.as_deref(),
            )
            .await?;
        sync_whitelist(
            &self.control_plane,
            self.runtime.as_ref(),
            &context,
            &server_dir,
            true,
        )
        .await?;
        sync_operators(
            &self.control_plane,
            self.runtime.as_ref(),
            &context,
            &server_dir,
            true,
        )
        .await?;
        let storage_bytes = persistent_size(&server_dir)?;
        ensure_storage_limit(storage_bytes)?;
        self.control_plane.complete_start(op, storage_bytes).await?;
        self.idle_since.remove(&op.server_id);
        self.last_snapshot
            .insert(op.server_id, SnapshotSchedule::new(Instant::now()));
        Ok(())
    }

    pub(super) async fn stop(
        &mut self,
        op: &OperationRecord,
        payload: &OperationPayload,
        snapshots: &SnapshotManager,
    ) -> Result<()> {
        let context = self.context_for_operation(op).await?;
        let placement = context.placement.clone();
        if placement.worker_id != self.config.id || placement.generation != op.generation {
            bail!("stop operation placement does not match worker or generation");
        }
        let server_dir = self.server_dir(op.server_id);
        let mut stage = payload.stage;

        if stage == OperationStage::Requested {
            if !self.runtime.server_is_running(op.server_id).await? {
                bail!("runtime stopped before the clean shutdown save completed");
            }
            self.runtime.save(&server_dir).await?;
            self.control_plane
                .update_operation_payload(op, OperationProgress::stage(OperationStage::Saved))
                .await?;
            stage = OperationStage::Saved;
        }

        if stage == OperationStage::Saved {
            if self.runtime.server_is_running(op.server_id).await? {
                self.runtime
                    .graceful_stop(op.server_id, &server_dir)
                    .await?;
            }
            self.control_plane
                .deactivate_placement_after_stop(op)
                .await?;
            self.control_plane
                .update_operation_payload(op, OperationProgress::stage(OperationStage::Stopped))
                .await?;
            stage = OperationStage::Stopped;
        }

        let (snapshot_generation, snapshot_size) = if stage == OperationStage::Stopped {
            let snapshot = snapshots
                .create(op.server_id, &server_dir, SnapshotReason::Shutdown)
                .await?;
            self.control_plane
                .publish_snapshot(op.server_id, op, snapshot.generation, snapshot.size_bytes)
                .await?;
            self.control_plane
                .update_operation_payload(
                    op,
                    OperationProgress::snapshot(
                        OperationStage::Snapshotted,
                        snapshot.generation,
                        snapshot.size_bytes,
                    ),
                )
                .await?;
            (snapshot.generation, snapshot.size_bytes)
        } else if stage == OperationStage::Snapshotted {
            let generation = payload
                .snapshot_generation
                .context("snapshotted stop operation is missing snapshot_generation")?;
            let size = payload
                .snapshot_size
                .context("snapshotted stop operation is missing snapshot_size")?;
            (generation, size)
        } else {
            bail!("stop operation reached invalid stage {stage:?}");
        };

        self.control_plane
            .complete_stop(op, snapshot_generation, snapshot_size)
            .await?;
        self.idle_since.remove(&op.server_id);
        self.last_snapshot.remove(&op.server_id);
        Ok(())
    }
}

async fn remove_reset_world(path: &std::path::Path) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            tokio::fs::remove_file(path).await?;
        }
        Ok(_) => tokio::fs::remove_dir_all(path).await?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
