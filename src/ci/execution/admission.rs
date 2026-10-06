//! Cross-process memory admission for CI build work.
//!
//! The advisory lock is session-scoped and held on a dedicated PostgreSQL
//! connection, so it serializes independent control-plane processes. If a
//! process is cancelled or crashes, closing that connection releases the lock;
//! the next owner removes only abandoned, explicitly labelled builder
//! containers before admitting more work.

use std::{
    collections::HashMap,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use sqlx::{Connection, PgConnection, PgPool};
use thiserror::Error;
use tokio::{process::Command, sync::oneshot, task::JoinHandle, time::timeout};
use uuid::Uuid;

const DEFAULT_SYSTEM_RESERVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const DEFAULT_BUILD_CONCURRENCY: u16 = 2;
const MIN_BUILD_CONCURRENCY: u16 = 1;
const MAX_BUILD_CONCURRENCY: u16 = 64;
const LOCK_POLL_INTERVAL: Duration = Duration::from_secs(2);
const LOCK_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const LEASE_STALE_AFTER: Duration = Duration::from_secs(45);
const DOCKER_TIMEOUT: Duration = Duration::from_secs(30);
const DB_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum BuildAdmissionError {
    #[error("host build admission timed out after {seconds} seconds: {detail}")]
    Timeout { seconds: u64, detail: String },
    #[error("host build admission is unavailable: {0}")]
    Unavailable(String),
    #[error("host build admission was fenced: {0}")]
    Fenced(String),
}

/// Parse the shared build concurrency setting at a process boundary.
///
/// The range is intentionally bounded: a typo must not turn one builder into
/// an unbounded fork bomb, while operators can still use multiple slots on a
/// larger host. Missing configuration keeps a bounded two-slot default.
pub(crate) fn parse_build_concurrency(value: Option<&str>) -> Result<u16, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_BUILD_CONCURRENCY);
    };
    let parsed = value
        .trim()
        .parse::<u16>()
        .map_err(|_| format!("CI_BUILD_CONCURRENCY must be an integer from {MIN_BUILD_CONCURRENCY} to {MAX_BUILD_CONCURRENCY}"))?;
    if !(MIN_BUILD_CONCURRENCY..=MAX_BUILD_CONCURRENCY).contains(&parsed) {
        return Err(format!(
            "CI_BUILD_CONCURRENCY must be from {MIN_BUILD_CONCURRENCY} to {MAX_BUILD_CONCURRENCY}"
        ));
    }
    Ok(parsed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HostMemorySnapshot {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_free_bytes: u64,
}
impl HostMemorySnapshot {
    pub(super) fn can_admit_with_reserved(
        self,
        requested_bytes: u64,
        reserve_bytes: u64,
        reserved_bytes: u64,
    ) -> bool {
        self.total_bytes > 0
            && self.available_bytes > 0
            && self.available_bytes <= self.total_bytes
            && reserved_bytes
                .checked_add(requested_bytes)
                .and_then(|required| required.checked_add(reserve_bytes))
                .is_some_and(|required| self.available_bytes >= required)
    }
}

#[async_trait]
pub(super) trait HostMemoryProbe: Send + Sync {
    async fn snapshot(&self) -> Result<HostMemorySnapshot, BuildAdmissionError>;
}

#[async_trait]
pub(super) trait AbandonedBuildCleaner: Send + Sync {
    /// Remove resources owned by a stale lease. The token is unique across
    /// builders, so a reclaim can never remove another live builder's work.
    async fn remove_lease(&self, lease_token: &str) -> Result<(), BuildAdmissionError>;
}

/// Database-backed, cross-process admission for one physical builder.
///
/// A builder identity has `concurrency` independently fenced slots. A global
/// per-identity advisory lock serializes the short claim transaction so the
/// memory sum cannot race between processes. Each held slot has its own
/// session-scoped advisory lock (crash-safe) and a heartbeat row (fencing and
/// stale resource cleanup). This is deliberately generic so Docker and the
/// future Bubblewrap backend share the same coordinator rather than silently
/// creating per-process semaphores.
#[derive(Clone)]
pub(super) struct BuildAdmissionCoordinator {
    pool: PgPool,
    builder_identity: String,
    builder_label: String,
    memory_probe: Arc<dyn HostMemoryProbe>,
    cleaner: Arc<dyn AbandonedBuildCleaner>,
    system_reserve_bytes: u64,
    concurrency: u16,
    lock_poll_interval: Duration,
    heartbeat_interval: Duration,
    wait_timeout: Duration,
}

impl BuildAdmissionCoordinator {
    pub fn new(pool: PgPool, image: impl Into<String>, docker_host: impl Into<String>) -> Self {
        let image = image.into();
        let docker_host = docker_host.into();
        let builder_identity = format!("docker:{docker_host}:{image}");
        Self::for_builder(
            pool,
            builder_identity,
            Arc::new(DockerHostMemoryProbe::new(image, docker_host.clone())),
            Arc::new(DockerAbandonedBuildCleaner::new(docker_host)),
        )
    }

    pub(super) fn for_builder(
        pool: PgPool,
        builder_identity: impl Into<String>,
        memory_probe: Arc<dyn HostMemoryProbe>,
        cleaner: Arc<dyn AbandonedBuildCleaner>,
    ) -> Self {
        let builder_identity = builder_identity.into();
        let builder_label = blake3::hash(builder_identity.as_bytes())
            .to_hex()
            .to_string();
        Self {
            pool,
            builder_identity,
            builder_label,
            memory_probe,
            cleaner,
            system_reserve_bytes: DEFAULT_SYSTEM_RESERVE_BYTES,
            concurrency: DEFAULT_BUILD_CONCURRENCY,
            lock_poll_interval: LOCK_POLL_INTERVAL,
            heartbeat_interval: LOCK_HEARTBEAT_INTERVAL,
            wait_timeout: Duration::from_secs(10 * 60),
        }
    }

    pub fn with_concurrency(mut self, concurrency: usize) -> Result<Self, BuildAdmissionError> {
        if !(usize::from(MIN_BUILD_CONCURRENCY)..=usize::from(MAX_BUILD_CONCURRENCY))
            .contains(&concurrency)
        {
            return Err(BuildAdmissionError::Unavailable(format!(
                "build concurrency must be between {MIN_BUILD_CONCURRENCY} and {MAX_BUILD_CONCURRENCY}"
            )));
        }
        self.concurrency = u16::try_from(concurrency).expect("validated build concurrency");
        Ok(self)
    }

    #[cfg(test)]
    pub(super) fn with_timing(
        mut self,
        poll_interval: Duration,
        heartbeat_interval: Duration,
        wait_timeout: Duration,
    ) -> Self {
        self.lock_poll_interval = poll_interval;
        self.heartbeat_interval = heartbeat_interval;
        self.wait_timeout = wait_timeout;
        self
    }

    pub(super) async fn acquire(
        &self,
        requested_memory_bytes: u64,
    ) -> Result<BuildAdmissionLease, BuildAdmissionError> {
        if requested_memory_bytes == 0 || self.system_reserve_bytes == 0 {
            return Err(BuildAdmissionError::Unavailable(
                "build memory and host reserve must be non-zero".into(),
            ));
        }
        let deadline = tokio::time::Instant::now() + self.wait_timeout;
        let mut last_snapshot = None;
        let global_key = advisory_key(&self.builder_identity, u32::MAX);
        loop {
            if tokio::time::Instant::now() >= deadline {
                let detail = last_snapshot
                    .map(|snapshot: HostMemorySnapshot| {
                        format!(
                            "available={} MiB of {} MiB; build requires {} MiB plus {} MiB reserved for system/runtime and active slots; swap free={} MiB",
                            snapshot.available_bytes / 1024 / 1024,
                            snapshot.total_bytes / 1024 / 1024,
                            requested_memory_bytes / 1024 / 1024,
                            self.system_reserve_bytes / 1024 / 1024,
                            snapshot.swap_free_bytes / 1024 / 1024,
                        )
                    })
                    .unwrap_or_else(|| "no host memory sample was available".into());
                return Err(BuildAdmissionError::Timeout {
                    seconds: self.wait_timeout.as_secs().max(1),
                    detail,
                });
            }
            // Advisory locks require a session that remains pinned for the
            // lease lifetime. Connect directly instead of borrowing the
            // application pool: CI_BUILD_CONCURRENCY may be 64 while the
            // request pool is intentionally much smaller.
            let connection_options = self.pool.connect_options();
            let mut connection = timeout(
                DB_OPERATION_TIMEOUT,
                PgConnection::connect_with(connection_options.as_ref()),
            )
            .await
            .map_err(|_| BuildAdmissionError::Timeout {
                seconds: DB_OPERATION_TIMEOUT.as_secs(),
                detail: "connect PostgreSQL build-admission session timed out".into(),
            })?
            .map_err(|error| {
                BuildAdmissionError::Unavailable(format!(
                    "connect PostgreSQL build-admission session: {error}"
                ))
            })?;
            configure_connection(&mut connection).await?;
            if !try_advisory_lock(&mut connection, global_key).await? {
                drop(connection);
                tokio::time::sleep(self.lock_poll_interval).await;
                continue;
            }

            let mut selected = None;
            for slot in 0..self.concurrency {
                let slot_key = advisory_key(&self.builder_identity, u32::from(slot));
                if !try_advisory_lock(&mut connection, slot_key).await? {
                    continue;
                }
                let stale_token = sqlx::query_scalar::<_, Uuid>(
                    "SELECT lease_token
                       FROM ci_build_admission_leases
                      WHERE builder_identity = $1 AND slot = $2
                        AND heartbeat_at < now() - make_interval(secs => $3)",
                )
                .bind(&self.builder_identity)
                .bind(i16::try_from(slot).expect("bounded concurrency fits i16"))
                .bind(LEASE_STALE_AFTER.as_secs() as i32)
                .fetch_optional(&mut connection)
                .await
                .map_err(|error| {
                    BuildAdmissionError::Unavailable(format!(
                        "inspect stale build-admission lease: {error}"
                    ))
                })?;
                if let Some(token) = stale_token {
                    // Keep the row until external resources are reclaimed. If
                    // Docker or the bubblewrap workspace is temporarily
                    // unavailable, retaining the fenced row lets the next
                    // claimant retry cleanup instead of leaking an orphan.
                    self.cleaner
                        .remove_lease(&token.simple().to_string())
                        .await?;
                    sqlx::query(
                        "DELETE FROM ci_build_admission_leases
                         WHERE builder_identity = $1 AND slot = $2 AND lease_token = $3",
                    )
                    .bind(&self.builder_identity)
                    .bind(i16::try_from(slot).expect("bounded concurrency fits i16"))
                    .bind(token)
                    .execute(&mut connection)
                    .await
                    .map_err(|error| {
                        BuildAdmissionError::Unavailable(format!(
                            "fence stale build-admission lease: {error}"
                        ))
                    })?;
                }
                let occupied: bool = sqlx::query_scalar(
                    "SELECT EXISTS(
                        SELECT 1 FROM ci_build_admission_leases
                        WHERE builder_identity = $1 AND slot = $2
                    )",
                )
                .bind(&self.builder_identity)
                .bind(i16::try_from(slot).expect("bounded concurrency fits i16"))
                .fetch_one(&mut connection)
                .await
                .map_err(|error| {
                    BuildAdmissionError::Unavailable(format!(
                        "inspect build-admission slot: {error}"
                    ))
                })?;
                if !occupied {
                    selected = Some((slot, slot_key));
                    break;
                }
                let _ = unlock_advisory(&mut connection, slot_key).await;
            }
            let Some((slot, slot_key)) = selected else {
                let _ = bounded_unlock_and_close(connection, global_key).await;
                tokio::time::sleep(self.lock_poll_interval).await;
                continue;
            };

            let reserved_bytes: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(requested_memory_bytes), 0)::bigint
                   FROM ci_build_admission_leases
                  WHERE builder_identity = $1",
            )
            .bind(&self.builder_identity)
            .fetch_one(&mut connection)
            .await
            .map_err(|error| {
                BuildAdmissionError::Unavailable(format!(
                    "read active build memory reservations: {error}"
                ))
            })?;
            let reserved_bytes = u64::try_from(reserved_bytes).map_err(|_| {
                BuildAdmissionError::Unavailable("active build memory reservation overflow".into())
            })?;
            let snapshot = match self.memory_probe.snapshot().await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    let _ = unlock_advisory(&mut connection, global_key).await;
                    let _ = bounded_unlock_and_close(connection, slot_key).await;
                    return Err(error);
                }
            };
            if !snapshot.can_admit_with_reserved(
                requested_memory_bytes,
                self.system_reserve_bytes,
                reserved_bytes,
            ) {
                last_snapshot = Some(snapshot);
                let _ = unlock_advisory(&mut connection, global_key).await;
                let _ = bounded_unlock_and_close(connection, slot_key).await;
                tracing::warn!(
                    requested_memory_bytes,
                    reserved_memory_bytes = reserved_bytes,
                    available_memory_bytes = snapshot.available_bytes,
                    system_reserve_bytes = self.system_reserve_bytes,
                    "deferring CI build because host memory headroom is insufficient"
                );
                tokio::time::sleep(self.lock_poll_interval).await;
                continue;
            }

            let backend_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut connection)
                .await
                .map_err(|error| {
                    BuildAdmissionError::Unavailable(format!(
                        "identify PostgreSQL build-admission session: {error}"
                    ))
                })?;
            let token = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO ci_build_admission_leases
                 (builder_identity, slot, lease_token, owner_pid, requested_memory_bytes)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(&self.builder_identity)
            .bind(i16::try_from(slot).expect("bounded concurrency fits i16"))
            .bind(token)
            .bind(backend_pid)
            .bind(i64::try_from(requested_memory_bytes).map_err(|_| {
                BuildAdmissionError::Unavailable("build memory reservation overflow".into())
            })?)
            .execute(&mut connection)
            .await
            .map_err(|error| {
                BuildAdmissionError::Unavailable(format!("record build-admission lease: {error}"))
            })?;
            unlock_advisory(&mut connection, global_key).await?;
            tracing::info!(
                requested_memory_bytes,
                reserved_memory_bytes = reserved_bytes,
                available_memory_bytes = snapshot.available_bytes,
                concurrency = self.concurrency,
                slot,
                backend_pid,
                builder_identity = %self.builder_identity,
                "admitted host-wide CI build slot"
            );
            return Ok(BuildAdmissionLease::new(
                connection,
                self.pool.clone(),
                self.cleaner.clone(),
                self.builder_identity.clone(),
                self.builder_label.clone(),
                slot,
                slot_key,
                backend_pid,
                token,
                requested_memory_bytes,
                self.heartbeat_interval,
            ));
        }
    }
}

