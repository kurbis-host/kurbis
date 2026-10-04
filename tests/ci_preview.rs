use async_trait::async_trait;
use chrono::Utc;
use kurbis_hoster::ci::{
    BuildKey, CiBuildStage, CiHostname, CiPreviewRepository, CiRefKind, PollCheckpoint, PollResult,
    SourceControlClient, SourceRef, SourceRefKind,
};
use kurbis_hoster::{db, routing::RouteRepository};
use sqlx::PgPool;
use uuid::Uuid;

static CI_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone)]
struct FixtureSource {
    commit_sha: std::sync::Arc<std::sync::RwLock<String>>,
}
impl Default for FixtureSource {
    fn default() -> Self {
        Self {
            commit_sha: std::sync::Arc::new(std::sync::RwLock::new(
                "0123456789abcdef0123456789abcdef01234567".into(),
            )),
        }
    }
}
#[async_trait]
impl SourceControlClient for FixtureSource {
    async fn poll(
        &self,
        _repository: &str,
        _checkpoint: &PollCheckpoint,
    ) -> Result<PollResult, kurbis_hoster::ci::PollError> {
        Ok(PollResult {
            refs: vec![SourceRef {
                kind: SourceRefKind::Branch,
                name: "main".into(),
                number: None,
                commit_sha: self.commit_sha.read().unwrap().clone(),
                source_repository: Some("kurbis-host/Pumpkin".into()),
                closed: false,
                deleted: false,
                updated_at: Some(Utc::now()),
            }],
            checkpoint: PollCheckpoint {
                last_successful_poll: Some(Utc::now()),
                ..Default::default()
            },
            not_modified: false,
            retry_after: None,
            complete_kinds: vec![SourceRefKind::Branch],
        })
    }
    async fn fetch_commit(
        &self,
        _repository: &str,
        _commit_sha: &str,
    ) -> Result<Vec<u8>, kurbis_hoster::ci::PollError> {
        Ok(Vec::new())
    }
}

fn unique_ci_root_domain() -> String {
    format!("pumpkin-{}.kurbis.host", Uuid::new_v4().simple())
}

async fn pool() -> PgPool {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
    let pool = db::connect(&url).await.unwrap();
    db::migrate(&pool).await.unwrap();
    pool
}

