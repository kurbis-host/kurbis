use anyhow::{Context, Result, bail};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::contracts::worker::OperationProgress;
use crate::model::OperationRecord;

use super::super::{EmptyRequest, OperationRetryOutcome, WorkerControl, bounded_worker_error};

impl WorkerControl {
    pub async fn claim_operation(&self) -> Result<Option<OperationRecord>> {
        self.post_json(
            "/internal/worker/control/operations/claim",
            &EmptyRequest {},
        )
        .await
    }

    /// Returns when the control plane has queued an operation or its wait
    /// window has passed. It carries no work; the caller claims next.
    pub async fn wait_for_operation(&self) -> Result<()> {
        let path = "/internal/worker/control/operations/wait";
        let response = self
            .authorized_request(Method::POST, self.endpoint(path)?)?
            // Longer than the control plane holds the request open.
            .timeout(std::time::Duration::from_secs(40))
            .json(&EmptyRequest {})
            .send()
            .await
            .context("wait for a worker operation")?;
        super::super::require_success(path, response).await?;
        Ok(())
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

    pub(crate) async fn retry_operation(
        &self,
        operation: &OperationRecord,
        error: &str,
    ) -> Result<OperationRetryOutcome> {
        let error = bounded_worker_error(error);
        let path = format!("/internal/worker/control/operations/{}/retry", operation.id);
        let response = self
            .authorized_request(Method::POST, self.endpoint(&path)?)?
            .json(&OperationErrorFenceRequest::new(operation, error.as_ref()))
            .send()
            .await
            .context("send worker control-plane request")?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable response body>".to_owned());
            if status == StatusCode::CONFLICT
                && serde_json::from_str::<WorkerControlError>(&body)
                    .is_ok_and(|response| response.code == "stale_worker_assignment")
            {
                return Ok(OperationRetryOutcome::NoLongerCurrent);
            }
            bail!(
                "worker control-plane request {path} failed with status {status}: {}",
                body.trim()
            );
        }
        let response: RetryOperationResponse = response
            .json()
            .await
            .context("decode worker control-plane response")?;
        Ok(if response.retry_scheduled {
            OperationRetryOutcome::Scheduled
        } else {
            OperationRetryOutcome::Exhausted
        })
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
        self.clear_players(operation.server_id, operation.generation)
            .await?;
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
        self.clear_players(operation.server_id, operation.generation)
            .await?;
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
struct WorkerControlError {
    code: String,
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
    use chrono::Utc;
    use serde_json::{Value, json};
    use tokio::net::TcpListener;

    use super::*;
    use crate::{
        model::{OperationKind, OperationStatus},
        worker::control_plane::OperationRetryOutcome,
    };

    #[derive(Clone, Default)]
    struct RequestOrder(Arc<Mutex<Vec<&'static str>>>);

    async fn clear_players(State(order): State<RequestOrder>) -> StatusCode {
        order.0.lock().unwrap().push("clear_players");
        StatusCode::NO_CONTENT
    }

    async fn complete_operation(State(order): State<RequestOrder>) -> StatusCode {
        order.0.lock().unwrap().push("complete_operation");
        StatusCode::NO_CONTENT
    }

    fn operation(kind: OperationKind) -> OperationRecord {
        let now = Utc::now();
        OperationRecord {
            id: Uuid::new_v4(),
            server_id: Uuid::new_v4(),
            worker_id: Some(Uuid::new_v4()),
            kind,
            status: OperationStatus::Running,
            generation: 7,
            attempt: 1,
            payload: Value::Null,
            last_error: None,
            created_at: now,
            updated_at: now,
        }
    }

    async fn control_plane_app(app: Router) -> (WorkerControl, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let control =
            WorkerControl::new(&format!("http://{address}"), &"a".repeat(43), true).unwrap();
        (control, task)
    }

    #[tokio::test]
    async fn stop_and_restore_clear_players_before_releasing_the_operation_fence() {
        let order = RequestOrder::default();
        let app = Router::new()
            .route(
                "/internal/worker/control/servers/{server_id}/players",
                post(clear_players),
            )
            .route(
                "/internal/worker/control/operations/{operation_id}/complete",
                post(complete_operation),
            )
            .with_state(order.clone());
        let (control, server) = control_plane_app(app).await;

        control
            .complete_stop(&operation(OperationKind::Stop), 8, 1024)
            .await
            .unwrap();
        assert_eq!(
            order.0.lock().unwrap().as_slice(),
            &["clear_players", "complete_operation"]
        );

        order.0.lock().unwrap().clear();
        control
            .complete_restore(&operation(OperationKind::Restore), 3, 9, 1024)
            .await
            .unwrap();
        assert_eq!(
            order.0.lock().unwrap().as_slice(),
            &["clear_players", "complete_operation"]
        );
        server.abort();
    }

    #[tokio::test]
    async fn cleanup_failure_prevents_operation_completion() {
        let order = RequestOrder::default();
        let app = Router::new()
            .route(
                "/internal/worker/control/servers/{server_id}/players",
                post(|| async {
                    (
                        StatusCode::CONFLICT,
                        Json(json!({
                            "code": "worker_assignment_invalid",
                            "message": "assignment no longer current"
                        })),
                    )
                }),
            )
            .route(
                "/internal/worker/control/operations/{operation_id}/complete",
                post(complete_operation),
            )
            .with_state(order.clone());
        let (control, server) = control_plane_app(app).await;

        assert!(
            control
                .complete_stop(&operation(OperationKind::Stop), 8, 1024)
                .await
                .is_err()
        );
        assert!(order.0.lock().unwrap().is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn unrelated_retry_conflicts_remain_errors() {
        let app = Router::new().route(
            "/internal/worker/control/operations/{operation_id}/retry",
            post(|| async {
                (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "code": "operation_retry_denied",
                        "message": "retry is not permitted"
                    })),
                )
            }),
        );
        let (control, server) = control_plane_app(app).await;

        let error = control
            .retry_operation(&operation(OperationKind::Stop), "transient failure")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("operation_retry_denied"));
        server.abort();
    }

    #[tokio::test]
    async fn retry_of_a_completed_or_superseded_operation_is_not_fatal() {
        let app = Router::new().route(
            "/internal/worker/control/operations/{operation_id}/retry",
            post(|| async {
                (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "code": "stale_worker_assignment",
                        "message": "operation is no longer current"
                    })),
                )
            }),
        );
        let (control, server) = control_plane_app(app).await;

        assert_eq!(
            control
                .retry_operation(&operation(OperationKind::Stop), "late cleanup")
                .await
                .unwrap(),
            OperationRetryOutcome::NoLongerCurrent
        );
        server.abort();
    }
}
