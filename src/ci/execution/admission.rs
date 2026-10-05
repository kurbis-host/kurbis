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
use sqlx::{PgPool, Postgres, pool::PoolConnection};
use thiserror::Error;
use tokio::{process::Command, task::JoinHandle, time::timeout};
use uuid::Uuid;

const BUILD_LOCK_CLASS: i32 = 724_517;
const BUILD_LOCK_OBJECT: i32 = 0;
const ABANDONED_BUILD_FILTER: &str = "label=kurbis.ci.build=true";
const DEFAULT_SYSTEM_RESERVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const LOCK_POLL_INTERVAL: Duration = Duration::from_secs(2);
const LOCK_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const DOCKER_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum BuildAdmissionError {
    #[error("host build admission timed out after {seconds} seconds: {detail}")]
    Timeout { seconds: u64, detail: String },
    #[error("host build admission is unavailable: {0}")]
    Unavailable(String),
    #[error("host build admission was fenced: {0}")]
    Fenced(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HostMemorySnapshot {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_free_bytes: u64,
}
impl HostMemorySnapshot {
    pub(super) fn required_bytes(self, requested_bytes: u64, reserve_bytes: u64) -> Option<u64> {
        requested_bytes.checked_add(reserve_bytes)
    }

    pub(super) fn can_admit(self, requested_bytes: u64, reserve_bytes: u64) -> bool {
        self.total_bytes > 0
            && self.available_bytes > 0
            && self.available_bytes <= self.total_bytes
            && self
                .required_bytes(requested_bytes, reserve_bytes)
                .is_some_and(|required| self.available_bytes >= required)
    }
}

#[async_trait]
pub(super) trait HostMemoryProbe: Send + Sync {
    async fn snapshot(&self) -> Result<HostMemorySnapshot, BuildAdmissionError>;
}

#[async_trait]
pub(super) trait AbandonedBuildCleaner: Send + Sync {
    async fn remove_abandoned(&self) -> Result<(), BuildAdmissionError>;
    async fn remove_lease(&self, lease_token: &str) -> Result<(), BuildAdmissionError>;
}

#[derive(Clone)]
pub(super) struct DockerBuildAdmission {
    pool: PgPool,
    memory_probe: Arc<dyn HostMemoryProbe>,
    cleaner: Arc<dyn AbandonedBuildCleaner>,
    system_reserve_bytes: u64,
    lock_poll_interval: Duration,
    heartbeat_interval: Duration,
    wait_timeout: Duration,
}
impl DockerBuildAdmission {
    pub fn new(pool: PgPool, image: impl Into<String>, docker_host: impl Into<String>) -> Self {
        let image = image.into();
        let docker_host = docker_host.into();
        Self {
            pool,
            memory_probe: Arc::new(DockerHostMemoryProbe::new(image, docker_host.clone())),
            cleaner: Arc::new(DockerAbandonedBuildCleaner::new(docker_host)),
            system_reserve_bytes: DEFAULT_SYSTEM_RESERVE_BYTES,
            lock_poll_interval: LOCK_POLL_INTERVAL,
            heartbeat_interval: LOCK_HEARTBEAT_INTERVAL,
            wait_timeout: Duration::from_secs(10 * 60),
        }
    }

    #[cfg(test)]
    pub(super) fn with_memory_probe(mut self, probe: Arc<dyn HostMemoryProbe>) -> Self {
        self.memory_probe = probe;
        self
    }

    #[cfg(test)]
    pub(super) fn with_cleaner(mut self, cleaner: Arc<dyn AbandonedBuildCleaner>) -> Self {
        self.cleaner = cleaner;
        self
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
        loop {
            if tokio::time::Instant::now() >= deadline {
                let detail = last_snapshot
                    .map(|snapshot: HostMemorySnapshot| {
                        format!(
                            "available={} MiB of {} MiB; build requires {} MiB plus {} MiB reserved for system/runtime; swap free={} MiB",
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

            let mut connection = self.pool.acquire().await.map_err(|error| {
                BuildAdmissionError::Unavailable(format!(
                    "acquire PostgreSQL build-admission session: {error}"
                ))
            })?;
            // Never return an advisory-lock session to the shared application pool.
            connection.close_on_drop();
            let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1, $2)")
                .bind(BUILD_LOCK_CLASS)
                .bind(BUILD_LOCK_OBJECT)
                .fetch_one(&mut *connection)
                .await
                .map_err(|error| {
                    BuildAdmissionError::Unavailable(format!(
                        "acquire PostgreSQL build-admission lock: {error}"
                    ))
                })?;
            if !acquired {
                drop(connection);
                tokio::time::sleep(self.lock_poll_interval).await;
                continue;
            }

            let backend_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut *connection)
                .await
                .map_err(|error| {
                    BuildAdmissionError::Unavailable(format!(
                        "identify PostgreSQL build-admission session: {error}"
                    ))
                })?;

            // A prior process can die while Docker keeps its container alive.
            // The global session lock proves there is no current Kurbis builder
            // owner, so reclaim only containers carrying the dedicated label.
            if let Err(error) = self.cleaner.remove_abandoned().await {
                let _ = unlock_and_close(connection).await;
                return Err(error);
            }

            match self.memory_probe.snapshot().await {
                Ok(snapshot)
                    if snapshot.can_admit(requested_memory_bytes, self.system_reserve_bytes) =>
                {
                    tracing::info!(
                        requested_memory_bytes,
                        total_memory_bytes = snapshot.total_bytes,
                        available_memory_bytes = snapshot.available_bytes,
                        swap_total_bytes = snapshot.swap_total_bytes,
                        swap_free_bytes = snapshot.swap_free_bytes,
                        system_reserve_bytes = self.system_reserve_bytes,
                        backend_pid,
                        "admitted host-wide CI build"
                    );
                    return Ok(BuildAdmissionLease::new(
                        connection,
                        self.pool.clone(),
                        self.cleaner.clone(),
                        backend_pid,
                        Uuid::new_v4().simple().to_string(),
                        requested_memory_bytes,
                        self.heartbeat_interval,
                    ));
                }
                Ok(snapshot) => {
                    last_snapshot = Some(snapshot);
                    tracing::warn!(
                        requested_memory_bytes,
                        total_memory_bytes = snapshot.total_bytes,
                        available_memory_bytes = snapshot.available_bytes,
                        swap_total_bytes = snapshot.swap_total_bytes,
                        swap_free_bytes = snapshot.swap_free_bytes,
                        system_reserve_bytes = self.system_reserve_bytes,
                        "deferring CI build because host memory headroom is insufficient"
                    );
                }
                Err(error) => {
                    let _ = unlock_and_close(connection).await;
                    return Err(error);
                }
            }
            unlock_and_close(connection).await.map_err(|error| {
                BuildAdmissionError::Unavailable(format!(
                    "release build-admission lock after memory deferral: {error}"
                ))
            })?;
            tokio::time::sleep(self.lock_poll_interval).await;
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
    async fn remove_abandoned(&self) -> Result<(), BuildAdmissionError> {
        let output = docker_command(&self.docker_host)
            .args(["ps", "-aq", "--filter", ABANDONED_BUILD_FILTER])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output();
        let output = timeout(DOCKER_TIMEOUT, output)
            .await
            .map_err(|_| {
                BuildAdmissionError::Unavailable(
                    "list abandoned CI build containers timed out".into(),
                )
            })?
            .map_err(|error| {
                BuildAdmissionError::Unavailable(format!(
                    "list abandoned CI build containers: {error}"
                ))
            })?;
        if !output.status.success() {
            return Err(BuildAdmissionError::Unavailable(format!(
                "list abandoned CI build containers failed: {}",
                bounded_stderr(&output.stderr)
            )));
        }
        let ids = String::from_utf8_lossy(&output.stdout)
            .split_ascii_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(());
        }
        let output = docker_command(&self.docker_host)
            .args(["rm", "--force"])
            .args(&ids)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output();
        let output = timeout(DOCKER_TIMEOUT, output)
            .await
            .map_err(|_| {
                BuildAdmissionError::Unavailable(
                    "remove abandoned CI build containers timed out".into(),
                )
            })?
            .map_err(|error| {
                BuildAdmissionError::Unavailable(format!(
                    "remove abandoned CI build containers: {error}"
                ))
            })?;
        if output.status.success() || is_missing(&output.stderr) {
            Ok(())
        } else {
            Err(BuildAdmissionError::Unavailable(format!(
                "remove abandoned CI build containers failed: {}",
                bounded_stderr(&output.stderr)
            )))
        }
    }

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
    connection: Option<PoolConnection<Postgres>>,
    cleaner: Arc<dyn AbandonedBuildCleaner>,
    backend_pid: i32,
    lease_token: String,
    requested_memory_bytes: u64,
    lost: Arc<AtomicBool>,
    heartbeat: Option<JoinHandle<()>>,
    released: bool,
}
impl BuildAdmissionLease {
    fn new(
        connection: PoolConnection<Postgres>,
        pool: PgPool,
        cleaner: Arc<dyn AbandonedBuildCleaner>,
        backend_pid: i32,
        lease_token: String,
        requested_memory_bytes: u64,
        heartbeat_interval: Duration,
    ) -> Self {
        let lost = Arc::new(AtomicBool::new(false));
        let heartbeat = Some(start_heartbeat(
            pool.clone(),
            backend_pid,
            lost.clone(),
            heartbeat_interval,
        ));
        Self {
            connection: Some(connection),
            cleaner,
            backend_pid,
            lease_token,
            requested_memory_bytes,
            lost,
            heartbeat,
            released: false,
        }
    }

    pub(super) fn token(&self) -> &str {
        &self.lease_token
    }

    pub(super) fn assert_alive(&self) -> Result<(), BuildAdmissionError> {
        if self.lost.load(Ordering::Acquire) {
            return Err(BuildAdmissionError::Fenced(format!(
                "PostgreSQL build-admission session {} no longer owns the advisory lock",
                self.backend_pid
            )));
        }
        Ok(())
    }

    pub(super) async fn release(mut self) {
        self.released = true;
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
        if let Err(error) = self.cleaner.remove_lease(&self.lease_token).await {
            tracing::warn!(
                backend_pid = self.backend_pid,
                ?error,
                "failed to remove lease-owned CI containers before releasing admission lock"
            );
        }
        if let Some(connection) = self.connection.take()
            && let Err(error) = unlock_and_close(connection).await
        {
            tracing::warn!(
                backend_pid = self.backend_pid,
                requested_memory_bytes = self.requested_memory_bytes,
                ?error,
                "failed to explicitly release PostgreSQL build-admission lock; closing its dedicated session"
            );
        }
    }
}
impl Drop for BuildAdmissionLease {
    fn drop(&mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
        // close_on_drop is set before the session lock is acquired. A cancelled
        // task therefore closes the dedicated DB session instead of returning a
        // still-locked connection to the shared pool.
        self.connection.take();
        if !self.released {
            let cleaner = self.cleaner.clone();
            let token = self.lease_token.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    if let Err(error) = cleaner.remove_lease(&token).await {
                        tracing::warn!(?error, "failed to remove cancelled CI build containers");
                    }
                });
            }
            tracing::info!(
                backend_pid = self.backend_pid,
                requested_memory_bytes = self.requested_memory_bytes,
                "dropped CI build admission lease; PostgreSQL session and lease-owned containers are being released"
            );
        }
    }
}

fn start_heartbeat(
    pool: PgPool,
    backend_pid: i32,
    lost: Arc<AtomicBool>,
    interval: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            let owns_lock = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(
                    SELECT 1 FROM pg_locks
                    WHERE locktype = 'advisory' AND pid = $1 AND granted
                      AND classid = $2::oid AND objid = $3::oid AND objsubid = 2
                )",
            )
            .bind(backend_pid)
            .bind(BUILD_LOCK_CLASS)
            .bind(BUILD_LOCK_OBJECT)
            .fetch_one(&pool)
            .await;
            match owns_lock {
                Ok(true) => {}
                Ok(false) => {
                    lost.store(true, Ordering::Release);
                    tracing::error!(backend_pid, "CI builder lost its PostgreSQL admission lock");
                    break;
                }
                Err(error) => {
                    lost.store(true, Ordering::Release);
                    tracing::error!(backend_pid, ?error, "CI builder admission heartbeat failed");
                    break;
                }
            }
        }
    })
}