#[tokio::test]
async fn ci_builds_deduplicate_and_stable_aliases_advance_only_when_ready() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let root_domain = unique_ci_root_domain();
    let name = format!("kurbis-host/Pumpkin-{}", Uuid::new_v4());
    let watched = repository.register_repository(&name, "main").await.unwrap();
    repository
        .poll_repository(watched.id, &FixtureSource::default())
        .await
        .unwrap();
    let source_repository: String = sqlx::query_scalar(
        "SELECT source_repository FROM ci_refs WHERE repository_id = $1 AND name = 'main'",
    )
    .bind(watched.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(source_repository, "kurbis-host/Pumpkin");
    let key = BuildKey::new(
        &name,
        "0123456789abcdef0123456789abcdef01234567",
        "patch-v1",
        "rust-1.90",
        "aarch64-apple-darwin",
    )
    .unwrap();
    let (build, _) = repository.ensure_build(&key).await.unwrap();
    let (same_build, _) = repository.ensure_build(&key).await.unwrap();
    assert_eq!(build.id, same_build.id);
    let commit_alias =
        CiHostname::for_ref(CiRefKind::Commit, &key.commit_sha, &root_domain).unwrap();
    let preview = repository
        .ensure_preview(
            &name,
            &key.commit_sha,
            CiRefKind::Commit,
            None,
            &commit_alias,
            build.id,
        )
        .await
        .unwrap();
    assert_eq!(preview.state, kurbis_hoster::ci::CiPreviewState::Queued);
    let routes = RouteRepository::new(pool.clone(), "kurbis.host");
    let routed = routes
        .resolve_ci_hostname(&commit_alias.value, &root_domain)
        .await
        .unwrap()
        .expect("CI commit alias resolves independently from account servers");
    assert_eq!(routed.preview_id, preview.id);
    assert!(
        routes
            .ci_subdomain_for_hostname(
                &format!("nested.commit-pumpkin.{root_domain}"),
                &root_domain
            )
            .is_none()
    );
    let branch_alias = CiHostname::for_ref(CiRefKind::Branch, "main", &root_domain).unwrap();
    let branch = repository
        .ensure_preview(
            &name,
            &key.commit_sha,
            CiRefKind::Branch,
            Some("main"),
            &branch_alias,
            build.id,
        )
        .await
        .unwrap();
    assert_eq!(branch.state, kurbis_hoster::ci::CiPreviewState::Lobby);
    assert_eq!(
        repository
            .lookup_preview(&branch_alias.value)
            .await
            .unwrap()
            .unwrap()
            .state,
        kurbis_hoster::ci::CiPreviewState::Lobby
    );
    let queued = repository
        .ensure_lazy_branch_build(branch.id)
        .await
        .unwrap();
    assert_eq!(queued.state, kurbis_hoster::ci::CiPreviewState::Queued);
    assert_eq!(
        repository
            .lookup_preview(&branch_alias.value)
            .await
            .unwrap()
            .unwrap()
            .state,
        kurbis_hoster::ci::CiPreviewState::Queued
    );
    for stage in [
        CiBuildStage::Fetching,
        CiBuildStage::Patching,
        CiBuildStage::Compiling,
        CiBuildStage::Packaging,
        CiBuildStage::Ready,
    ] {
        repository
            .transition_build(build.id, stage, None)
            .await
            .unwrap();
    }
    repository
        .attach_artifact(build.id, "sha256:test", "ci/test.tar", 42)
        .await
        .unwrap();
    let newer_sha = "fedcba9876543210fedcba9876543210fedcba98";
    sqlx::query("UPDATE ci_refs SET commit_sha = $3 WHERE repository_id = $1 AND kind = 'branch' AND name = $2")
        .bind(watched.id).bind("main").bind(newer_sha).execute(&pool).await.unwrap();
    assert!(
        repository
            .advance_stable_alias(&branch_alias.value, build.id, CiRefKind::Branch, "main")
            .await
            .unwrap()
            .is_none(),
        "an older build must not advance an alias after the branch head changed"
    );
    sqlx::query("UPDATE ci_refs SET commit_sha = $3 WHERE repository_id = $1 AND kind = 'branch' AND name = $2")
        .bind(watched.id).bind("main").bind(&key.commit_sha).execute(&pool).await.unwrap();
    let advanced = repository
        .advance_stable_alias(&branch_alias.value, build.id, CiRefKind::Branch, "main")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(advanced.state, kurbis_hoster::ci::CiPreviewState::Ready);
    let running = repository.mark_preview_running(advanced.id).await.unwrap();
    assert_eq!(running.state, kurbis_hoster::ci::CiPreviewState::Running);
    repository.touch_preview(running.id).await.unwrap();
    let ttl_hours: f64 = sqlx::query_scalar("SELECT (extract(epoch FROM (expires_at - now())) / 3600)::float8 FROM ci_preview_aliases WHERE id = $1")
        .bind(running.id).fetch_one(&pool).await.unwrap();
    assert!(
        ttl_hours >= 71.9,
        "use should slide preview TTL to about 72h, got {ttl_hours}"
    );
    sqlx::query(
        "UPDATE ci_preview_aliases SET expires_at = now() - interval '1 second' WHERE id = $1",
    )
    .bind(running.id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE ci_builds SET expires_at = now() - interval '1 second' WHERE id = $1")
        .bind(build.id)
        .execute(&pool)
        .await
        .unwrap();
    repository.expire(Utc::now()).await.unwrap();
    let expired_state: Option<String> =
        sqlx::query_scalar("SELECT state FROM ci_preview_aliases WHERE id = $1")
            .bind(running.id)
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert!(expired_state.is_none() || expired_state.as_deref() == Some("expired"));
    let build_stage: Option<String> =
        sqlx::query_scalar("SELECT stage FROM ci_builds WHERE id = $1")
            .bind(build.id)
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert!(build_stage.is_none() || build_stage.as_deref() == Some("expired"));
    assert!(
        repository
            .lookup_preview(&branch_alias.value)
            .await
            .unwrap()
            .is_none()
    );
    pool.close().await;
}

#[tokio::test]
async fn first_branch_join_uses_latest_observed_head_after_lobby_was_created() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("kurbis-host/Pumpkin-{}", Uuid::new_v4());
    let watched = repository
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let source = std::sync::Arc::new(FixtureSource::default());
    let root_domain = unique_ci_root_domain();
    let poller = kurbis_hoster::ci::CiPoller::new(repository.clone(), source.clone()).with_config(
        kurbis_hoster::ci::CiPollerConfig {
            root_domain: root_domain.clone().into(),
            patch_fingerprint: "patch-v1".into(),
            toolchain_version: "rust-1.98@sha256:test".into(),
            target_architecture: "x86_64-linux".into(),
        },
    );
    poller.poll_once(watched.id).await.unwrap();
    let old_sha = "0123456789abcdef0123456789abcdef01234567";
    let new_sha = "fedcba9876543210fedcba9876543210fedcba98";
    let old_key = BuildKey::new(
        &repository_name,
        old_sha,
        "patch-v1",
        "rust-1.98@sha256:test",
        "x86_64-linux",
    )
    .unwrap();
    let (old_build, _) = repository.ensure_build(&old_key).await.unwrap();
    let branch_alias = CiHostname::for_ref(CiRefKind::Branch, "main", &root_domain).unwrap();
    let lobby = repository
        .ensure_preview(
            &repository_name,
            old_sha,
            CiRefKind::Branch,
            Some("main"),
            &branch_alias,
            old_build.id,
        )
        .await
        .unwrap();
    assert_eq!(lobby.state, kurbis_hoster::ci::CiPreviewState::Lobby);

    *source.commit_sha.write().unwrap() = new_sha.into();
    poller.poll_once(watched.id).await.unwrap();
    let new_key = BuildKey::new(
        &repository_name,
        new_sha,
        "patch-v1",
        "rust-1.98@sha256:test",
        "x86_64-linux",
    )
    .unwrap();
    let (observed_head_build, inserted) = repository.ensure_build(&new_key).await.unwrap();
    assert!(
        !inserted,
        "polling the new head already inserted its build record"
    );

    let (admitted, request, candidate_build_id) = repository
        .prepare_lazy_branch_admission(lobby.id, &root_domain)
        .await
        .unwrap();
    assert_eq!(admitted.commit_sha, new_sha);
    assert_eq!(admitted.state, kurbis_hoster::ci::CiPreviewState::Queued);
    assert_eq!(request.unwrap().key.commit_sha, new_sha);
    let (candidate_stage, candidate_sha): (String, String) =
        sqlx::query_as("SELECT stage, commit_sha FROM ci_builds WHERE id = $1")
            .bind(candidate_build_id.unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(candidate_build_id, Some(observed_head_build.id));
    assert_eq!(candidate_stage, "queued");
    assert_eq!(candidate_sha, new_sha);
    assert_eq!(
        repository
            .lookup_preview(&branch_alias.value)
            .await
            .unwrap()
            .unwrap()
            .commit_sha,
        new_sha
    );
    pool.close().await;
}

#[tokio::test]
async fn ci_runtime_assignment_requires_worker_matching_artifact_architecture() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{CiPreviewRuntimeRepository, RuntimeLease};

    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let root_domain = unique_ci_root_domain();
    let repository_name = format!("kurbis-host/Pumpkin-{}", Uuid::new_v4());
    repository
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let key = BuildKey::new(
        &repository_name,
        "0123456789abcdef0123456789abcdef01234567",
        "patch-v1",
        "rust-1.98@sha256:test",
        "x86_64-linux",
    )
    .unwrap();
    let (build, _) = repository.ensure_build(&key).await.unwrap();
    for stage in [
        CiBuildStage::Fetching,
        CiBuildStage::Patching,
        CiBuildStage::Compiling,
        CiBuildStage::Packaging,
    ] {
        repository
            .transition_build(build.id, stage, None)
            .await
            .unwrap();
    }
    repository
        .attach_artifact(build.id, "sha256:arch-test", "ci/arch-test.tar", 64)
        .await
        .unwrap();
    repository
        .transition_build(build.id, CiBuildStage::Ready, None)
        .await
        .unwrap();
    let alias = CiHostname::for_ref(CiRefKind::Commit, &key.commit_sha, &root_domain).unwrap();
    let preview = repository
        .ensure_preview(
            &repository_name,
            &key.commit_sha,
            CiRefKind::Commit,
            None,
            &alias,
            build.id,
        )
        .await
        .unwrap();

    let compatible_worker = Uuid::new_v4();
    let incompatible_worker = Uuid::new_v4();
    for (worker_id, architecture, address, port_min) in [
        (compatible_worker, "x86_64-linux", "198.51.100.41", 26000),
        (incompatible_worker, "aarch64-linux", "198.51.100.42", 27000),
    ] {
        sqlx::query(
            "INSERT INTO workers
                (id, name, address, signal_address, total_memory_bytes, total_disk_bytes,
                 total_cpu_millis, heartbeat_at, draining, port_min, port_max,
                 runtime_architecture, supports_ci_previews)
             VALUES ($1,$2,$3,'10.0.0.10',4294967296,21474836480,4000,now() + interval '5 minutes',FALSE,$4,$5,$6,$7)",
        )
        .bind(worker_id)
        .bind(format!("ci-arch-{architecture}-{}", Uuid::new_v4()))
        .bind(address)
        .bind(port_min)
        .bind(port_min + 20)
        .bind(architecture)
        .bind(worker_id == compatible_worker)
        .execute(&pool)
        .await
        .unwrap();
    }

    let eligible_workers: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workers WHERE NOT draining AND supports_ci_previews
           AND runtime_architecture = 'x86_64-linux'
           AND heartbeat_at > now() - interval '30 seconds'
           AND total_memory_bytes - reserved_memory_bytes >= 1073741824
           AND total_disk_bytes - reserved_disk_bytes >= 5368709120
           AND total_cpu_millis - reserved_cpu_millis >= 1000",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(eligible_workers >= 1);
    let exact_candidates: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workers worker
         CROSS JOIN LATERAL (
            SELECT candidate.port AS base_port
            FROM generate_series(worker.port_min, worker.port_max - 2) AS candidate(port)
            WHERE NOT EXISTS (SELECT 1 FROM port_leases lease WHERE lease.worker_id = worker.id
                              AND lease.port IN (candidate.port, candidate.port + 1, candidate.port + 2))
              AND NOT EXISTS (SELECT 1 FROM ci_port_leases lease WHERE lease.worker_id = worker.id
                              AND lease.port IN (candidate.port, candidate.port + 1, candidate.port + 2))
            ORDER BY candidate.port LIMIT 1
         ) candidates
         WHERE NOT worker.draining AND worker.signal_address <> ''
           AND worker.supports_ci_previews AND worker.runtime_architecture = $1
           AND worker.heartbeat_at > now() - interval '30 seconds'
           AND worker.total_memory_bytes - worker.reserved_memory_bytes >= $2
           AND worker.total_disk_bytes - worker.reserved_disk_bytes >= $3
           AND worker.total_cpu_millis - worker.reserved_cpu_millis >= $4",
    )
    .bind("x86_64-linux")
    .bind(1024_i64 * 1024 * 1024)
    .bind(5_i64 * 1024 * 1024 * 1024)
    .bind(1000_i64)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(exact_candidates >= 1);
    let runtime_repository = CiPreviewRuntimeRepository::new(pool.clone());
    let lease = runtime_repository
        .claim_start(preview.id, &[7u8; 32])
        .await
        .unwrap();
    let RuntimeLease::Claimed(placement) = lease else {
        panic!("matching Linux worker should claim the CI preview: {lease:?}");
    };
    assert_ne!(placement.worker_id, incompatible_worker);
    let selected_architecture: String =
        sqlx::query_scalar("SELECT runtime_architecture FROM workers WHERE id = $1")
            .bind(placement.worker_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(selected_architecture, "x86_64-linux");
    sqlx::query("DELETE FROM ci_runtime_placements WHERE preview_id = $1")
        .bind(preview.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers WHERE id = ANY($1)")
        .bind(vec![compatible_worker, incompatible_worker])
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

#[derive(Default)]
struct TestBuildComponents {
    compile_calls: std::sync::atomic::AtomicUsize,
}
#[async_trait]
impl kurbis_hoster::ci::PatchApplier for TestBuildComponents {
    async fn apply(
        &self,
        source_archive: Vec<u8>,
        fingerprint: &str,
    ) -> Result<Vec<u8>, kurbis_hoster::ci::BuildPipelineError> {
        let mut patched = source_archive;
        patched.extend_from_slice(fingerprint.as_bytes());
        Ok(patched)
    }
}
#[async_trait]
impl kurbis_hoster::ci::ControlledBuilder for TestBuildComponents {
    async fn verify_sandbox(
        &self,
        limits: kurbis_hoster::ci::SandboxLimits,
    ) -> Result<(), kurbis_hoster::ci::BuildPipelineError> {
        if limits.memory_bytes == 0
            || limits.cpu_millis == 0
            || limits.pid_limit == 0
            || limits.disk_bytes == 0
        {
            return Err(kurbis_hoster::ci::BuildPipelineError::Sandbox(
                "missing resource limit".into(),
            ));
        }
        Ok(())
    }
    async fn compile_and_package(
        &self,
        patched_source: Vec<u8>,
        _key: &BuildKey,
    ) -> Result<kurbis_hoster::ci::ControlledBuildOutput, kurbis_hoster::ci::BuildPipelineError>
    {
        self.compile_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut artifact = patched_source;
        artifact.extend_from_slice(b"verified-pumpkin-artifact");
        Ok(kurbis_hoster::ci::ControlledBuildOutput {
            artifact,
            build_log: "cargo build succeeded\n".into(),
        })
    }
}
struct TestArtifactCache;
#[async_trait]
impl kurbis_hoster::ci::ArtifactCache for TestArtifactCache {
    async fn put(
        &self,
        digest: &str,
        object_key: &str,
        bytes: &[u8],
        _expires_at: chrono::DateTime<Utc>,
    ) -> Result<kurbis_hoster::ci::BuildOutput, kurbis_hoster::ci::CiServiceError> {
        Ok(kurbis_hoster::ci::BuildOutput {
            artifact_digest: digest.to_owned(),
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

struct TestCheckRuns {
    id: i64,
    stages: std::sync::Mutex<Vec<CiBuildStage>>,
}
#[async_trait]
impl kurbis_hoster::ci::CheckRunClient for TestCheckRuns {
    async fn create(
        &self,
        _repository: &str,
        _details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<kurbis_hoster::ci::CheckRunId, kurbis_hoster::ci::CheckRunError> {
        Ok(kurbis_hoster::ci::CheckRunId(self.id))
    }
    async fn update(
        &self,
        _repository: &str,
        id: kurbis_hoster::ci::CheckRunId,
        details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<(), kurbis_hoster::ci::CheckRunError> {
        assert_eq!(id.0, self.id);
        assert_eq!(details.commit_sha.len(), 40);
        self.stages.lock().unwrap().push(details.stage);
        Ok(())
    }
}

#[tokio::test]
async fn controlled_build_pipeline_verifies_sandbox_caches_artifact_and_deduplicates() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{BuildRequest, ControlledBuildPipeline, SandboxLimits};
    let pool = pool().await;
    let repo = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("kurbis-host/Pumpkin-{}", Uuid::new_v4());
    let watched = repo
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let components = std::sync::Arc::new(TestBuildComponents::default());
    let check_run_id = (Uuid::new_v4().as_u128() & i64::MAX as u128) as i64;
    let check_runs = std::sync::Arc::new(TestCheckRuns {
        id: check_run_id,
        stages: std::sync::Mutex::new(Vec::new()),
    });
    let pipeline = ControlledBuildPipeline::new(
        repo.clone(),
        std::sync::Arc::new(FixtureSource::default()),
        components.clone(),
        components.clone(),
        std::sync::Arc::new(TestArtifactCache),
        SandboxLimits {
            memory_bytes: 2 * 1024 * 1024 * 1024,
            cpu_millis: 1000,
            pid_limit: 512,
            disk_bytes: 5 * 1024 * 1024 * 1024,
            process_timeout: std::time::Duration::from_secs(3600),
        },
    )
    .with_check_runs(check_runs.clone());
    let key = BuildKey::new(
        &repository_name,
        "0123456789abcdef0123456789abcdef01234567",
        "patch-v1",
        "native",
        "aarch64-macos",
    )
    .unwrap();
    let (queued_build, created) = repo.ensure_build(&key).await.unwrap();
    assert!(created);
    repo.record_check_run(
        queued_build.id,
        Some(check_run_id),
        None,
        CiBuildStage::Queued,
    )
    .await
    .unwrap();
    let request = BuildRequest {
        key: key.clone(),
        source_repository: None,
        source_archive: b"pinned-source-archive".to_vec(),
    };
    let built = pipeline.run(request.clone()).await.unwrap();
    assert_eq!(built.stage, CiBuildStage::Ready);
    assert!(built.artifact_digest.is_some());
    let cached = pipeline.run(request.clone()).await.unwrap();
    assert_eq!(cached.id, built.id);
    assert_eq!(
        *check_runs.stages.lock().unwrap(),
        vec![
            CiBuildStage::Fetching,
            CiBuildStage::Patching,
            CiBuildStage::Compiling,
            CiBuildStage::Packaging,
            CiBuildStage::Ready
        ]
    );
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert!(repo.repository_name(watched.id).await.is_ok());
    sqlx::query("UPDATE ci_builds SET expires_at = now() - interval '1 second' WHERE id = $1")
        .bind(built.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE ci_artifacts SET expires_at = now() - interval '1 second' WHERE digest = $1",
    )
    .bind(built.artifact_digest.as_deref().unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let rebuilt = pipeline.run(request).await.unwrap();
    assert_eq!(rebuilt.id, built.id);
    assert_eq!(rebuilt.stage, CiBuildStage::Ready);
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    pool.close().await;
}

#[tokio::test]
async fn object_artifact_cache_verifies_content_and_removes_expired_blobs() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{ArtifactCache, ObjectArtifactCache};
    use object_store::{ObjectStore, local::LocalFileSystem};
    let pool = pool().await;
    let directory = tempfile::tempdir().unwrap();
    let store: std::sync::Arc<dyn ObjectStore> =
        std::sync::Arc::new(LocalFileSystem::new_with_prefix(directory.path()).unwrap());
    let cache = ObjectArtifactCache::new(pool.clone(), store);
    let bytes = b"known verified pumpkin artifact";
    let digest = blake3::hash(bytes).to_hex().to_string();
    let key = format!("ci/test/{digest}.tar");
    let written = cache
        .put(
            &digest,
            &key,
            bytes,
            Utc::now() + chrono::Duration::hours(72),
        )
        .await
        .unwrap();
    assert_eq!(written.size_bytes, bytes.len() as i64);
    assert_eq!(
        cache.get(&digest).await.unwrap().as_deref(),
        Some(bytes.as_slice())
    );
    assert!(
        cache
            .put("wrong-digest", "ci/test/wrong.tar", bytes, Utc::now())
            .await
            .is_err()
    );
    sqlx::query(
        "UPDATE ci_artifacts SET expires_at = now() - interval '1 second' WHERE digest = $1",
    )
    .bind(&digest)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(cache.delete_expired(Utc::now()).await.unwrap(), 1);
    assert_eq!(cache.get(&digest).await.unwrap(), None);
    pool.close().await;
}

#[derive(Clone)]
struct PullRequestFixtureSource {
    fetches: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
}
#[async_trait]
impl SourceControlClient for PullRequestFixtureSource {
    async fn poll(
        &self,
        _repository: &str,
        _checkpoint: &PollCheckpoint,
    ) -> Result<PollResult, kurbis_hoster::ci::PollError> {
        Ok(PollResult {
            refs: vec![SourceRef {
                kind: SourceRefKind::PullRequest,
                name: "pr-314".into(),
                number: Some(314),
                commit_sha: "fedcba9876543210fedcba9876543210fedcba98".into(),
                source_repository: Some("contributor/Pumpkin".into()),
                closed: false,
                deleted: false,
                updated_at: Some(Utc::now()),
            }],
            checkpoint: PollCheckpoint {
                last_successful_poll: Some(Utc::now()),
                ..Default::default()
            },
            not_modified: false,
            retry_after: None,
            complete_kinds: vec![SourceRefKind::PullRequest],
        })
    }

    async fn fetch_commit(
        &self,
        repository: &str,
        commit_sha: &str,
    ) -> Result<Vec<u8>, kurbis_hoster::ci::PollError> {
        self.fetches
            .lock()
            .unwrap()
            .push((repository.to_owned(), commit_sha.to_owned()));
        Ok(b"exact-fork-head-source".to_vec())
    }
}

struct IdempotentCheckRuns {
    id: i64,
    create_count: std::sync::atomic::AtomicUsize,
    update_stages: std::sync::Mutex<Vec<CiBuildStage>>,
}
#[async_trait]
impl kurbis_hoster::ci::CheckRunClient for IdempotentCheckRuns {
    async fn create(
        &self,
        _repository: &str,
        _details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<kurbis_hoster::ci::CheckRunId, kurbis_hoster::ci::CheckRunError> {
        let sequence = self
            .create_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst) as i64;
        Ok(kurbis_hoster::ci::CheckRunId(self.id + sequence))
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
        id: kurbis_hoster::ci::CheckRunId,
        details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<(), kurbis_hoster::ci::CheckRunError> {
        assert!(id.0 >= self.id);
        assert!(details.build_log.len() <= 8 * 1024);
        assert!(
            details
                .preview_hostnames
                .iter()
                .any(|host| host.starts_with("commit-"))
        );
        self.update_stages.lock().unwrap().push(details.stage);
        Ok(())
    }
}

#[tokio::test]
async fn production_poll_orchestration_eagerly_builds_fork_pr_once_with_one_check_run() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{CiPoller, CiPollerConfig, ControlledBuildPipeline, SandboxLimits};

    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let root_domain = unique_ci_root_domain();
    let repository_name = format!("kurbis-host/Pumpkin-{}", Uuid::new_v4());
    let watched = repository
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let fetches = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let source = std::sync::Arc::new(PullRequestFixtureSource {
        fetches: fetches.clone(),
    });
    let components = std::sync::Arc::new(TestBuildComponents::default());
    let check_run_id = (Uuid::new_v4().as_u128() & i64::MAX as u128) as i64;
    let check_runs = std::sync::Arc::new(IdempotentCheckRuns {
        id: check_run_id,
        create_count: std::sync::atomic::AtomicUsize::new(0),
        update_stages: std::sync::Mutex::new(Vec::new()),
    });
    let key = BuildKey::new(
        &repository_name,
        "fedcba9876543210fedcba9876543210fedcba98",
        "patch-v1",
        "test-toolchain",
        "x86_64-linux",
    )
    .unwrap();
    let (seeded, _) = repository.ensure_build(&key).await.unwrap();
    repository
        .claim_check_run(seeded.id, Uuid::new_v4(), chrono::Duration::minutes(2))
        .await
        .unwrap();
    // The poller owns check-run creation in this flow; leave the build's check-run row empty.
    sqlx::query("DELETE FROM ci_check_runs WHERE build_id = $1")
        .bind(seeded.id)
        .execute(&pool)
        .await
        .unwrap();

    let builder = std::sync::Arc::new(
        ControlledBuildPipeline::new(
            repository.clone(),
            source.clone(),
            components.clone(),
            components.clone(),
            std::sync::Arc::new(TestArtifactCache),
            SandboxLimits {
                memory_bytes: 2 * 1024 * 1024 * 1024,
                cpu_millis: 1000,
                pid_limit: 512,
                disk_bytes: 5 * 1024 * 1024 * 1024,
                process_timeout: std::time::Duration::from_secs(3600),
            },
        )
        .with_check_runs(check_runs.clone()),
    );
    let poller = CiPoller::new(repository.clone(), source)
        .with_config(CiPollerConfig {
            root_domain: root_domain.clone(),
            patch_fingerprint: "patch-v1".into(),
            toolchain_version: "test-toolchain".into(),
            target_architecture: "x86_64-linux".into(),
        })
        .with_check_runs(check_runs.clone())
        .with_builder(builder.clone());

    poller.poll_once(watched.id).await.unwrap();
    poller.poll_once(watched.id).await.unwrap();

    assert_eq!(
        fetches.lock().unwrap().as_slice(),
        &[(
            "contributor/Pumpkin".to_owned(),
            "fedcba9876543210fedcba9876543210fedcba98".to_owned(),
        )],
        "the fork's immutable head is fetched once; the second poll reuses the successful build"
    );
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the database uniqueness key and claim lease deduplicate repeated PR polls"
    );
    assert_eq!(
        check_runs
            .create_count
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "exactly one build-only Check Run is created for the commit"
    );
    let stages = check_runs.update_stages.lock().unwrap().clone();
    assert!(stages.contains(&CiBuildStage::Fetching));
    assert!(stages.contains(&CiBuildStage::Patching));
    assert!(stages.contains(&CiBuildStage::Compiling));
    assert!(stages.contains(&CiBuildStage::Packaging));
    assert_eq!(stages.last(), Some(&CiBuildStage::Ready));
    let build = repository.ensure_build(&key).await.unwrap().0;
    assert_eq!(build.stage, CiBuildStage::Ready);
    assert!(
        build
            .build_log
            .contains("host patches applied successfully")
    );
    assert_eq!(
        repository.check_run_id(build.id).await.unwrap(),
        Some(check_run_id)
    );
    pool.close().await;
}

#[derive(Clone)]
struct MutableBranchSource {
    commit: std::sync::Arc<std::sync::RwLock<String>>,
    fetches: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}
#[async_trait]
impl SourceControlClient for MutableBranchSource {
    async fn poll(
        &self,
        repository: &str,
        checkpoint: &PollCheckpoint,
    ) -> Result<PollResult, kurbis_hoster::ci::PollError> {
        let commit_sha = self.commit.read().unwrap().clone();
        Ok(PollResult {
            refs: vec![SourceRef {
                kind: SourceRefKind::Branch,
                name: "main".into(),
                number: None,
                commit_sha,
                source_repository: Some(repository.to_owned()),
                closed: false,
                deleted: false,
                updated_at: Some(Utc::now()),
            }],
            checkpoint: PollCheckpoint {
                last_successful_poll: Some(Utc::now()),
                ..checkpoint.clone()
            },
            not_modified: false,
            retry_after: None,
            complete_kinds: vec![SourceRefKind::Branch],
        })
    }

    async fn fetch_commit(
        &self,
        _repository: &str,
        commit_sha: &str,
    ) -> Result<Vec<u8>, kurbis_hoster::ci::PollError> {
        self.fetches.lock().unwrap().push(commit_sha.to_owned());
        Ok(format!("exact-branch-source:{commit_sha}").into_bytes())
    }
}

#[tokio::test]
async fn admitted_branch_builds_new_heads_and_advances_only_after_old_runtime_stops() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{CiPoller, CiPollerConfig, ControlledBuildPipeline, SandboxLimits};

    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("kurbis-host/Pumpkin-{}", Uuid::new_v4());
    let watched = repository
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let root_domain = format!("pumpkin-{}.kurbis.host", Uuid::new_v4().simple());
    let first_sha = "0123456789abcdef0123456789abcdef01234567";
    let next_sha = "fedcba9876543210fedcba9876543210fedcba98";
    let source = std::sync::Arc::new(MutableBranchSource {
        commit: std::sync::Arc::new(std::sync::RwLock::new(first_sha.into())),
        fetches: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
    });
    let components = std::sync::Arc::new(TestBuildComponents::default());
    let check_run_id = (Uuid::new_v4().as_u128() & i64::MAX as u128) as i64;
    let check_runs = std::sync::Arc::new(IdempotentCheckRuns {
        id: check_run_id,
        create_count: std::sync::atomic::AtomicUsize::new(0),
        update_stages: std::sync::Mutex::new(Vec::new()),
    });
    let builder = std::sync::Arc::new(
        ControlledBuildPipeline::new(
            repository.clone(),
            source.clone(),
            components.clone(),
            components.clone(),
            std::sync::Arc::new(TestArtifactCache),
            SandboxLimits {
                memory_bytes: 2 * 1024 * 1024 * 1024,
                cpu_millis: 1000,
                pid_limit: 512,
                disk_bytes: 5 * 1024 * 1024 * 1024,
                process_timeout: std::time::Duration::from_secs(3600),
            },
        )
        .with_check_runs(check_runs.clone())
        .with_root_domain(root_domain.clone()),
    );
    let poller = CiPoller::new(repository.clone(), source.clone())
        .with_config(CiPollerConfig {
            root_domain: root_domain.clone(),
            patch_fingerprint: "patch-v1".into(),
            toolchain_version: "test-toolchain".into(),
            target_architecture: "x86_64-linux".into(),
        })
        .with_check_runs(check_runs.clone())
        .with_builder(builder.clone());

    poller.poll_once(watched.id).await.unwrap();
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "branch polling alone remains lazy"
    );
    let branch_host = CiHostname::for_ref(CiRefKind::Branch, "main", &root_domain).unwrap();
    let lobby = repository
        .lookup_preview(&branch_host.value)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lobby.state, kurbis_hoster::ci::CiPreviewState::Lobby);
    let (_, first_request, _) = repository
        .prepare_lazy_branch_admission(lobby.id, &root_domain)
        .await
        .unwrap();
    let first_build = builder.run(first_request.unwrap()).await.unwrap();
    assert_eq!(first_build.stage, CiBuildStage::Ready);

    sqlx::query("UPDATE ci_preview_aliases SET state = 'running' WHERE hostname = $1")
        .bind(&branch_host.value)
        .execute(&pool)
        .await
        .unwrap();
    *source.commit.write().unwrap() = next_sha.into();
    poller.poll_once(watched.id).await.unwrap();

    let next_key = BuildKey::new(
        &repository_name,
        next_sha,
        "patch-v1",
        "test-toolchain",
        "x86_64-linux",
    )
    .unwrap();
    let next_build = repository.ensure_build(&next_key).await.unwrap().0;
    assert_eq!(next_build.stage, CiBuildStage::Ready);
    assert_eq!(
        repository.check_run_id(next_build.id).await.unwrap(),
        Some(check_run_id + 1)
    );
    assert_eq!(
        repository
            .lookup_preview(&branch_host.value)
            .await
            .unwrap()
            .unwrap()
            .commit_sha,
        first_sha,
        "the stable alias keeps serving the prior successful runtime until it stops"
    );
    assert!(
        check_runs
            .update_stages
            .lock()
            .unwrap()
            .contains(&CiBuildStage::Ready)
    );

    sqlx::query("UPDATE ci_preview_aliases SET state = 'ready' WHERE hostname = $1")
        .bind(&branch_host.value)
        .execute(&pool)
        .await
        .unwrap();
    poller.poll_once(watched.id).await.unwrap();
    assert_eq!(
        repository
            .lookup_preview(&branch_host.value)
            .await
            .unwrap()
            .unwrap()
            .commit_sha,
        next_sha,
        "a subsequent poll advances the stable alias after the previous runtime is idle"
    );
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        2,
        "the admitted branch compiles its new immutable head once"
    );
    assert_eq!(
        source.fetches.lock().unwrap().as_slice(),
        &[first_sha.to_owned(), next_sha.to_owned()]
    );
    pool.close().await;
}

#[tokio::test]
async fn expired_check_run_is_retained_until_terminal_update_is_reconciled() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("kurbis-host/Pumpkin-{}", Uuid::new_v4());
    repository
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let key = BuildKey::new(
        repository_name,
        "fedcba9876543210fedcba9876543210fedcba98",
        "patch-v1",
        "rust-1.98@sha256:test",
        "x86_64-linux",
    )
    .unwrap();
    let (build, _) = repository.ensure_build(&key).await.unwrap();
    let check_run_id = i64::try_from(Uuid::new_v4().as_u128() & i64::MAX as u128).unwrap();
    repository
        .record_check_run(build.id, Some(check_run_id), None, CiBuildStage::Queued)
        .await
        .unwrap();
    repository
        .transition_build(build.id, CiBuildStage::Failed, Some("fixture failure"))
        .await
        .unwrap();
    repository
        .record_check_run(build.id, Some(check_run_id), None, CiBuildStage::Failed)
        .await
        .unwrap();
    sqlx::query("UPDATE ci_builds SET expires_at = now() - interval '1 second' WHERE id = $1")
        .bind(build.id)
        .execute(&pool)
        .await
        .unwrap();

    repository.expire(Utc::now()).await.unwrap();
    let retained_stage: String = sqlx::query_scalar("SELECT stage FROM ci_builds WHERE id = $1")
        .bind(build.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(retained_stage, "expired");
    let pending = repository
        .check_run_reconciliation_candidates(8)
        .await
        .unwrap();
    let pending = pending
        .into_iter()
        .find(|candidate| candidate.build_id == build.id)
        .expect("expired build awaits a terminal Check Run update");
    assert_eq!(pending.stage, CiBuildStage::Expired);
    assert_eq!(pending.check_run_id, check_run_id);

    sqlx::query(
        "UPDATE ci_check_runs SET updated_at = now() - interval '8 days' WHERE build_id = $1",
    )
    .bind(build.id)
    .execute(&pool)
    .await
    .unwrap();
    repository.expire(Utc::now()).await.unwrap();
    let deleted =
        sqlx::query_scalar::<_, bool>("SELECT NOT EXISTS(SELECT 1 FROM ci_builds WHERE id = $1)")
            .bind(build.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(deleted);
    pool.close().await;
}

#[tokio::test]
async fn expired_build_lease_fences_every_late_stage_log_and_artifact_write() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("kurbis-host/Pumpkin-{}", Uuid::new_v4());
    repository
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let key = BuildKey::new(
        repository_name,
        "fedcba9876543210fedcba9876543210fedcba98",
        "patch-v1",
        "test-toolchain",
        "x86_64-linux",
    )
    .unwrap();
    let (build, _) = repository.ensure_build(&key).await.unwrap();
    let lease = Uuid::new_v4();
    assert!(
        repository
            .claim_build(build.id, lease, chrono::Duration::minutes(5))
            .await
            .unwrap()
    );
    repository
        .transition_claimed_build(build.id, lease, CiBuildStage::Fetching)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE ci_builds SET lease_expires_at = now() - interval '1 second' WHERE id = $1",
    )
    .bind(build.id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(!repository.owns_build_lease(build.id, lease).await.unwrap());
    assert!(
        repository
            .transition_claimed_build(build.id, lease, CiBuildStage::Patching)
            .await
            .is_err()
    );
    assert!(
        repository
            .append_build_log_claimed(build.id, lease, CiBuildStage::Fetching, "stale output")
            .await
            .is_err()
    );
    assert!(
        repository
            .attach_claimed_artifact(build.id, lease, "deadbeef", "ci/stale.tar", 12)
            .await
            .is_err()
    );
    assert!(
        !repository
            .fail_claimed_build(build.id, lease, CiBuildStage::Fetching, "stale failure")
            .await
            .unwrap()
    );
    let build = repository.ensure_build(&key).await.unwrap().0;
    assert_eq!(build.stage, CiBuildStage::Fetching);
    assert!(!build.build_log.contains("stale output"));
    assert!(build.artifact_digest.is_none());
    pool.close().await;
}
