//! Supervision for long-lived control-plane workers.
//!
//! Background workers are part of the control plane's availability contract. A
//! detached Tokio task that logs an error and keeps going is not supervision:
//! it can silently stop making progress while the HTTP server still reports
//! success. This module gives each worker an explicit lifecycle state, keeps a
//! heartbeat alive while an operation or retry backoff is in progress, and
//! retains every worker join handle so a fatal failure reaches the process
//! boundary instead of disappearing in a detached task.

use std::{
    future::Future,
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, Error, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use thiserror::Error as ThisError;
use tokio::task::JoinSet;

const DEFAULT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);
const DEFAULT_RETRY_BACKOFF: Duration = Duration::from_secs(5);
const DEFAULT_MAX_RETRY_BACKOFF: Duration = Duration::from_secs(5 * 60);

/// Whether an operation failure can recover without restarting the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    Retryable,
    Fatal,
}

/// A worker operation failure with an explicit recovery policy.
#[derive(Debug, ThisError)]
pub enum WorkerError {
    #[error("retryable worker failure: {0}")]
    Retryable(#[source] Error),
    #[error("fatal worker failure: {0}")]
    Fatal(#[source] Error),
}

impl WorkerError {
    pub fn retryable(error: impl Into<Error>) -> Self {
        Self::Retryable(error.into())
    }

    pub fn fatal(error: impl Into<Error>) -> Self {
        Self::Fatal(error.into())
    }

    pub fn class(&self) -> FailureClass {
        match self {
            Self::Retryable(_) => FailureClass::Retryable,
            Self::Fatal(_) => FailureClass::Fatal,
        }
    }

    pub fn error(&self) -> &Error {
        match self {
            Self::Retryable(error) | Self::Fatal(error) => error,
        }
    }
}

/// Worker timing and retry policy.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub interval: Duration,
    pub heartbeat_interval: Duration,
    pub heartbeat_timeout: Duration,
    pub initial_retry_backoff: Duration,
    pub max_retry_backoff: Duration,
    pub required: bool,
}

impl SupervisorConfig {
    pub fn required(interval: Duration) -> Self {
        Self {
            interval,
            heartbeat_interval: interval
                .min(Duration::from_secs(15))
                .max(Duration::from_millis(10)),
            heartbeat_timeout: DEFAULT_HEARTBEAT_TIMEOUT,
            initial_retry_backoff: DEFAULT_RETRY_BACKOFF,
            max_retry_backoff: DEFAULT_MAX_RETRY_BACKOFF,
            required: true,
        }
    }

    pub fn optional(interval: Duration) -> Self {
        Self {
            required: false,
            ..Self::required(interval)
        }
    }

