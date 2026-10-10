use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use chrono::Utc;
use kurbis_hoster::ci::{
    ArtifactCache, BuildKey, BuildPipelineError, BuildRequest, CheckRunClient, CiBuildStage,
    CiHostname, CiPoller, CiPollerConfig, CiPreviewRepository, CiPreviewRuntimeCoordinator,
    CiPreviewRuntimeHost, CiPreviewRuntimeRepository, CiPreviewState, CiRefKind,
    CiRuntimePlacement, CiRuntimeState, ControlledBuildOutput, ControlledBuildPipeline,
    ControlledBuilder, PatchApplier, PollCheckpoint, PollError, PollResult, RuntimeCapability,
    SandboxLimits, SourceControlClient, SourceRef, SourceRefKind,
};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

const REPOSITORY: &str = "Pumpkin-MC/Pumpkin";
const COMMIT_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const TARGET_ARCHITECTURE: &str = "x86_64-linux";
const TOOLCHAIN: &str = "rust-1.98@sha256:milestone5";
const PATCH_FINGERPRINT: &str = "milestone5-patch";
const ROOT_DOMAIN: &str = "milestone5.pumpkin.kurbis.host";

#[derive(Clone)]
struct PrObservationSource {
    polls: Arc<Mutex<usize>>,
    fetches: Arc<Mutex<Vec<(String, String)>>>,
}