struct DockerAbandonedBuildCleaner {
    docker_host: String,
}
impl DockerAbandonedBuildCleaner {
    fn new(docker_host: String) -> Self {
        Self { docker_host }
    }
}
#[async_trait]
impl AbandonedBuildCleaner for DockerAbandonedBuildCleaner {
    async fn remove_lease(&self, lease_token: &str) -> Result<(), BuildAdmissionError> {
        let output = docker_command(&self.docker_host)
            .args([
                "ps",
                "-aq",
                "--filter",
                &format!("label=kurbis.ci.build.lease={lease_token}"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output();
        let output = timeout(DOCKER_TIMEOUT, output)
            .await
            .map_err(|_| {
                BuildAdmissionError::Unavailable("list lease-owned CI builds timed out".into())
            })?
            .map_err(|error| {
                BuildAdmissionError::Unavailable(format!("list lease-owned CI builds: {error}"))
            })?;
        if !output.status.success() {
            return Err(BuildAdmissionError::Unavailable(format!(
                "list lease-owned CI builds failed: {}",
                bounded_stderr(&output.stderr)
            )));
        }
        let containers = String::from_utf8_lossy(&output.stdout)
            .split_ascii_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if containers.is_empty() {
            return Ok(());
        }
        let output = docker_command(&self.docker_host)
            .args(["rm", "--force"])
            .args(containers)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output();
        let output = timeout(DOCKER_TIMEOUT, output)
            .await
            .map_err(|_| {
                BuildAdmissionError::Unavailable("remove lease-owned CI builds timed out".into())
            })?
            .map_err(|error| {
                BuildAdmissionError::Unavailable(format!("remove lease-owned CI builds: {error}"))
            })?;
        if output.status.success() || is_missing(&output.stderr) {
            Ok(())
        } else {
            Err(BuildAdmissionError::Unavailable(format!(
                "remove lease-owned CI builds failed: {}",
                bounded_stderr(&output.stderr)
            )))
        }
    }
}

pub(super) struct BuildAdmissionLease {
    pool: PgPool,
    cleaner: Arc<dyn AbandonedBuildCleaner>,
    builder_identity: String,
    builder_label: String,
    slot: u16,
    slot_key: i64,
    backend_pid: i32,
    lease_token: Uuid,
    requested_memory_bytes: u64,
    lost: Arc<AtomicBool>,
    heartbeat: Option<JoinHandle<()>>,
    heartbeat_stop: Option<oneshot::Sender<()>>,
    released: bool,
}
impl BuildAdmissionLease {
    #[allow(clippy::too_many_arguments)]
    fn new(
        connection: PgConnection,
        pool: PgPool,
        cleaner: Arc<dyn AbandonedBuildCleaner>,
        builder_identity: String,
        builder_label: String,
        slot: u16,
        slot_key: i64,
        backend_pid: i32,
        lease_token: Uuid,
        requested_memory_bytes: u64,
        heartbeat_interval: Duration,
    ) -> Self {
        let lost = Arc::new(AtomicBool::new(false));
        let (heartbeat_stop, stop_signal) = oneshot::channel();
        let heartbeat = Some(start_heartbeat(
            connection,
            builder_identity.clone(),
            slot,
            slot_key,
            backend_pid,
            lease_token,
            lost.clone(),
            heartbeat_interval,
            stop_signal,
        ));
        Self {
            pool,
            cleaner,
            builder_identity,
            builder_label,
            slot,
            slot_key,
            backend_pid,
            lease_token,
            requested_memory_bytes,
            lost,
            heartbeat,
            heartbeat_stop: Some(heartbeat_stop),
            released: false,
        }
    }

    pub(super) fn token(&self) -> String {
        self.lease_token.simple().to_string()
    }

    pub(super) fn builder_label(&self) -> &str {
        &self.builder_label
    }

    pub(super) fn assert_alive(&self) -> Result<(), BuildAdmissionError> {
        if self.lost.load(Ordering::Acquire) {
            return Err(BuildAdmissionError::Fenced(format!(
                "PostgreSQL build-admission slot {} for builder {} is no longer owned",
                self.slot, self.builder_identity
            )));
        }
        Ok(())
    }

    pub(super) async fn release(mut self) {
        self.released = true;
        if let Some(stop) = self.heartbeat_stop.take() {
            let _ = stop.send(());
        }
        if let Some(heartbeat) = self.heartbeat.take() {
            let _ = heartbeat.await;
        }
        let cleanup_succeeded = match self.cleaner.remove_lease(&self.token()).await {
            Ok(()) => true,
            Err(error) => {
                // Do not delete a lease whose external resources could not be
                // reclaimed. The dedicated advisory session is still released
                // below; the heartbeat row then acts as a retryable stale
                // fence instead of silently losing ownership information.
                tracing::warn!(
                    backend_pid = self.backend_pid,
                    ?error,
                    "failed to remove lease-owned CI resources; retaining lease row for stale cleanup"
                );
                false
            }
        };
        if cleanup_succeeded {
            let token = self.lease_token;
            if let Err(error) = timeout(
                DB_OPERATION_TIMEOUT,
                sqlx::query(
                    "DELETE FROM ci_build_admission_leases
                     WHERE builder_identity = $1 AND slot = $2 AND lease_token = $3",
                )
                .bind(&self.builder_identity)
                .bind(i16::try_from(self.slot).expect("u16 concurrency fits i16"))
                .bind(token)
                .execute(&self.pool),
            )
            .await
            .map_err(|_| "delete build-admission lease row timed out".to_owned())
            .and_then(|result| result.map_err(|error| error.to_string()))
            {
                tracing::warn!(
                    backend_pid = self.backend_pid,
                    ?error,
                    "failed to delete build-admission lease row; stale cleanup will retry"
                );
            }
        }
        tracing::debug!(
            backend_pid = self.backend_pid,
            requested_memory_bytes = self.requested_memory_bytes,
            slot_key = self.slot_key,
            "released PostgreSQL build-admission slot"
        );
    }
}
impl Drop for BuildAdmissionLease {
    fn drop(&mut self) {
        self.heartbeat_stop.take();
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
        if !self.released {
            let cleaner = self.cleaner.clone();
            let token = self.token();
            let pool = self.pool.clone();
            let identity = self.builder_identity.clone();
            let slot = self.slot;
            let lease_token = self.lease_token;
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let cleanup_succeeded = match cleaner.remove_lease(&token).await {
                        Ok(()) => true,
                        Err(error) => {
                            tracing::warn!(
                                ?error,
                                "failed to remove cancelled CI build resources; retaining stale lease row"
                            );
                            false
                        }
                    };
                    if cleanup_succeeded
                        && let Err(error) = sqlx::query(
                            "DELETE FROM ci_build_admission_leases
                             WHERE builder_identity = $1 AND slot = $2 AND lease_token = $3",
                        )
                        .bind(identity)
                        .bind(i16::try_from(slot).expect("u16 concurrency fits i16"))
                        .bind(lease_token)
                        .execute(&pool)
                        .await
                    {
                        tracing::warn!(
                            ?error,
                            "failed to delete cancelled CI build-admission lease row; stale cleanup will retry"
                        );
                    }
                });
            }
            tracing::info!(
                backend_pid = self.backend_pid,
                requested_memory_bytes = self.requested_memory_bytes,
                "dropped CI build admission lease; PostgreSQL slot and lease resources are being released"
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn start_heartbeat(
    mut connection: PgConnection,
    builder_identity: String,
    slot: u16,
    slot_key: i64,
    backend_pid: i32,
    lease_token: Uuid,
    lost: Arc<AtomicBool>,
    interval: Duration,
    mut stop_signal: oneshot::Receiver<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {
                    let updated = timeout(
                        DB_OPERATION_TIMEOUT,
                        sqlx::query(
                            "UPDATE ci_build_admission_leases
                             SET heartbeat_at = now()
                             WHERE builder_identity = $1 AND slot = $2 AND lease_token = $3",
                        )
                        .bind(&builder_identity)
                        .bind(i16::try_from(slot).expect("u16 concurrency fits i16"))
                        .bind(lease_token)
                        .execute(&mut connection),
                    )
                    .await
                    .map_err(|_| "heartbeat update timed out".to_owned())
                    .and_then(|result| result.map_err(|error| error.to_string()))
                    .map(|result| result.rows_affected() == 1);
                    match updated {
                        Ok(true) => {}
                        Ok(false) => {
                            lost.store(true, Ordering::Release);
                            tracing::error!(backend_pid, slot, "CI builder lease row was fenced");
                            break;
                        }
                        Err(error) => {
                            lost.store(true, Ordering::Release);
                            tracing::error!(backend_pid, slot, ?error, "CI builder admission heartbeat failed");
                            break;
                        }
                    }
                }
                _ = &mut stop_signal => {
                    break;
                }
            }
        }
        if let Err(error) = bounded_unlock_and_close(connection, slot_key).await {
            lost.store(true, Ordering::Release);
            tracing::warn!(
                backend_pid,
                slot,
                ?error,
                "failed to close PostgreSQL build-admission heartbeat session"
            );
        }
    })
}

