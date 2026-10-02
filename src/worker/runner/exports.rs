use std::path::{Path as FsPath, PathBuf};

use anyhow::Result;

use super::WorkerRunner;
use crate::{contracts::worker::WorkerExportTask, worker::snapshot::SnapshotManager};

impl WorkerRunner {
    async fn claim_world_export(&self) -> Result<Option<WorkerExportTask>> {
        self.control_plane.claim_export().await
    }

    pub(super) async fn spawn_world_export_if_idle(&mut self) -> Result<()> {
        if let Some(task) = self.world_export_task.take() {
            if task.is_finished() {
                if let Err(error) = task.await {
                    tracing::error!(?error, "world export task panicked");
                }
            } else {
                self.world_export_task = Some(task);
                return Ok(());
            }
        }
        let Some(task) = self.claim_world_export().await? else {
            return Ok(());
        };
        let snapshots = self.snapshots.for_export(
            task.server_id,
            task.id,
            task.source_generation,
            task.attempt,
        );
        let control_plane = self.control_plane.clone();
        let staging_root = self.config.root.join("exports");
        self.world_export_task = Some(tokio::spawn(async move {
            if let Err(error) = run_world_export(control_plane, snapshots, task, staging_root).await
            {
                tracing::error!(?error, "world export worker failed unexpectedly");
            }
        }));
        Ok(())
    }
}

async fn run_world_export(
    control_plane: crate::worker::control_plane::WorkerControl,
    snapshots: SnapshotManager,
    task: WorkerExportTask,
    staging_root: PathBuf,
) -> Result<()> {
    tokio::fs::create_dir_all(&staging_root).await?;
    let stale_file = staging_root.join(format!("{}-{}.zip.part", task.id, task.attempt));
    let _ = tokio::fs::remove_file(&stale_file).await;

    let result = build_upload_complete(&control_plane, &snapshots, &task, &staging_root).await;
    if let Err(error) = result {
        if let Err(report_error) = control_plane
            .fail_export(
                &task,
                &format!("{error:#}").chars().take(1000).collect::<String>(),
            )
            .await
        {
            tracing::warn!(export_id = %task.id, ?report_error, "failed export report was not accepted");
        }
        let _ = tokio::fs::remove_file(stale_file).await;
        return Ok(());
    }
    Ok(())
}

async fn build_upload_complete(
    control_plane: &crate::worker::control_plane::WorkerControl,
    snapshots: &SnapshotManager,
    task: &WorkerExportTask,
    staging_root: &FsPath,
) -> Result<()> {
    let archive = snapshots
        .create_world_archive(
            task.id,
            task.server_id,
            task.source_generation,
            task.attempt,
            staging_root,
        )
        .await?;
    let result = snapshots.upload_world_archive(&archive).await;
    if let Err(error) = result {
        let _ = snapshots
            .delete_world_archive(task.server_id, &archive.archive_key)
            .await;
        snapshots.remove_world_archive_staging(&archive).await;
        return Err(error);
    }
    if let Err(error) = control_plane
        .complete_export(
            task,
            &archive.archive_key,
            archive.size_bytes,
            &archive.checksum,
        )
        .await
    {
        let _ = snapshots
            .delete_world_archive(task.server_id, &archive.archive_key)
            .await;
        snapshots.remove_world_archive_staging(&archive).await;
        return Err(error);
    }
    snapshots.remove_world_archive_staging(&archive).await;
    Ok(())
}