impl PrObservationSource {
    fn new() -> Self {
        Self {
            polls: Arc::new(Mutex::new(0)),
            fetches: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl SourceControlClient for PrObservationSource {
    async fn poll(
        &self,
        _repository: &str,
        checkpoint: &PollCheckpoint,
    ) -> Result<PollResult, PollError> {
        let mut polls = self.polls.lock().expect("poll fixture lock");
        *polls += 1;
        let observed = SourceRef {
            kind: SourceRefKind::PullRequest,
            name: "pr-5005".to_owned(),
            number: Some(5005),
            commit_sha: COMMIT_SHA.to_owned(),
            source_repository: Some("contributor/Pumpkin".to_owned()),
            closed: false,
            deleted: false,
            updated_at: Some(Utc::now()),
        };
        // Eager work is intentionally disabled.  The first poll is the
        // admission boundary and leaves a durable queued build for the next
        // coordinator tick, making the queued/coalesced state observable.
        Ok(PollResult {
            refs: vec![observed],
            eager_refs: Vec::new(),
            checkpoint: PollCheckpoint {
                etag: Some("milestone5-pr-v1".to_owned()),
                last_successful_poll: Some(Utc::now()),
                ..checkpoint.clone()
            },
            not_modified: false,
            retry_after: None,
            complete_kinds: vec![SourceRefKind::PullRequest],
        })
    }

    async fn fetch_commit(&self, repository: &str, commit_sha: &str) -> Result<Vec<u8>, PollError> {
        self.fetches
            .lock()
            .expect("fetch fixture lock")
            .push((repository.to_owned(), commit_sha.to_owned()));
        Ok(format!("remote-source:{repository}:{commit_sha}").into_bytes())
    }
}

#[derive(Debug, Clone)]
struct RemoteBuildReceipt {
    source_commit: String,
    runtime_revision: String,
    artifact_digest: String,
    artifact_size: usize,
}

#[derive(Default)]
struct RemoteReceiptBuilder {
    compile_calls: std::sync::atomic::AtomicUsize,
    receipts: Mutex<Vec<RemoteBuildReceipt>>,
    fail_compile: std::sync::atomic::AtomicBool,
}

struct ReceiptPatcher;

#[async_trait]
impl PatchApplier for ReceiptPatcher {
    async fn apply(
        &self,
        mut source_archive: Vec<u8>,
        fingerprint: &str,
    ) -> Result<Vec<u8>, BuildPipelineError> {
        source_archive.extend_from_slice(b"|patched:");
        source_archive.extend_from_slice(fingerprint.as_bytes());
        Ok(source_archive)
    }
}

#[async_trait]
impl ControlledBuilder for RemoteReceiptBuilder {
    async fn verify_sandbox(&self, limits: SandboxLimits) -> Result<(), BuildPipelineError> {
        if limits.memory_bytes == 0
            || limits.cpu_millis == 0
            || limits.pid_limit == 0
            || limits.disk_bytes == 0
            || limits.process_timeout.is_zero()
        {
            return Err(BuildPipelineError::Sandbox(
                "remote acceptance fixture received unsupported zero limits".to_owned(),
            ));
        }
        Ok(())
    }

    async fn compile_and_package(
        &self,
        patched_source: Vec<u8>,
        key: &BuildKey,
    ) -> Result<ControlledBuildOutput, BuildPipelineError> {
        self.compile_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail_compile.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(BuildPipelineError::Transient {
                stage: CiBuildStage::Compiling,
                message: "remote receipt host unavailable".to_owned(),
            });
        }
        let mut artifact = patched_source;
        artifact.extend_from_slice(b"|pumpkin-release-artifact|runtime-revision:2026.10.11");
        let digest = blake3::hash(&artifact).to_hex().to_string();
        let receipt = RemoteBuildReceipt {
            source_commit: key.commit_sha.clone(),
            runtime_revision: "2026.10.11".to_owned(),
            artifact_digest: digest,
            artifact_size: artifact.len(),
        };
        self.receipts
            .lock()
            .expect("receipt fixture lock")
            .push(receipt.clone());
        let receipt_json = serde_json::json!({
            "kind": "remote_build_receipt",
            "source_commit": receipt.source_commit,
            "runtime_revision": receipt.runtime_revision,
            "artifact_digest": receipt.artifact_digest,
            "artifact_size": receipt.artifact_size,
        });
        Ok(ControlledBuildOutput {
            artifact,
            build_log: format!("remote_receipt={receipt_json}\n"),
        })
    }
}

struct LocalArtifactCache {
    cache: kurbis_hoster::ci::ObjectArtifactCache,
}

#[async_trait]
impl ArtifactCache for LocalArtifactCache {
    async fn put(
        &self,
        digest: &str,
        object_key: &str,
        bytes: &[u8],
        expires_at: chrono::DateTime<Utc>,
    ) -> Result<kurbis_hoster::ci::BuildOutput, kurbis_hoster::ci::CiServiceError> {
        self.cache.put(digest, object_key, bytes, expires_at).await
    }

    async fn get(
        &self,
        digest: &str,
    ) -> Result<Option<Vec<u8>>, kurbis_hoster::ci::CiServiceError> {
        self.cache.get(digest).await
    }

    async fn delete_expired(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> Result<u64, kurbis_hoster::ci::CiServiceError> {
        self.cache.delete_expired(now).await
    }
}

#[derive(Default)]
struct RecordingCheckRuns {
    updates: Mutex<Vec<CiBuildStage>>,
}

#[async_trait]
impl CheckRunClient for RecordingCheckRuns {
    async fn create(
        &self,
        _repository: &str,
        _details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<kurbis_hoster::ci::CheckRunId, kurbis_hoster::ci::CheckRunError> {
        Ok(kurbis_hoster::ci::CheckRunId(5005))
    }

    async fn get_or_create(
        &self,
        repository: &str,
        details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<kurbis_hoster::ci::CheckRunId, kurbis_hoster::ci::CheckRunError> {
        self.create(repository, details).await
    }

    async fn update(
        &self,
        _repository: &str,
        _id: kurbis_hoster::ci::CheckRunId,
        details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<(), kurbis_hoster::ci::CheckRunError> {
        self.updates
            .lock()
            .expect("check run fixture lock")
            .push(details.stage);
        Ok(())
    }
}

#[derive(Clone)]
struct RecordingRuntimeHost {
    runtimes: CiPreviewRuntimeRepository,
    fail_start: Arc<std::sync::atomic::AtomicBool>,
    starts: Arc<std::sync::atomic::AtomicUsize>,
    stops: Arc<std::sync::atomic::AtomicUsize>,
    deleted_worlds: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl CiPreviewRuntimeHost for RecordingRuntimeHost {
    async fn verify_sandbox(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn start(
        &self,
        placement: &CiRuntimePlacement,
        capability: &RuntimeCapability,
    ) -> anyhow::Result<()> {
        self.starts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail_start.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("remote runtime failed readiness")
        }
        if capability.preview_id() != placement.preview_id
            || capability.generation() != placement.generation
        {
            anyhow::bail!("runtime receipt generation does not match placement")
        }
        let marked = self
            .runtimes
            .mark_world_initialized(
                placement.preview_id,
                placement.worker_id,
                placement.generation,
                placement.lease_token,
            )
            .await?;
        if !marked {
            anyhow::bail!("runtime bootstrap callback was fenced")
        }
        let world_key = format!(
            "ci-previews/{}/worlds/generation-{}.tar.gz",
            placement.preview_id, placement.generation
        );
        let checksum = format!("{:064x}", placement.generation);
        self.runtimes
            .publish_world_snapshot(
                placement.preview_id,
                placement.worker_id,
                placement.generation,
                placement.lease_token,
                &world_key,
                64,
                &checksum,
            )
            .await?;
        Ok(())
    }

    async fn stop(
        &self,
        _placement: &CiRuntimePlacement,
        _retain_world: bool,
    ) -> anyhow::Result<()> {
        self.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn delete_world(&self, _placement: &CiRuntimePlacement) -> anyhow::Result<()> {
        self.deleted_worlds
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

async fn disposable_pool() -> PgPool {
    let url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must point to disposable PostgreSQL for milestone 5 acceptance");
    let options = url
        .parse::<PgConnectOptions>()
        .expect("TEST_DATABASE_URL must be a valid PostgreSQL URL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone().database("postgres"))
        .await
        .expect("connect to PostgreSQL admin database");
    let database = format!("kurbis_m5_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&admin)
        .await
        .expect("create disposable milestone 5 database");
    admin.close().await;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect_with(options.database(&database))
        .await
        .expect("connect to disposable milestone 5 database");
    kurbis_hoster::db::migrate(&pool)
        .await
        .expect("apply canonical schema to milestone 5 database");
    // Keep the database name in a connection-local setting so cleanup can be
    // performed after the pool is closed without a second helper parameter.
    sqlx::query("SELECT set_config('kurbis.m5.database', $1, false)")
        .bind(database)
        .execute(&pool)
        .await
        .expect("record disposable database name");
    pool
}

async fn close_disposable_pool(pool: PgPool) {
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .expect("read disposable database name");
    pool.close().await;
    let url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must remain set while cleaning acceptance database");
    let options = url
        .parse::<PgConnectOptions>()
        .expect("valid PostgreSQL URL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.database("postgres"))
        .await
        .expect("reconnect to PostgreSQL admin database");
    sqlx::query(&format!("DROP DATABASE IF EXISTS {database}"))
        .execute(&admin)
        .await
        .expect("drop disposable milestone 5 database");
    admin.close().await;
}

async fn create_worker(pool: &PgPool) -> Uuid {
    let worker_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workers(
             id,name,address,signal_address,total_memory_bytes,total_disk_bytes,total_cpu_millis,
             heartbeat_at,draining,port_min,port_max,runtime_architecture,supports_ci_previews
         ) VALUES($1,$2,'198.51.100.10','10.0.0.10',4294967296,21474836480,4000,
                   now()+interval '5 minutes',false,32100,32199,$3,true)",
    )
    .bind(worker_id)
    .bind(format!("milestone5-worker-{}", Uuid::new_v4().simple()))
    .bind(TARGET_ARCHITECTURE)
    .execute(pool)
    .await
    .expect("insert CI preview worker fixture");
    worker_id
}

async fn query_build(pool: &PgPool, build_id: Uuid) -> (String, Option<String>, String, i32) {
    sqlx::query_as(
        "SELECT stage, artifact_digest, build_log, retry_count
         FROM ci_builds WHERE id=$1",
    )
    .bind(build_id)
    .fetch_one(pool)
    .await
    .expect("read build fixture")
}

fn pipeline(
    repository: CiPreviewRepository,
    source: Arc<PrObservationSource>,
    builder: Arc<RemoteReceiptBuilder>,
    artifacts: Arc<LocalArtifactCache>,
    check_runs: Arc<RecordingCheckRuns>,
) -> ControlledBuildPipeline<
    PrObservationSource,
    ReceiptPatcher,
    RemoteReceiptBuilder,
    LocalArtifactCache,
> {
    ControlledBuildPipeline::new(
        repository,
        source,
        Arc::new(ReceiptPatcher),
        builder,
        artifacts,
        SandboxLimits {
            memory_bytes: 2 * 1024 * 1024 * 1024,
            cpu_millis: 2000,
            pid_limit: 512,
            disk_bytes: 5 * 1024 * 1024 * 1024,
            process_timeout: Duration::from_secs(60),
        },
    )
    .with_root_domain(ROOT_DOMAIN)
    .with_check_runs(check_runs)
}

#[tokio::test]
async fn pr_to_remote_receipt_to_runtime_join_ttl_and_expiry_is_atomic() {
    let pool = disposable_pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let source = Arc::new(PrObservationSource::new());
    let watched = repository
        .register_repository(REPOSITORY, "master")
        .await
        .expect("register watched repository");
    let poller = CiPoller::new(repository.clone(), source.clone()).with_config(CiPollerConfig {
        root_domain: ROOT_DOMAIN.to_owned(),
        patch_fingerprint: PATCH_FINGERPRINT.to_owned(),
        toolchain_version: TOOLCHAIN.to_owned(),
        target_architecture: TARGET_ARCHITECTURE.to_owned(),
        public_status_base_url: None,
    });

    // 1. PR observed -> one durable queued build and two queued aliases.
    poller.poll_once(watched.id).await.expect("observe PR head");
    poller
        .poll_once(watched.id)
        .await
        .expect("repeat observation is idempotent");
    let key = BuildKey::new(
        REPOSITORY,
        COMMIT_SHA,
        PATCH_FINGERPRINT,
        TOOLCHAIN,
        TARGET_ARCHITECTURE,
    )
    .expect("valid immutable build key");
    let (build, _) = repository
        .ensure_build(&key)
        .await
        .expect("load observed build");
    let queued_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ci_builds WHERE repository=$1 AND commit_sha=$2")
            .bind(REPOSITORY)
            .bind(COMMIT_SHA)
            .fetch_one(&pool)
            .await
            .expect("count immutable build keys");
    assert_eq!(
        queued_count, 1,
        "repeat PR observations must coalesce by build key"
    );
    assert_eq!(build.stage, CiBuildStage::Queued);
    // Observation alone is metadata, not build work. A current-head demand
    // marks the exact immutable row requested; only then is it recoverable by
    // the durable queue after a poller restart.
    assert_eq!(
        repository
            .queued_pull_request_builds(REPOSITORY)
            .await
            .unwrap()
            .len(),
        0
    );
    assert!(repository.mark_build_requested(build.id).await.unwrap());
    assert_eq!(
        repository
            .queued_pull_request_builds(REPOSITORY)
            .await
            .unwrap()
            .len(),
        1
    );
    let pr_alias = CiHostname::for_ref(CiRefKind::PullRequest, "pr-5005", ROOT_DOMAIN).unwrap();
    let commit_alias = CiHostname::for_ref(CiRefKind::Commit, COMMIT_SHA, ROOT_DOMAIN).unwrap();
    assert_eq!(
        repository
            .lookup_preview(&pr_alias.value)
            .await
            .unwrap()
            .unwrap()
            .state,
        CiPreviewState::Queued
    );
    assert_eq!(
        repository
            .lookup_preview(&commit_alias.value)
            .await
            .unwrap()
            .unwrap()
            .state,
        CiPreviewState::Queued
    );

    // 2. Two coordinator calls race for the same queued work. PostgreSQL's
    // build lease allows one remote compilation and the other observes the
    // coalesced result rather than producing a second artifact.
    let builder = Arc::new(RemoteReceiptBuilder::default());
    let check_runs = Arc::new(RecordingCheckRuns::default());
    let object_root = tempfile::tempdir().expect("artifact object-store tempdir");
    let store: Arc<dyn object_store::ObjectStore> = Arc::new(
        object_store::local::LocalFileSystem::new_with_prefix(object_root.path())
            .expect("local object store"),
    );
    let artifacts = Arc::new(LocalArtifactCache {
        cache: kurbis_hoster::ci::ObjectArtifactCache::new(pool.clone(), store),
    });
    let build_pipeline = pipeline(
        repository.clone(),
        source.clone(),
        builder.clone(),
        artifacts.clone(),
        check_runs.clone(),
    );
    let request = BuildRequest {
        key: key.clone(),
        source_repository: Some("contributor/Pumpkin".to_owned()),
        source_archive: Vec::new(),
    };
    let (left, right) = tokio::join!(
        build_pipeline.run(request.clone()),
        build_pipeline.run(request.clone()),
    );
    left.expect("first coalesced build succeeds");
    right.expect("second coalesced request is safely observable");
    let (stage, artifact_digest, build_log, _) = query_build(&pool, build.id).await;
    assert_eq!(stage, "ready");
    let artifact_digest = artifact_digest.expect("remote receipt must publish a digest");
    assert_eq!(
        builder
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let receipts = builder.receipts.lock().unwrap().clone();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].source_commit, COMMIT_SHA);
    assert_eq!(receipts[0].runtime_revision, "2026.10.11");
    assert_eq!(receipts[0].artifact_digest, artifact_digest);
    assert!(build_log.contains("remote_build_receipt"));
    assert!(build_log.contains("2026.10.11"));
    assert!(build_log.contains(&artifact_digest));
    assert_eq!(
        source.fetches.lock().unwrap().as_slice(),
        &[("contributor/Pumpkin".to_owned(), COMMIT_SHA.to_owned())]
    );
    let artifact = artifacts
        .get(&artifact_digest)
        .await
        .expect("read artifact object")
        .expect("published artifact exists");
    assert_eq!(
        blake3::hash(&artifact).to_hex().to_string(),
        artifact_digest
    );
    repository
        .advance_stable_aliases_for_build(build.id, ROOT_DOMAIN)
        .await
        .expect("advance stable PR alias after ready artifact");
    assert_eq!(
        repository
            .lookup_preview(&pr_alias.value)
            .await
            .unwrap()
            .unwrap()
            .state,
        CiPreviewState::Ready
    );

    // 3. Preview join -> runtime generation/capability -> durable bootstrap
    // receipt -> running alias. The fixture host performs the same bootstrap
    // checkpoint callback the worker uses before readiness is published.
    let worker_id = create_worker(&pool).await;
    let runtimes = CiPreviewRuntimeRepository::new(pool.clone());
    let host = Arc::new(RecordingRuntimeHost {
        runtimes: runtimes.clone(),
        fail_start: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        starts: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        stops: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        deleted_worlds: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    // Ensure the worker id was actually used; the runtime repository picks it
    // by architecture/capacity, not by an untrusted request field.
    let coordinator =
        CiPreviewRuntimeCoordinator::new(repository.clone(), runtimes.clone(), host.clone());
    let joined = coordinator
        .ensure_running(&pr_alias.value)
        .await
        .expect("ready preview joins through the runtime coordinator");
    assert_eq!(joined, CiPreviewState::Running);
    let placement = runtimes
        .current(
            repository
                .lookup_preview(&pr_alias.value)
                .await
                .unwrap()
                .unwrap()
                .id,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(placement.worker_id, worker_id);
    assert_eq!(placement.generation, 1);
    assert_eq!(placement.state, CiRuntimeState::Running);
    assert!(placement.active && placement.world_initialized);
    assert_eq!(host.starts.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        repository
            .lookup_preview(&pr_alias.value)
            .await
            .unwrap()
            .unwrap()
            .state
            == CiPreviewState::Running
    );

    // 4. A join/activity heartbeat slides the shared 72-hour TTL for alias,
    // immutable build, and artifact together.
    sqlx::query("UPDATE ci_preview_aliases SET expires_at=now()+interval '1 minute' WHERE id=$1")
        .bind(placement.preview_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        coordinator
            .touch_player_activity(placement.preview_id)
            .await
            .unwrap()
    );
    let ttl_hours: f64 = sqlx::query_scalar(
        "SELECT (extract(epoch FROM (expires_at-now()))/3600.0)::float8 FROM ci_preview_aliases WHERE id=$1",
    )
    .bind(placement.preview_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        ttl_hours > 71.9,
        "join activity must slide preview TTL, got {ttl_hours}"
    );

    // 5. Unsupported live state fails closed: a failed alias is reported as
    // failed and must not ask the worker to start an untrusted runtime.
    sqlx::query("UPDATE ci_preview_aliases SET state='failed' WHERE id=$1")
        .bind(placement.preview_id)
        .execute(&pool)
        .await
        .unwrap();
    let unsupported_state = coordinator.ensure_running(&pr_alias.value).await;
    assert!(
        unsupported_state.is_err(),
        "unsupported failed runtime state must fail closed"
    );
    assert_eq!(host.starts.load(std::sync::atomic::Ordering::SeqCst), 1);
    sqlx::query("UPDATE ci_preview_aliases SET state='running', expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(placement.preview_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE ci_builds SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(placement.build_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE ci_artifacts SET expires_at=now()-interval '1 second' WHERE digest=$1")
        .bind(&placement.artifact_digest)
        .execute(&pool)
        .await
        .unwrap();

    // 6. TTL expiry -> generation-fenced cleanup. No worker acknowledgement is
    // trusted after the alias is expired; the coordinator owns deletion and
    // leaves no route or active placement behind.
    repository
        .expire(Utc::now())
        .await
        .expect("mark expired metadata");
    let expired = coordinator
        .expire(Utc::now())
        .await
        .expect("cleanup expired runtime");
    assert_eq!(expired, 1);
    assert_eq!(
        host.deleted_worlds
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert!(
        runtimes
            .current(placement.preview_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .lookup_preview(&pr_alias.value)
            .await
            .unwrap()
            .is_none()
    );
    assert!(coordinator.ensure_running(&pr_alias.value).await.is_err());

    close_disposable_pool(pool).await;
}

#[tokio::test]
async fn failed_remote_build_and_failed_runtime_start_leave_no_ready_or_live_route() {
    let pool = disposable_pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let source = Arc::new(PrObservationSource::new());
    let watched = repository
        .register_repository(REPOSITORY, "master")
        .await
        .unwrap();
    let poller = CiPoller::new(repository.clone(), source.clone()).with_config(CiPollerConfig {
        root_domain: ROOT_DOMAIN.to_owned(),
        patch_fingerprint: PATCH_FINGERPRINT.to_owned(),
        toolchain_version: TOOLCHAIN.to_owned(),
        target_architecture: TARGET_ARCHITECTURE.to_owned(),
        public_status_base_url: None,
    });
    poller.poll_once(watched.id).await.unwrap();
    let key = BuildKey::new(
        REPOSITORY,
        COMMIT_SHA,
        PATCH_FINGERPRINT,
        TOOLCHAIN,
        TARGET_ARCHITECTURE,
    )
    .unwrap();
    let (build, _) = repository.ensure_build(&key).await.unwrap();
    let alias = CiHostname::for_ref(CiRefKind::PullRequest, "pr-5005", ROOT_DOMAIN).unwrap();
    let builder = Arc::new(RemoteReceiptBuilder::default());
    builder
        .fail_compile
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let checks = Arc::new(RecordingCheckRuns::default());
    let object_root = tempfile::tempdir().unwrap();
    let store: Arc<dyn object_store::ObjectStore> = Arc::new(
        object_store::local::LocalFileSystem::new_with_prefix(object_root.path()).unwrap(),
    );
    let artifacts = Arc::new(LocalArtifactCache {
        cache: kurbis_hoster::ci::ObjectArtifactCache::new(pool.clone(), store),
    });
    let build_pipeline = pipeline(
        repository.clone(),
        source,
        builder.clone(),
        artifacts,
        checks,
    );
    let request = BuildRequest {
        key,
        source_repository: Some("contributor/Pumpkin".to_owned()),
        source_archive: Vec::new(),
    };
    assert!(build_pipeline.run(request).await.is_err());
    let (stage, digest, _log, retry_count) = query_build(&pool, build.id).await;
    assert_eq!(stage, "failed");
    assert!(digest.is_none());
    assert_eq!(retry_count, 1);
    let preview = repository
        .lookup_preview(&alias.value)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(preview.state, CiPreviewState::Failed);
    assert_eq!(
        repository
            .queued_pull_request_builds(REPOSITORY)
            .await
            .unwrap()
            .len(),
        0
    );

    // A ready artifact with a runtime readiness failure must roll back to ready
    // (not running/booting) and release the worker's active reservation.
    builder
        .fail_compile
        .store(false, std::sync::atomic::Ordering::SeqCst);
    sqlx::query("UPDATE ci_builds SET retry_at=now()-interval '1 second' WHERE id=$1")
        .bind(build.id)
        .execute(&pool)
        .await
        .unwrap();
    let request = BuildRequest {
        key: BuildKey::new(
            REPOSITORY,
            COMMIT_SHA,
            PATCH_FINGERPRINT,
            TOOLCHAIN,
            TARGET_ARCHITECTURE,
        )
        .unwrap(),
        source_repository: Some("contributor/Pumpkin".to_owned()),
        source_archive: Vec::new(),
    };
    build_pipeline.run(request).await.unwrap();
    repository
        .advance_stable_aliases_for_build(build.id, ROOT_DOMAIN)
        .await
        .unwrap();
    let worker_id = create_worker(&pool).await;
    let runtimes = CiPreviewRuntimeRepository::new(pool.clone());
    let fail_start = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let host = Arc::new(RecordingRuntimeHost {
        runtimes: runtimes.clone(),
        fail_start: fail_start.clone(),
        starts: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        stops: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        deleted_worlds: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let coordinator =
        CiPreviewRuntimeCoordinator::new(repository.clone(), runtimes.clone(), host.clone());
    assert!(coordinator.ensure_running(&alias.value).await.is_err());
    let persisted = repository
        .lookup_preview(&alias.value)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.state, CiPreviewState::Ready);
    assert_eq!(host.starts.load(std::sync::atomic::Ordering::SeqCst), 1);
    let stopped = runtimes.current(persisted.id).await.unwrap().unwrap();
    assert_eq!(stopped.state, CiRuntimeState::Stopped);
    assert!(!stopped.active);
    let reserved: i64 = sqlx::query_scalar("SELECT reserved_memory_bytes FROM workers WHERE id=$1")
        .bind(worker_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        reserved, 0,
        "failed runtime readiness must release active capacity"
    );
    close_disposable_pool(pool).await;
}

struct BadDigestCache;

#[async_trait]
impl ArtifactCache for BadDigestCache {
    async fn put(
        &self,
        _digest: &str,
        object_key: &str,
        bytes: &[u8],
        _expires_at: chrono::DateTime<Utc>,
    ) -> Result<kurbis_hoster::ci::BuildOutput, kurbis_hoster::ci::CiServiceError> {
        Ok(kurbis_hoster::ci::BuildOutput {
            artifact_digest: "not-the-content-digest".to_owned(),
            object_key: object_key.to_owned(),
            size_bytes: bytes.len() as i64,
        })
    }

    async fn get(
        &self,
        _digest: &str,
    ) -> Result<Option<Vec<u8>>, kurbis_hoster::ci::CiServiceError> {
        Ok(None)
    }

    async fn delete_expired(
        &self,
        _now: chrono::DateTime<Utc>,
    ) -> Result<u64, kurbis_hoster::ci::CiServiceError> {
        Ok(0)
    }
}

#[tokio::test]
async fn artifact_receipt_digest_mismatch_fails_closed_before_ready_alias() {
    let pool = disposable_pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let source = Arc::new(PrObservationSource::new());
    let watched = repository
        .register_repository(REPOSITORY, "master")
        .await
        .unwrap();
    let poller = CiPoller::new(repository.clone(), source.clone()).with_config(CiPollerConfig {
        root_domain: ROOT_DOMAIN.to_owned(),
        patch_fingerprint: PATCH_FINGERPRINT.to_owned(),
        toolchain_version: TOOLCHAIN.to_owned(),
        target_architecture: TARGET_ARCHITECTURE.to_owned(),
        public_status_base_url: None,
    });
    poller.poll_once(watched.id).await.unwrap();
    let key = BuildKey::new(
        REPOSITORY,
        COMMIT_SHA,
        PATCH_FINGERPRINT,
        TOOLCHAIN,
        TARGET_ARCHITECTURE,
    )
    .unwrap();
    let (build, _) = repository.ensure_build(&key).await.unwrap();
    let alias = CiHostname::for_ref(CiRefKind::PullRequest, "pr-5005", ROOT_DOMAIN).unwrap();
    let checks = Arc::new(RecordingCheckRuns::default());
    let builder = Arc::new(RemoteReceiptBuilder::default());
    let build_pipeline = ControlledBuildPipeline::new(
        repository.clone(),
        source,
        Arc::new(ReceiptPatcher),
        builder,
        Arc::new(BadDigestCache),
        SandboxLimits {
            memory_bytes: 1,
            cpu_millis: 1,
            pid_limit: 1,
            disk_bytes: 1,
            process_timeout: Duration::from_secs(1),
        },
    )
    .with_root_domain(ROOT_DOMAIN)
    .with_check_runs(checks);
    let request = BuildRequest {
        key,
        source_repository: Some("contributor/Pumpkin".to_owned()),
        source_archive: b"source".to_vec(),
    };
    let error = build_pipeline
        .run(request)
        .await
        .expect_err("mismatched remote artifact must be rejected");
    assert!(error.to_string().contains("mismatched digest"));
    let (stage, digest, _log, _retry_count) = query_build(&pool, build.id).await;
    assert_eq!(stage, "failed");
    assert!(digest.is_none());
    let failure_class: Option<String> =
        sqlx::query_scalar("SELECT failure_class FROM ci_builds WHERE id=$1")
            .bind(build.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(failure_class.as_deref(), Some("permanent"));
    assert_eq!(
        repository
            .lookup_preview(&alias.value)
            .await
            .unwrap()
            .unwrap()
            .state,
        CiPreviewState::Failed
    );
    close_disposable_pool(pool).await;
}