async fn configure_connection(connection: &mut PgConnection) -> Result<(), BuildAdmissionError> {
    timeout(
        DB_OPERATION_TIMEOUT,
        sqlx::query("SET statement_timeout = '5000ms'").execute(&mut *connection),
    )
    .await
    .map_err(|_| BuildAdmissionError::Timeout {
        seconds: DB_OPERATION_TIMEOUT.as_secs(),
        detail: "configure PostgreSQL build-admission statement timeout timed out".into(),
    })?
    .map_err(|error| {
        BuildAdmissionError::Unavailable(format!(
            "configure PostgreSQL build-admission statement timeout: {error}"
        ))
    })?;
    Ok(())
}

async fn try_advisory_lock(
    connection: &mut PgConnection,
    key: i64,
) -> Result<bool, BuildAdmissionError> {
    sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(key)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| {
            BuildAdmissionError::Unavailable(format!(
                "acquire PostgreSQL build-admission lock: {error}"
            ))
        })
}

async fn unlock_advisory(
    connection: &mut PgConnection,
    key: i64,
) -> Result<(), BuildAdmissionError> {
    let unlocked: bool = timeout(
        DB_OPERATION_TIMEOUT,
        sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
            .bind(key)
            .fetch_one(&mut *connection),
    )
    .await
    .map_err(|_| {
        BuildAdmissionError::Unavailable("release PostgreSQL build-admission lock timed out".into())
    })?
    .map_err(|error| {
        BuildAdmissionError::Unavailable(format!(
            "release PostgreSQL build-admission lock: {error}"
        ))
    })?;
    if unlocked {
        Ok(())
    } else {
        Err(BuildAdmissionError::Fenced(
            "PostgreSQL advisory lock was not held".into(),
        ))
    }
}

