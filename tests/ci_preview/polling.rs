use super::*;

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
        details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<kurbis_hoster::ci::CheckRunId, kurbis_hoster::ci::CheckRunError> {
        if let Some(url) = details.details_url.as_deref() {
            assert!(url.starts_with("https://panel.example.test/ci/previews/commit-"));
        }
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
        if let Some(url) = details.details_url.as_deref() {
            assert!(url.starts_with("https://panel.example.test/ci/previews/commit-"));
        }
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
            public_status_base_url: Some("https://panel.example.test/ci/previews".into()),
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
            public_status_base_url: Some("https://panel.example.test/ci/previews".into()),
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
async fn deleted_branch_expires_its_preview_and_claims_world_cleanup_but_not_closed_prs() {
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
            root_domain: root_domain.clone(),
            patch_fingerprint: "patch-delete-v1".into(),
            toolchain_version: "test-toolchain".into(),
            target_architecture: "x86_64-linux".into(),
            public_status_base_url: Some("https://panel.example.test/ci/previews".into()),
        },
    );
    poller.poll_once(watched.id).await.unwrap();
    let sha = source.commit_sha.read().unwrap().clone();
    let key = BuildKey::new(
        &repository_name,
        &sha,
        "patch-delete-v1",
        "test-toolchain",
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
        .attach_artifact(
            build.id,
            "sha256:deleted-branch",
            "ci/deleted-branch.tar",
            64,
        )
        .await
        .unwrap();
    repository
        .transition_build(build.id, CiBuildStage::Ready, None)
        .await
        .unwrap();
    let branch_hostname = CiHostname::for_ref(CiRefKind::Branch, "main", &root_domain).unwrap();
    let branch = repository
        .ensure_preview(
            &repository_name,
            &sha,
            CiRefKind::Branch,
            Some("main"),
            &branch_hostname,
            build.id,
        )
        .await
        .unwrap();

    let pr_hostname = CiHostname::for_ref(CiRefKind::PullRequest, "42", &root_domain).unwrap();
    let pr = repository
        .ensure_preview(
            &repository_name,
            &sha,
            CiRefKind::PullRequest,
            Some("42"),
            &pr_hostname,
            build.id,
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO ci_refs(repository_id, kind, name, number, commit_sha, source_repository, closed, deleted)
         VALUES($1, 'pull_request', '42', 42, $2, $3, true, false)",
    )
    .bind(watched.id)
    .bind(&sha)
    .bind(&repository_name)
    .execute(&pool)
    .await
    .unwrap();

    let worker_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workers(id,name,address,signal_address,total_memory_bytes,total_disk_bytes,total_cpu_millis,
             heartbeat_at,draining,port_min,port_max,runtime_architecture,supports_ci_previews)
         VALUES($1,$2,'198.51.100.81','10.0.0.81',4294967296,21474836480,4000,
             now()+interval '5 minutes',false,28000,28020,'x86_64-linux',true)",
    )
    .bind(worker_id)
    .bind(format!("ci-delete-worker-{}", Uuid::new_v4()))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE ci_preview_aliases SET state='running', runtime_generation=1 WHERE id=$1")
        .bind(branch.id)
        .execute(&pool)
        .await
        .unwrap();
    let lease_token = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO ci_runtime_placements
         (preview_id,worker_id,generation,lease_token,java_port,bedrock_port,signal_port,state,active,capability_hash)
         VALUES($1,$2,1,$3,28000,28001,28002,'running',true,$4)",
    )
    .bind(branch.id)
    .bind(worker_id)
    .bind(lease_token)
    .bind([3u8; 32].as_slice())
    .execute(&pool)
    .await
    .unwrap();

    source
        .include_branch
        .store(false, std::sync::atomic::Ordering::SeqCst);
    source
        .complete_branches
        .store(false, std::sync::atomic::Ordering::SeqCst);
    poller.poll_once(watched.id).await.unwrap();
    assert!(
        repository
            .lookup_preview(&branch_hostname.value)
            .await
            .unwrap()
            .is_some(),
        "a truncated branch enumeration must not expire unseen branch aliases"
    );

    source
        .complete_branches
        .store(true, std::sync::atomic::Ordering::SeqCst);
    poller.poll_once(watched.id).await.unwrap();
    assert!(
        repository
            .lookup_preview(&branch_hostname.value)
            .await
            .unwrap()
            .is_none()
    );
    let pr_state = repository
        .lookup_preview_by_id(pr.id)
        .await
        .unwrap()
        .expect("PR close remains available by explicit design");
    assert_ne!(pr_state.state, kurbis_hoster::ci::CiPreviewState::Expired);
    let cleanup = kurbis_hoster::ci::CiPreviewRuntimeRepository::new(pool.clone())
        .claim_expired(Utc::now(), 16)
        .await
        .unwrap();
    assert_eq!(cleanup.len(), 1);
    assert_eq!(cleanup[0].placement.preview_id, branch.id);
    assert_eq!(cleanup[0].placement.generation, 1);
    let runtime_state: String =
        sqlx::query_scalar("SELECT state FROM ci_runtime_placements WHERE preview_id=$1")
            .bind(branch.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(runtime_state, "deleting");

    sqlx::query("DELETE FROM ci_runtime_placements WHERE preview_id=$1")
        .bind(branch.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_preview_aliases WHERE id=$1")
        .bind(pr.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers WHERE id=$1")
        .bind(worker_id)
        .execute(&pool)
        .await
        .unwrap();
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