    fn normalized(self) -> Self {
        let heartbeat_interval = self
            .heartbeat_interval
            .max(Duration::from_millis(1))
            .min(self.heartbeat_timeout.max(Duration::from_millis(1)));
        let heartbeat_timeout = self
            .heartbeat_timeout
            .max(heartbeat_interval.saturating_mul(2));
        let initial_retry_backoff = self.initial_retry_backoff.max(Duration::from_millis(1));
        let max_retry_backoff = self
            .max_retry_backoff
            .max(initial_retry_backoff)
            .max(Duration::from_millis(1));
        Self {
            interval: self.interval.max(Duration::from_millis(1)),
            heartbeat_interval,
            heartbeat_timeout,
            initial_retry_backoff,
            max_retry_backoff,
            required: self.required,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    Starting,
    Running,
    BackingOff,
    Fatal,
    Stopped,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkerSnapshot {
    pub name: String,
    pub state: WorkerState,
    pub required: bool,
    pub last_heartbeat: Option<DateTime<Utc>>,
    pub last_success: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SupervisionSnapshot {
    pub healthy: bool,
    pub ready: bool,
    pub workers: Vec<WorkerSnapshot>,
}

#[derive(Debug)]
struct WorkerRecord {
    snapshot: WorkerSnapshot,
    heartbeat_timeout: Duration,
}

/// Registry shared by the HTTP API and all supervised background workers.
#[derive(Clone, Debug, Default)]
pub struct SupervisorRegistry {
    workers: Arc<RwLock<Vec<Arc<tokio::sync::RwLock<WorkerRecord>>>>>,
}

impl SupervisorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, name: impl Into<String>, config: &SupervisorConfig) -> WorkerHandle {
        let name = name.into();
        let record = Arc::new(tokio::sync::RwLock::new(WorkerRecord {
            snapshot: WorkerSnapshot {
                name,
                state: WorkerState::Starting,
                required: config.required,
                last_heartbeat: None,
                last_success: None,
                last_error: None,
                consecutive_failures: 0,
            },
            heartbeat_timeout: config.heartbeat_timeout,
        }));
        self.workers
            .write()
            .expect("supervisor registry lock poisoned")
            .push(record.clone());
        WorkerHandle { record }
    }

    pub async fn snapshot(&self) -> SupervisionSnapshot {
        let records = self
            .workers
            .read()
            .expect("supervisor registry lock poisoned")
            .clone();
        let mut workers = Vec::with_capacity(records.len());
        let mut healthy = true;
        let mut ready = true;
        let now = Utc::now();
        for record in records {
            let record = record.read().await;
            let worker = record.snapshot.clone();
            let heartbeat_fresh = worker.last_heartbeat.is_some_and(|heartbeat| {
                (now - heartbeat).to_std().unwrap_or(Duration::MAX) <= record.heartbeat_timeout
            });
            if worker.required
                && (!heartbeat_fresh
                    || matches!(
                        worker.state,
                        WorkerState::BackingOff | WorkerState::Fatal | WorkerState::Stopped
                    ))
            {
                healthy = false;
            }
            if worker.required
                && (worker.last_success.is_none()
                    || matches!(
                        worker.state,
                        WorkerState::Starting
                            | WorkerState::BackingOff
                            | WorkerState::Fatal
                            | WorkerState::Stopped
                    ))
            {
                ready = false;
            }
            workers.push(worker);
        }
        SupervisionSnapshot {
            healthy,
            ready,
            workers,
        }
    }

    pub async fn is_healthy(&self) -> bool {
        self.snapshot().await.healthy
    }

    pub async fn is_ready(&self) -> bool {
        self.snapshot().await.ready
    }
}

#[derive(Clone, Debug)]
pub struct WorkerHandle {
    record: Arc<tokio::sync::RwLock<WorkerRecord>>,
}

impl WorkerHandle {
    /// Start an operation attempt and mark the worker as actively running.
    pub async fn heartbeat(&self) {
        let mut record = self.record.write().await;
        record.snapshot.last_heartbeat = Some(Utc::now());
        if matches!(
            record.snapshot.state,
            WorkerState::Starting | WorkerState::BackingOff
        ) {
            record.snapshot.state = WorkerState::Running;
        }
    }

    async fn touch_heartbeat(&self) {
        self.record.write().await.snapshot.last_heartbeat = Some(Utc::now());
    }

    pub async fn success(&self) {
        let mut record = self.record.write().await;
        let now = Utc::now();
        record.snapshot.last_heartbeat = Some(now);
        record.snapshot.last_success = Some(now);
        record.snapshot.last_error = None;
        record.snapshot.consecutive_failures = 0;
        record.snapshot.state = WorkerState::Running;
    }

    pub async fn failure(&self, error: &Error, class: FailureClass) {
        let mut record = self.record.write().await;
        // A failing required worker turns the health check red, which stops
        // a deploy; without this line nothing says which worker or why.
        tracing::warn!(
            worker = %record.snapshot.name,
            required = record.snapshot.required,
            ?class,
            error = %format!("{error:#}"),
            "control-plane worker failed"
        );
        record.snapshot.last_heartbeat = Some(Utc::now());
        record.snapshot.last_error = Some(error.to_string());
        record.snapshot.consecutive_failures =
            record.snapshot.consecutive_failures.saturating_add(1);
        record.snapshot.state = match class {
            FailureClass::Retryable => WorkerState::BackingOff,
            FailureClass::Fatal => WorkerState::Fatal,
        };
    }

    pub async fn stopped(&self) {
        let mut record = self.record.write().await;
        record.snapshot.state = WorkerState::Stopped;
    }

    pub async fn snapshot(&self) -> WorkerSnapshot {
        self.record.read().await.snapshot.clone()
    }
}

async fn wait_with_heartbeat(
    handle: &WorkerHandle,
    duration: Duration,
    heartbeat_interval: Duration,
) {
    let deadline = tokio::time::Instant::now() + duration;
    let mut heartbeat = tokio::time::interval(heartbeat_interval);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return,
            _ = heartbeat.tick() => handle.touch_heartbeat().await,
        }
    }
}

async fn run_operation_with_heartbeat<Fut>(
    handle: &WorkerHandle,
    operation: Fut,
    heartbeat_interval: Duration,
) -> Result<(), WorkerError>
where
    Fut: Future<Output = Result<(), WorkerError>> + Send,
{
    tokio::pin!(operation);
    let mut heartbeat = tokio::time::interval(heartbeat_interval);
    loop {
        tokio::select! {
            result = &mut operation => return result,
            _ = heartbeat.tick() => handle.touch_heartbeat().await,
        }
    }
}

/// Run one control-plane worker forever with retries and explicit fatal exits.
///
/// A retryable failure never silently disappears: it is reflected in the
/// registry, exponentially backs off, and then retries the operation. A fatal
/// failure marks the worker unhealthy and returns to the caller so the caller
/// can retain the join handle and decide how to terminate the process.
pub async fn run_supervised<F, Fut>(
    handle: WorkerHandle,
    config: SupervisorConfig,
    mut operation: F,
) -> Result<(), WorkerError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), WorkerError>> + Send,
{
    let config = config.normalized();
    let mut retry_attempt = 0u32;
    loop {
        handle.heartbeat().await;
        let result =
            run_operation_with_heartbeat(&handle, operation(), config.heartbeat_interval).await;
        match result {
            Ok(()) => {
                retry_attempt = 0;
                handle.success().await;
                wait_with_heartbeat(&handle, config.interval, config.heartbeat_interval).await;
            }
            Err(error) => {
                let class = error.class();
                handle.failure(error.error(), class).await;
                if class == FailureClass::Fatal {
                    return Err(error);
                }
                let delay = config
                    .initial_retry_backoff
                    .checked_mul(2u32.saturating_pow(retry_attempt.min(16)))
                    .unwrap_or(config.max_retry_backoff)
                    .min(config.max_retry_backoff);
                retry_attempt = retry_attempt.saturating_add(1);
                wait_with_heartbeat(&handle, delay, config.heartbeat_interval).await;
            }
        }
    }
}

