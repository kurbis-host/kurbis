use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::contracts::worker::OperationProgress;
use crate::model::OperationRecord;

use super::super::{EmptyRequest, WorkerControl, bounded_worker_error};

impl WorkerControl {
    pub async fn claim_operation(&self) -> Result<Option<OperationRecord>> {
        self.post_json(
            "/internal/worker/control/operations/claim",
            &EmptyRequest {},
        )
        .await
    }

    pub async fn update_operation_payload(
        &self,
        operation: &OperationRecord,
        patch: OperationProgress,
    ) -> Result<()> {
        self.post_no_content(
            &format!(
                "/internal/worker/control/operations/{}/progress",
                operation.id
            ),
            &OperationFenceRequest::new(operation, Some(patch)),
        )
        .await
    }

    pub async fn retry_operation(&self, operation: &OperationRecord, error: &str) -> Result<bool> {
        let error = bounded_worker_error(error);
        let response: RetryOperationResponse = self
            .post_json(
                &format!("/internal/worker/control/operations/{}/retry", operation.id),
                &OperationErrorFenceRequest::new(operation, error.as_ref()),
            )
            .await?;
        Ok(response.retry_scheduled)
    }

    pub async fn begin_snapshot_retry(&self, operation: &OperationRecord) -> Result<()> {
        self.post_no_content(
            &format!(
                "/internal/worker/control/operations/{}/begin-snapshot-retry",
                operation.id
            ),
            &OperationFenceRequest::new(operation, None),
        )
        .await
    }

    pub async fn defer_snapshot_operation(
        &self,
        operation: &OperationRecord,
        error: &str,
    ) -> Result<()> {
        let error = bounded_worker_error(error);
        self.post_no_content(
            &format!(
                "/internal/worker/control/operations/{}/defer-snapshot",
                operation.id
            ),
            &OperationErrorFenceRequest::new(operation, error.as_ref()),
        )
        .await
    }

    pub async fn complete_start(
        &self,
        operation: &OperationRecord,
        storage_bytes: i64,
    ) -> Result<()> {
        self.complete_operation(operation, OperationCompletion::Start { storage_bytes })
            .await
    }

    pub async fn complete_snapshot(
        &self,
        operation: &OperationRecord,
        snapshot_generation: i64,
    ) -> Result<()> {
        self.complete_operation(
            operation,
            OperationCompletion::Snapshot {
                snapshot_generation,
            },
        )
        .await
    }

    pub async fn complete_stop(
        &self,
        operation: &OperationRecord,
        snapshot_generation: i64,
        storage_bytes: i64,
    ) -> Result<()> {
        self.complete_operation(
            operation,
            OperationCompletion::Stop {
                snapshot_generation,
                storage_bytes,
            },
        )
        .await
    }

    pub async fn complete_restore(
        &self,
        operation: &OperationRecord,
        target_generation: i64,
        snapshot_generation: i64,
        storage_bytes: i64,
    ) -> Result<()> {
        self.complete_operation(
            operation,
            OperationCompletion::Restore {
                target_generation,
                snapshot_generation,
                storage_bytes,
            },
        )
        .await
    }

    pub async fn complete_reset(
        &self,
        operation: &OperationRecord,
        snapshot_generation: i64,
        storage_bytes: i64,
        was_running: bool,
    ) -> Result<()> {
        self.complete_operation(
            operation,
            OperationCompletion::Reset {
                snapshot_generation,
                storage_bytes,
                was_running,
            },
        )
        .await
    }

    pub async fn validate_delete(&self, operation: &OperationRecord) -> Result<()> {
        self.post_no_content(
            &format!(
                "/internal/worker/control/operations/{}/validate-delete",
                operation.id
            ),
            &OperationFenceRequest::new(operation, None),
        )
        .await
    }

    pub async fn deactivate_placement_after_stop(&self, operation: &OperationRecord) -> Result<()> {
        self.post_no_content(
            &format!(
                "/internal/worker/control/operations/{}/deactivate-placement",
                operation.id
            ),
            &OperationFenceRequest::new(operation, None),
        )
        .await
    }

    pub async fn complete_delete(&self, operation: &OperationRecord) -> Result<()> {
        self.complete_operation(operation, OperationCompletion::Delete)
            .await
    }

    pub async fn mark_runtime_lost(
        &self,
        server_id: Uuid,
        generation: i64,
        error: &str,
    ) -> Result<()> {
        let error = bounded_worker_error(error);
        self.post_no_content(
            "/internal/worker/control/runtime-lost",
            &RuntimeLostRequest {
                server_id,
                generation,
                error: error.as_ref(),
            },
        )
        .await
    }

    pub async fn publish_snapshot(
        &self,
        server_id: Uuid,
        operation: &OperationRecord,
        snapshot_generation: i64,
        size_bytes: i64,
    ) -> Result<()> {
        self.post_no_content(
            &format!("/internal/worker/control/servers/{server_id}/snapshots/publish"),
            &PublishSnapshotRequest {
                operation_id: operation.id,
                generation: operation.generation,
                attempt: operation.attempt,
                snapshot_generation,
                size_bytes,
            },
        )
        .await
    }

    async fn complete_operation(
        &self,
        operation: &OperationRecord,
        completion: OperationCompletion,
    ) -> Result<()> {
        self.post_no_content(
            &format!(
                "/internal/worker/control/operations/{}/complete",
                operation.id
            ),
            &CompleteOperationRequest {
                generation: operation.generation,
                attempt: operation.attempt,
                completion,
            },
        )
        .await
    }
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct OperationFenceRequest {
    generation: i64,
    attempt: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    patch: Option<OperationProgress>,
}

impl OperationFenceRequest {
    fn new(operation: &OperationRecord, patch: Option<OperationProgress>) -> Self {
        Self {
            generation: operation.generation,
            attempt: operation.attempt,
            patch,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct OperationErrorFenceRequest<'a> {
    generation: i64,
    attempt: i32,
    error: &'a str,
}

impl<'a> OperationErrorFenceRequest<'a> {
    fn new(operation: &OperationRecord, error: &'a str) -> Self {
        Self {
            generation: operation.generation,
            attempt: operation.attempt,
            error,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryOperationResponse {
    retry_scheduled: bool,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeLostRequest<'a> {
    server_id: Uuid,
    generation: i64,
    error: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct PublishSnapshotRequest {
    operation_id: Uuid,
    generation: i64,
    attempt: i32,
    snapshot_generation: i64,
    size_bytes: i64,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct CompleteOperationRequest {
    generation: i64,
    attempt: i32,
    completion: OperationCompletion,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum OperationCompletion {
    Start {
        storage_bytes: i64,
    },
    Snapshot {
        snapshot_generation: i64,
    },
    Stop {
        snapshot_generation: i64,
        storage_bytes: i64,
    },
    Restore {
        target_generation: i64,
        snapshot_generation: i64,
        storage_bytes: i64,
    },
    Reset {
        snapshot_generation: i64,
        storage_bytes: i64,
        was_running: bool,
    },
    Delete,
}