async fn unlock_and_close(mut connection: PgConnection, slot_key: i64) -> Result<(), sqlx::Error> {
    let _unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
        .bind(slot_key)
        .fetch_one(&mut connection)
        .await?;
    connection.close().await
}

async fn bounded_unlock_and_close(connection: PgConnection, slot_key: i64) -> Result<(), String> {
    timeout(DB_OPERATION_TIMEOUT, unlock_and_close(connection, slot_key))
        .await
        .map_err(|_| "release PostgreSQL build-admission session timed out".to_owned())?
        .map_err(|error| format!("release PostgreSQL build-admission session: {error}"))
}

fn advisory_key(builder_identity: &str, slot: u32) -> i64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"kurbis-ci-build-admission-v2\0");
    hasher.update(builder_identity.as_bytes());
    hasher.update(&slot.to_be_bytes());
    let bytes = hasher.finalize().as_bytes()[..8]
        .try_into()
        .expect("blake3 digest has at least eight bytes");
    i64::from_be_bytes(bytes) & i64::MAX
}

struct DockerHostMemoryProbe {
    image: String,
    docker_host: String,
}
impl DockerHostMemoryProbe {
    fn new(image: String, docker_host: String) -> Self {
        Self { image, docker_host }
    }
}
#[async_trait]
impl HostMemoryProbe for DockerHostMemoryProbe {
    async fn snapshot(&self) -> Result<HostMemorySnapshot, BuildAdmissionError> {
        let output = docker_command(&self.docker_host)
            .args([
                "run",
                "--rm",
                "--pull=never",
                "--network=none",
                "--memory",
                "67108864",
                "--memory-swap",
                "67108864",
                "--cpu-period",
                "100000",
                "--cpu-quota",
                "10000",
                "--pids-limit",
                "16",
                "--read-only",
                "--tmpfs",
                "/tmp:rw,noexec,nosuid,nodev,size=4m",
                "--entrypoint",
                "/bin/sh",
                &self.image,
                "-ec",
                "cat /proc/meminfo",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output();
        let output = timeout(DOCKER_TIMEOUT, output)
            .await
            .map_err(|_| BuildAdmissionError::Unavailable("host memory probe timed out".into()))?
            .map_err(|error| {
                BuildAdmissionError::Unavailable(format!("host memory probe: {error}"))
            })?;
        if !output.status.success() {
            return Err(BuildAdmissionError::Unavailable(format!(
                "host memory probe failed: {}",
                bounded_stderr(&output.stderr)
            )));
        }
        parse_meminfo(&String::from_utf8_lossy(&output.stdout)).map_err(|error| {
            BuildAdmissionError::Unavailable(format!(
                "host memory probe returned invalid data: {error}"
            ))
        })
    }
}

fn docker_command(docker_host: &str) -> Command {
    let mut command = Command::new("docker");
    command.args(["--host", docker_host]);
    command
}

pub(super) fn parse_meminfo(contents: &str) -> Result<HostMemorySnapshot, &'static str> {
    let mut values = HashMap::new();
    for line in contents.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let mut parts = rest.split_ascii_whitespace();
        let Some(value) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
            continue;
        };
        let multiplier = match parts.next() {
            Some("kB") => 1024,
            Some("MB") => 1024 * 1024,
            Some("B") | None => 1,
            _ => return Err("unknown /proc/meminfo unit"),
        };
        values.insert(name, value.saturating_mul(multiplier));
    }
    Ok(HostMemorySnapshot {
        total_bytes: *values.get("MemTotal").ok_or("MemTotal is missing")?,
        available_bytes: *values
            .get("MemAvailable")
            .ok_or("MemAvailable is missing")?,
        swap_total_bytes: *values.get("SwapTotal").ok_or("SwapTotal is missing")?,
        swap_free_bytes: *values.get("SwapFree").ok_or("SwapFree is missing")?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CgroupMemoryEvents {
    pub oom: u64,
    pub oom_kill: u64,
    pub max: u64,
}
impl CgroupMemoryEvents {
    pub fn classify(self) -> &'static str {
        if self.oom_kill > 0 {
            "cgroup_oom_kill"
        } else if self.oom > 0 {
            "cgroup_oom_without_kill"
        } else if self.max > 0 {
            "cgroup_memory_max_reclaim"
        } else {
            "no_cgroup_oom_evidence"
        }
    }
}

pub(super) fn parse_cgroup_memory_events(
    contents: &str,
) -> Result<CgroupMemoryEvents, &'static str> {
    let mut values = HashMap::new();
    for line in contents.lines() {
        let mut parts = line.split_ascii_whitespace();
        let Some(name) = parts.next() else { continue };
        let Some(value) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
            return Err("cgroup memory event value is not an unsigned integer");
        };
        if parts.next().is_some() {
            return Err("cgroup memory event line has extra fields");
        }
        values.insert(name, value);
    }
    Ok(CgroupMemoryEvents {
        oom: *values.get("oom").ok_or("oom event counter is missing")?,
        oom_kill: *values
            .get("oom_kill")
            .ok_or("oom_kill event counter is missing")?,
        max: *values.get("max").ok_or("max event counter is missing")?,
    })
}