async fn unlock_and_close(mut connection: PoolConnection<Postgres>) -> Result<(), sqlx::Error> {
    let _unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1, $2)")
        .bind(BUILD_LOCK_CLASS)
        .bind(BUILD_LOCK_OBJECT)
        .fetch_one(&mut *connection)
        .await?;
    connection.close().await
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

fn parse_meminfo(contents: &str) -> Result<HostMemorySnapshot, &'static str> {
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
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn host_admission_includes_build_and_system_headroom() {
        let snapshot = HostMemorySnapshot {
            total_bytes: 23 * 1024 * 1024 * 1024,
            available_bytes: 4_900 * 1024 * 1024,
            swap_total_bytes: 0,
            swap_free_bytes: 0,
        };
        assert!(!snapshot.can_admit(10 * 1024 * 1024 * 1024, 4 * 1024 * 1024 * 1024));
        let safe_snapshot = HostMemorySnapshot {
            available_bytes: 15 * 1024 * 1024 * 1024,
            ..snapshot
        };
        assert!(safe_snapshot.can_admit(10 * 1024 * 1024 * 1024, 4 * 1024 * 1024 * 1024));
        assert!(!safe_snapshot.can_admit(u64::MAX, 4 * 1024 * 1024 * 1024));
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
    struct NoAbandonedBuilds;
    #[async_trait]
    impl AbandonedBuildCleaner for NoAbandonedBuilds {
        async fn remove_abandoned(&self) -> Result<(), BuildAdmissionError> {
            Ok(())
        }
        async fn remove_lease(&self, _: &str) -> Result<(), BuildAdmissionError> {
            Ok(())
        }
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL; exercises real PostgreSQL session locks"]
    async fn independent_postgres_sessions_serialize_and_cancel_releases_lock() {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
        let pool_a = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        let pool_b = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        let make = |pool| {
            DockerBuildAdmission::new(
                pool,
                "unused-builder@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "ssh://unused@builder.invalid",
            )
            .with_memory_probe(Arc::new(HealthyHost))
            .with_cleaner(Arc::new(NoAbandonedBuilds))
            .with_timing(Duration::from_millis(10), Duration::from_millis(25), Duration::from_millis(160))
        };
        let first = make(pool_a.clone());
        let second = make(pool_b.clone());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let owner = tokio::spawn(async move {
            let _lease = first.acquire(1024).await.unwrap();
            tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        rx.await.unwrap();
        assert!(matches!(
            second.acquire(1024).await,
            Err(BuildAdmissionError::Timeout { .. })
        ));
        owner.abort();
        let _ = owner.await;
        let recovered = tokio::time::timeout(Duration::from_secs(3), second.acquire(1024))
            .await
            .unwrap()
            .unwrap();
        recovered.release().await;
        pool_a.close().await;
        pool_b.close().await;
    }
}