struct WorkerTaskResult {
    name: String,
    result: Result<(), WorkerError>,
}

/// Owns all long-lived worker tasks and propagates their terminal failures.
///
/// The HTTP server does not own these tasks and does not create detached
/// workers. Dropping this supervisor aborts its children; awaiting `run`
/// observes both explicit fatal results and task panics.
pub struct Supervisor {
    registry: SupervisorRegistry,
    tasks: JoinSet<WorkerTaskResult>,
}

impl Supervisor {
    pub fn new(registry: SupervisorRegistry) -> Self {
        Self {
            registry,
            tasks: JoinSet::new(),
        }
    }

    pub fn registry(&self) -> SupervisorRegistry {
        self.registry.clone()
    }

    pub fn spawn<F, Fut>(&mut self, name: impl Into<String>, config: SupervisorConfig, operation: F)
    where
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), WorkerError>> + Send + 'static,
    {
        let name = name.into();
        let handle = self.registry.register(name.clone(), &config);
        self.tasks.spawn(async move {
            let result = run_supervised(handle, config, operation).await;
            WorkerTaskResult { name, result }
        });
    }

    pub async fn run(mut self) -> Result<()> {
        while let Some(joined) = self.tasks.join_next().await {
            let task = joined.context("supervised control-plane worker task panicked")?;
            match task.result {
                Ok(()) => {
                    return Err(anyhow::anyhow!(
                        "supervised control-plane worker {} stopped unexpectedly",
                        task.name
                    ));
                }
                Err(error) => {
                    return Err(anyhow::anyhow!(
                        "supervised control-plane worker {} stopped: {error}",
                        task.name
                    ))
                    .context(error.error().to_string());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    fn fast_config(required: bool) -> SupervisorConfig {
        SupervisorConfig {
            interval: Duration::from_millis(1),
            heartbeat_interval: Duration::from_millis(1),
            heartbeat_timeout: Duration::from_millis(50),
            initial_retry_backoff: Duration::from_millis(1),
            max_retry_backoff: Duration::from_millis(4),
            required,
        }
    }

    #[tokio::test]
    async fn retries_update_state_and_fatal_failure_reaches_supervisor() {
        let registry = SupervisorRegistry::new();
        let config = fast_config(true);
        let mut supervisor = Supervisor::new(registry.clone());
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_operation = attempts.clone();
        supervisor.spawn("retrying", config, move || {
            let attempt = attempts_for_operation.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt == 0 {
                    Err(WorkerError::retryable(anyhow::anyhow!("temporary outage")))
                } else {
                    Err(WorkerError::fatal(anyhow::anyhow!("stop test worker")))
                }
            }
        });

        let result = supervisor.run().await;
        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let snapshot = registry.snapshot().await;
        assert_eq!(snapshot.workers[0].state, WorkerState::Fatal);
        assert_eq!(snapshot.workers[0].consecutive_failures, 2);
        assert_eq!(
            snapshot.workers[0].last_error.as_deref(),
            Some("stop test worker")
        );
        assert!(!snapshot.ready);
        assert!(!snapshot.healthy);
    }

    #[tokio::test]
    async fn heartbeat_stays_fresh_during_slow_operation() {
        let registry = SupervisorRegistry::new();
        let config = fast_config(true);
        let worker = registry.register("slow", &config);
        let worker_for_task = worker.clone();
        let task = tokio::spawn(async move {
            run_supervised(worker_for_task, config, || async {
                tokio::time::sleep(Duration::from_millis(12)).await;
                Err(WorkerError::fatal(anyhow::anyhow!("done")))
            })
            .await
        });
        tokio::time::sleep(Duration::from_millis(6)).await;
        let snapshot = worker.snapshot().await;
        assert!(snapshot.last_heartbeat.is_some());
        assert_eq!(snapshot.state, WorkerState::Running);
        let result = task.await.unwrap();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn optional_workers_do_not_block_readiness() {
        let registry = SupervisorRegistry::new();
        let config = fast_config(false);
        let worker = registry.register("optional", &config);
        worker.heartbeat().await;
        assert!(registry.is_ready().await);
        assert!(registry.is_healthy().await);
    }

    #[tokio::test]
    async fn retry_backoff_is_unhealthy_until_the_next_attempt_succeeds() {
        let registry = SupervisorRegistry::new();
        let config = SupervisorConfig {
            interval: Duration::from_secs(60),
            heartbeat_interval: Duration::from_millis(1),
            heartbeat_timeout: Duration::from_millis(50),
            initial_retry_backoff: Duration::from_millis(20),
            max_retry_backoff: Duration::from_millis(20),
            required: true,
        };
        let worker = registry.register("backoff", &config);
        let worker_for_task = worker.clone();
        let task = tokio::spawn(async move {
            run_supervised(worker_for_task, config, || async {
                Err(WorkerError::retryable(anyhow::anyhow!(
                    "database unavailable"
                )))
            })
            .await
        });
        tokio::time::sleep(Duration::from_millis(5)).await;
        let snapshot = registry.snapshot().await;
        assert_eq!(snapshot.workers[0].state, WorkerState::BackingOff);
        assert!(!snapshot.healthy);
        assert!(!snapshot.ready);
        task.abort();
    }
}