fn bounded_stderr(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes).trim().to_owned();
    if text.len() <= 1024 {
        text
    } else {
        format!("…{}", &text[text.len() - 1024..])
    }
}

fn is_missing(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.contains("No such container") || text.contains("not found")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::{str::FromStr, sync::Mutex};

    #[test]
    fn configured_build_concurrency_is_bounded_and_defaults_to_two_slots() {
        assert_eq!(parse_build_concurrency(None).unwrap(), 2);
        assert_eq!(parse_build_concurrency(Some("1")).unwrap(), 1);
        assert_eq!(parse_build_concurrency(Some("64")).unwrap(), 64);
        assert!(parse_build_concurrency(Some("0")).is_err());
        assert!(parse_build_concurrency(Some("65")).is_err());
        assert!(parse_build_concurrency(Some("many")).is_err());
    }

    #[test]
    fn host_admission_includes_build_and_system_headroom() {
        let snapshot = HostMemorySnapshot {
            total_bytes: 23 * 1024 * 1024 * 1024,
            available_bytes: 4_900 * 1024 * 1024,
            swap_total_bytes: 0,
            swap_free_bytes: 0,
        };
        assert!(!snapshot.can_admit_with_reserved(
            10 * 1024 * 1024 * 1024,
            4 * 1024 * 1024 * 1024,
            0
        ));
        let safe_snapshot = HostMemorySnapshot {
            available_bytes: 15 * 1024 * 1024 * 1024,
            ..snapshot
        };
        assert!(safe_snapshot.can_admit_with_reserved(
            10 * 1024 * 1024 * 1024,
            4 * 1024 * 1024 * 1024,
            0
        ));
        assert!(!safe_snapshot.can_admit_with_reserved(u64::MAX, 4 * 1024 * 1024 * 1024, 0));
    }

    #[test]
    fn cgroup_events_distinguish_kills_from_reclaim() {
        let oom = parse_cgroup_memory_events("low 0\nhigh 2\nmax 3\noom 1\noom_kill 1\n").unwrap();
        assert_eq!(oom.classify(), "cgroup_oom_kill");
        let reclaim =
            parse_cgroup_memory_events("low 0\nhigh 0\nmax 2\noom 0\noom_kill 0\n").unwrap();
        assert_eq!(reclaim.classify(), "cgroup_memory_max_reclaim");
        assert!(parse_cgroup_memory_events("oom 0\noom_kill nope\nmax 0").is_err());
    }

    #[test]
    fn parse_meminfo_uses_bytes_and_rejects_missing_values() {
        let snapshot = parse_meminfo(
            "MemTotal:       1024 kB\nMemAvailable:    512 kB\nSwapTotal:        0 kB\nSwapFree:         0 kB\n",
        )
        .unwrap();
        assert_eq!(snapshot.total_bytes, 1024 * 1024);
        assert_eq!(snapshot.available_bytes, 512 * 1024);
        assert_eq!(snapshot.swap_total_bytes, 0);
        assert!(parse_meminfo("MemTotal: 1 kB\n").is_err());
    }

    struct HealthyHost;
    #[async_trait]
    impl HostMemoryProbe for HealthyHost {
        async fn snapshot(&self) -> Result<HostMemorySnapshot, BuildAdmissionError> {
            Ok(HostMemorySnapshot {
                total_bytes: 24 * 1024 * 1024 * 1024,
                available_bytes: 16 * 1024 * 1024 * 1024,
                swap_total_bytes: 0,
                swap_free_bytes: 0,
            })
        }
    }

    #[derive(Clone)]
    struct RecordingCleaner {
        tokens: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl AbandonedBuildCleaner for RecordingCleaner {
        async fn remove_lease(&self, lease_token: &str) -> Result<(), BuildAdmissionError> {
            self.tokens
                .lock()
                .expect("recording cleaner mutex is not poisoned")
                .push(lease_token.to_owned());
            Ok(())
        }
    }

    struct DisposableAdmissionDatabase {
        pool: PgPool,
        admin: PgPool,
        name: String,
    }

    impl DisposableAdmissionDatabase {
        async fn create() -> Self {
            let url = std::env::var("TEST_DATABASE_URL")
                .expect("TEST_DATABASE_URL must point to disposable local PostgreSQL");
            let options = PgConnectOptions::from_str(&url).expect("valid TEST_DATABASE_URL");
            let admin = PgPoolOptions::new()
                .max_connections(2)
                .connect_with(options.clone().database("postgres"))
                .await
                .expect("connect to local PostgreSQL admin database");
            let name = format!("kurbis_admission_{}", Uuid::new_v4().simple());
            sqlx::query(&format!("CREATE DATABASE {name}"))
                .execute(&admin)
                .await
                .expect("create disposable admission database");
            let pool = PgPoolOptions::new()
                .max_connections(8)
                .connect_with(options.database(&name))
                .await
                .expect("connect to disposable admission database");
            crate::db::migrate(&pool)
                .await
                .expect("apply canonical schema to disposable admission database");
            Self { pool, admin, name }
        }

        async fn cleanup(self) {
            self.pool.close().await;
            sqlx::query(
                "SELECT pg_terminate_backend(pid)
                   FROM pg_stat_activity
                  WHERE datname = $1 AND pid <> pg_backend_pid()",
            )
            .bind(&self.name)
            .execute(&self.admin)
            .await
            .expect("terminate remaining disposable database sessions");
            sqlx::query(&format!("DROP DATABASE {}", self.name))
                .execute(&self.admin)
                .await
                .expect("drop disposable admission database");
            self.admin.close().await;
        }
    }

    fn coordinator(
        pool: PgPool,
        identity: &'static str,
        concurrency: usize,
        cleaner: Arc<dyn AbandonedBuildCleaner>,
    ) -> BuildAdmissionCoordinator {
        BuildAdmissionCoordinator::for_builder(pool, identity, Arc::new(HealthyHost), cleaner)
            .with_concurrency(concurrency)
            .expect("valid test concurrency")
            .with_timing(
                Duration::from_millis(10),
                Duration::from_millis(25),
                Duration::from_millis(180),
            )
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL; uses disposable PostgreSQL admission database"]
    async fn postgres_admission_enforces_slots_cancellation_stale_fencing_and_identity_scope() {
        let database = DisposableAdmissionDatabase::create().await;
        let pool = database.pool.clone();
        let cleaner_tokens = Arc::new(Mutex::new(Vec::new()));
        let cleaner: Arc<dyn AbandonedBuildCleaner> = Arc::new(RecordingCleaner {
            tokens: cleaner_tokens.clone(),
        });

        let first = coordinator(pool.clone(), "docker:test-host:image-a", 2, cleaner.clone());
        let second = coordinator(pool.clone(), "docker:test-host:image-a", 2, cleaner.clone());
        let third = coordinator(pool.clone(), "docker:test-host:image-a", 2, cleaner.clone());
        let lease_a = first.acquire(1024).await.expect("first slot is admitted");
        let lease_b = second.acquire(1024).await.expect("second slot is admitted");
        assert!(matches!(
            third.acquire(1024).await,
            Err(BuildAdmissionError::Timeout { .. })
        ));
        lease_a.release().await;
        lease_b.release().await;

        let cancelling = coordinator(
            pool.clone(),
            "docker:test-host:cancellation",
            1,
            cleaner.clone(),
        );
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let lease = cancelling.acquire(1024).await.expect("cancellation lease");
            ready_tx.send(()).expect("signal admitted lease");
            std::future::pending::<()>().await;
            lease
        });
        ready_rx.await.expect("receive cancellation admission");
        task.abort();
        let _ = task.await;
        let recovered = tokio::time::timeout(
            Duration::from_secs(3),
            coordinator(
                pool.clone(),
                "docker:test-host:cancellation",
                1,
                cleaner.clone(),
            )
            .acquire(1024),
        )
        .await
        .expect("cancellation releases the slot")
        .expect("slot after cancellation is admitted");
        recovered.release().await;

        let stale_identity = "bubblewrap:test-host:stale";
        let stale_token = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO ci_build_admission_leases
             (builder_identity, slot, lease_token, owner_pid, requested_memory_bytes, heartbeat_at)
             VALUES ($1, 0, $2, 999999, 1024, now() - interval '1 hour')",
        )
        .bind(stale_identity)
        .bind(stale_token)
        .execute(&pool)
        .await
        .expect("insert stale admission row");
        let stale_reclaimed = coordinator(pool.clone(), stale_identity, 1, cleaner.clone())
            .acquire(1024)
            .await
            .expect("stale slot is reclaimed");
        stale_reclaimed.release().await;
        assert!(
            cleaner_tokens
                .lock()
                .expect("recording cleaner mutex is not poisoned")
                .iter()
                .any(|token| token == &stale_token.simple().to_string())
        );

        let docker_identity = coordinator(
            pool.clone(),
            "docker:test-host:identity-a",
            1,
            cleaner.clone(),
        );
        let bubblewrap_identity = coordinator(
            pool.clone(),
            "bubblewrap:test-host:identity-b",
            1,
            cleaner.clone(),
        );
        let docker_lease = docker_identity
            .acquire(1024)
            .await
            .expect("first builder identity is admitted");
        let bubblewrap_lease = bubblewrap_identity
            .acquire(1024)
            .await
            .expect("distinct builder identity is admitted independently");
        docker_lease.release().await;
        bubblewrap_lease.release().await;

        // Admission sessions are direct PostgreSQL connections rather than
        // borrowed application-pool slots, so the configured upper bound does
        // not starve the control-plane pool when an operator selects 64 slots.
        let high_concurrency = coordinator(
            pool.clone(),
            "bubblewrap:test-host:high-concurrency",
            64,
            cleaner.clone(),
        );
        let mut leases = Vec::with_capacity(64);
        for _ in 0..64 {
            leases.push(
                high_concurrency
                    .acquire(1024)
                    .await
                    .expect("all configured high-concurrency slots are independently admitted"),
            );
        }
        for lease in leases {
            lease.release().await;
        }
        database.cleanup().await;
    }
}
