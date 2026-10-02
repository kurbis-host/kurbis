use anyhow::Result;
use serde::Serialize;

use crate::contracts::worker::WorkerExportTask;

use super::super::{EmptyRequest, WorkerControl, bounded_worker_error};

impl WorkerControl {
    pub async fn claim_export(&self) -> Result<Option<WorkerExportTask>> {
        self.post_json("/internal/worker/control/exports/claim", &EmptyRequest {})
            .await
    }

    pub async fn complete_export(
        &self,
        task: &WorkerExportTask,
        archive_key: &str,
        archive_size: i64,
        archive_checksum: &str,
    ) -> Result<()> {
        self.post_no_content(
            &format!("/internal/worker/control/exports/{}/complete", task.id),
            &ExportFenceRequest {
                source_generation: task.source_generation,
                attempt: task.attempt,
                archive_key: Some(archive_key),
                archive_size: Some(archive_size),
                archive_checksum: Some(archive_checksum),
                error: None,
            },
        )
        .await
    }

    pub async fn fail_export(&self, task: &WorkerExportTask, error: &str) -> Result<()> {
        let error = bounded_worker_error(error);
        self.post_no_content(
            &format!("/internal/worker/control/exports/{}/fail", task.id),
            &ExportFenceRequest {
                source_generation: task.source_generation,
                attempt: task.attempt,
                archive_key: None,
                archive_size: None,
                archive_checksum: None,
                error: Some(error.as_ref()),
            },
        )
        .await
    }
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ExportFenceRequest<'a> {
    source_generation: i64,
    attempt: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive_key: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive_size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive_checksum: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}
