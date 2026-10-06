use super::*;
use bytes::Bytes;

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

struct PermissionDeniedCheckRuns {
    calls: std::sync::atomic::AtomicUsize,
}
#[async_trait]
impl kurbis_hoster::ci::CheckRunClient for PermissionDeniedCheckRuns {
    async fn create(
        &self,
        _repository: &str,
        _details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<kurbis_hoster::ci::CheckRunId, kurbis_hoster::ci::CheckRunError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(kurbis_hoster::ci::CheckRunError::Http {
            status: StatusCode::FORBIDDEN,
            message: "fixture permission denied".into(),
        })
    }

    async fn update(
        &self,
        _repository: &str,
        _id: kurbis_hoster::ci::CheckRunId,
        _details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<(), kurbis_hoster::ci::CheckRunError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(kurbis_hoster::ci::CheckRunError::Http {
            status: StatusCode::FORBIDDEN,
            message: "fixture permission denied".into(),
        })
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
        head_sha: std::sync::Arc::new(std::sync::RwLock::new(
            "fedcba9876543210fedcba9876543210fedcba98".to_owned(),
        )),
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
        .with_check_runs(check_runs.clone())
        .with_root_domain(root_domain.clone()),
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

    let (first_poll, overlapping_poll) =
        tokio::join!(poller.poll_once(watched.id), poller.poll_once(watched.id),);
    first_poll.unwrap();
    overlapping_poll.unwrap();
    poller.poll_once(watched.id).await.unwrap();

    assert_eq!(
        fetches.lock().unwrap().as_slice(),
        &[(
            "contributor/Pumpkin".to_owned(),
            "fedcba9876543210fedcba9876543210fedcba98".to_owned(),
        )],
        "overlapping pollers and later polls fetch the immutable fork head only once"
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
struct EmptyPollSource;
#[async_trait]
impl SourceControlClient for EmptyPollSource {
    async fn poll(
        &self,
        _repository: &str,
        checkpoint: &PollCheckpoint,
    ) -> Result<PollResult, kurbis_hoster::ci::PollError> {
        Ok(PollResult {
            refs: Vec::new(),
            eager_refs: Vec::new(),
            checkpoint: PollCheckpoint {
                last_successful_poll: Some(Utc::now()),
                ..checkpoint.clone()
            },
            not_modified: true,
            retry_after: None,
            complete_kinds: Vec::new(),
        })
    }

    async fn fetch_commit(
        &self,
        _repository: &str,
        commit_sha: &str,
    ) -> Result<Vec<u8>, kurbis_hoster::ci::PollError> {
        Ok(format!("recovered-pr-head:{commit_sha}").into_bytes())
    }
}

#[tokio::test]
async fn polling_recovers_admitted_pr_build_after_unchanged_provider_poll() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{CiPoller, CiPollerConfig, ControlledBuildPipeline, SandboxLimits};

    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("kurbis-host/Pumpkin-{}", Uuid::new_v4());
    let watched = repository
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let commit_sha = "fedcba9876543210fedcba9876543210fedcba98";
    sqlx::query(
        "INSERT INTO ci_refs(repository_id,kind,name,number,commit_sha,source_repository,closed,deleted)
         VALUES($1,'pull_request','pr-77',77,$2,'contributor/Pumpkin',false,false)",
    )
    .bind(watched.id)
    .bind(commit_sha)
    .execute(&pool)
    .await
    .unwrap();
    let key = BuildKey::new(
        &repository_name,
        commit_sha,
        "patch-recovery-v1",
        "test-toolchain",
        "x86_64-linux",
    )
    .unwrap();
    let (queued, _) = repository.ensure_build(&key).await.unwrap();
    assert_eq!(queued.stage, CiBuildStage::Queued);

    let source = std::sync::Arc::new(EmptyPollSource);
    let components = std::sync::Arc::new(TestBuildComponents::default());
    let builder = std::sync::Arc::new(ControlledBuildPipeline::new(
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
    ));
    let poller = CiPoller::new(repository.clone(), source)
        .with_config(CiPollerConfig {
            root_domain: unique_ci_root_domain(),
            patch_fingerprint: "patch-recovery-v1".into(),
            toolchain_version: "test-toolchain".into(),
            target_architecture: "x86_64-linux".into(),
            public_status_base_url: None,
        })
        .with_builder(builder);

    poller.poll_once(watched.id).await.unwrap();
    let recovered = repository.ensure_build(&key).await.unwrap().0;
    assert_eq!(recovered.stage, CiBuildStage::Ready);
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    pool.close().await;
}

#[derive(Clone)]
struct MutableBranchSource {
    name: String,
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
                name: self.name.clone(),
                number: None,
                commit_sha,
                source_repository: Some(repository.to_owned()),
                closed: false,
                deleted: false,
                updated_at: Some(Utc::now()),
            }],
            eager_refs: Vec::new(),
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
async fn branch_polling_stays_lazy_after_admission_and_ignores_check_run_permission_errors() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{CiPoller, CiPollerConfig, ControlledBuildPipeline, SandboxLimits};

    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("kurbis-host/Pumpkin-{}", Uuid::new_v4());
    let watched = repository
        .register_repository(&repository_name, "master")
        .await
        .unwrap();
    let root_domain = format!("pumpkin-{}.kurbis.host", Uuid::new_v4().simple());
    let branch_name = "copilot/pumpkin-game-tick-research";
    let first_sha = "0123456789abcdef0123456789abcdef01234567";
    let next_sha = "fedcba9876543210fedcba9876543210fedcba98";
    let source = std::sync::Arc::new(MutableBranchSource {
        name: branch_name.into(),
        commit: std::sync::Arc::new(std::sync::RwLock::new(first_sha.into())),
        fetches: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
    });
    let components = std::sync::Arc::new(TestBuildComponents::default());
    let check_runs = std::sync::Arc::new(PermissionDeniedCheckRuns {
        calls: std::sync::atomic::AtomicUsize::new(0),
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

    // Polling observes the branch and creates only lazy DB records.
    poller.poll_once(watched.id).await.unwrap();
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "branch polling must not dispatch the BuildCoordinator"
    );
    assert_eq!(
        check_runs.calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "branch polling must not create a GitHub Check Run"
    );
    let check_run_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ci_check_runs WHERE repository = $1")
            .bind(&repository_name)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        check_run_rows, 0,
        "lazy branch polling must not persist Check Runs"
    );
    let branch_host = CiHostname::for_ref(CiRefKind::Branch, branch_name, &root_domain).unwrap();
    let lobby = repository
        .lookup_preview(&branch_host.value)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lobby.state, kurbis_hoster::ci::CiPreviewState::Lobby);

    // The explicit join path remains the only branch build admission point.
    let (admitted, request, build_id) = repository
        .prepare_lazy_branch_admission(lobby.id, &root_domain)
        .await
        .unwrap();
    assert_eq!(admitted.commit_sha, first_sha);
    builder.run(request.unwrap()).await.unwrap();
    assert_eq!(build_id, Some(admitted.build_id));
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "an explicit branch join still dispatches exactly one build"
    );

    // A later branch head is persisted for the next demand-triggered join, but
    // does not build or create a Check Run while the current runtime is active.
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
    assert_eq!(next_build.stage, CiBuildStage::Queued);
    assert_eq!(repository.check_run_id(next_build.id).await.unwrap(), None);
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a branch head update must not dispatch another build"
    );
    assert_eq!(
        check_runs.calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "branch head updates must remain outside the Check Run surface"
    );
    assert_eq!(
        repository
            .lookup_preview(&branch_host.value)
            .await
            .unwrap()
            .unwrap()
            .commit_sha,
        first_sha,
        "the active branch runtime continues serving its admitted commit"
    );
    assert_eq!(
        source.fetches.lock().unwrap().as_slice(),
        &[first_sha.to_owned()],
        "branch polling does not fetch source archives until a join dispatches a build"
    );
    pool.close().await;
}

#[tokio::test]
async fn deleted_branch_expires_its_preview_and_claims_world_cleanup_but_not_closed_prs() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use kurbis_hoster::control_plane::{
        api::router, maintenance::finish_claimed_stale_ci_preview_deletion,
    };
    use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path as StorePath};
    use std::sync::Arc;

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
    .bind(repository_name.clone())
    .execute(&pool)
    .await
    .unwrap();

    let worker_id = Uuid::new_v4();
    let worker_token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let worker_lease = Uuid::new_v4();
    let snapshot_store = Arc::new(InMemory::new());
    let snapshot_bytes = Bytes::from_static(b"branch-world-snapshot-before-worker-loss");
    let snapshot_key = format!("ci-previews/{}/worlds/1-test.tar.gz", branch.id);
    snapshot_store
        .put(
            &StorePath::parse(&snapshot_key).unwrap(),
            PutPayload::from_bytes(snapshot_bytes.clone()),
        )
        .await
        .unwrap();
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
    sqlx::query(
        "INSERT INTO worker_credentials(worker_id,api_token_hash,lease_id,lease_expires_at)
         VALUES($1,$2,$3,now()+interval '1 hour')",
    )
    .bind(worker_id)
    .bind(blake3::hash(worker_token.as_bytes()).as_bytes().to_vec())
    .bind(worker_lease)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE workers SET reserved_memory_bytes=$2,reserved_disk_bytes=$3,reserved_cpu_millis=$4 WHERE id=$1",
    )
    .bind(worker_id)
    .bind(kurbis_hoster::ci::CI_RUNTIME_MEMORY_BYTES)
    .bind(kurbis_hoster::ci::CI_RUNTIME_DISK_BYTES)
    .bind(kurbis_hoster::ci::CI_RUNTIME_CPU_MILLIS)
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
         (preview_id,worker_id,generation,lease_token,java_port,bedrock_port,signal_port,state,active,capacity_reserved,world_initialized,capability_hash)
         VALUES($1,$2,1,$3,28000,28001,28002,'running',true,true,true,$4)",
    )
    .bind(branch.id)
    .bind(worker_id)
    .bind(lease_token)
    .bind([3u8; 32].as_slice())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ci_world_snapshots(preview_id,object_key,size_bytes,checksum,source_generation)
         VALUES($1,$2,$3,$4,1)",
    )
    .bind(branch.id)
    .bind(&snapshot_key)
    .bind(snapshot_bytes.len() as i64)
    .bind(blake3::hash(&snapshot_bytes).to_hex().to_string())
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
    sqlx::query("UPDATE workers SET heartbeat_at=now()-interval '10 minutes' WHERE id=$1")
        .bind(worker_id)
        .execute(&pool)
        .await
        .unwrap();

    let runtime_repository = kurbis_hoster::ci::CiPreviewRuntimeRepository::new(pool.clone());
    let stale_claim = runtime_repository.claim_stale_expired(16).await.unwrap();
    assert_eq!(stale_claim.len(), 1);
    assert_eq!(stale_claim[0].placement.preview_id, branch.id);
    assert_eq!(stale_claim[0].placement.lease_token, lease_token);

    let app = router(common::api_state(
        pool.clone(),
        "ci-branch-delete-secret",
        snapshot_store.clone(),
    ));
    let stale_deleted_ack = Request::builder()
        .method(axum::http::Method::POST)
        .uri(format!(
            "/internal/worker/control/ci/runtimes/{}/deleted",
            branch.id
        ))
        .header("authorization", format!("Bearer {worker_token}"))
        .header("x-worker-lease", worker_lease.to_string())
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"generation":1,"lease_token":lease_token}).to_string(),
        ))
        .unwrap();
    let stale_delete_response = app.clone().oneshot(stale_deleted_ack).await.unwrap();
    if stale_delete_response.status() != StatusCode::CONFLICT {
        let (parts, body) = stale_delete_response.into_parts();
        panic!(
            "stale delete response {}: {}",
            parts.status,
            String::from_utf8_lossy(&to_bytes(body, 4096).await.unwrap())
        );
    }
    finish_claimed_stale_ci_preview_deletion(
        &runtime_repository,
        snapshot_store.clone(),
        &stale_claim[0],
    )
    .await
    .unwrap();
    assert!(
        snapshot_store
            .get(&StorePath::parse(&snapshot_key).unwrap())
            .await
            .is_err(),
        "control-plane cleanup removes the shared world checkpoint"
    );
    assert!(
        runtime_repository
            .current(branch.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .lookup_preview_by_id(branch.id)
            .await
            .unwrap()
            .is_none()
    );
    let (reserved_memory, reserved_disk, reserved_cpu): (i64, i64, i64) = sqlx::query_as(
        "SELECT reserved_memory_bytes,reserved_disk_bytes,reserved_cpu_millis FROM workers WHERE id=$1",
    )
    .bind(worker_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((reserved_memory, reserved_disk, reserved_cpu), (0, 0, 0));

    let app = router(common::api_state(
        pool.clone(),
        "ci-branch-delete-secret",
        snapshot_store.clone(),
    ));
    let stale_delete_ack = Request::builder()
        .method(axum::http::Method::POST)
        .uri(format!(
            "/internal/worker/control/ci/runtimes/{}/deleted",
            branch.id
        ))
        .header("authorization", format!("Bearer {worker_token}"))
        .header("x-worker-lease", worker_lease.to_string())
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"generation":1,"lease_token":lease_token}).to_string(),
        ))
        .unwrap();
    assert_eq!(
        app.oneshot(stale_delete_ack).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    sqlx::query("DELETE FROM ci_preview_aliases WHERE id=$1")
        .bind(pr.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM worker_credentials WHERE worker_id=$1")
        .bind(worker_id)
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
async fn expired_check_run_keeps_only_a_minimal_reconciliation_tombstone() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("kurbis-host/Pumpkin-{}", Uuid::new_v4());
    repository
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let key = BuildKey::new(
        &repository_name,
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
    let alias = CiHostname::for_ref(
        CiRefKind::Commit,
        &key.commit_sha,
        kurbis_hoster::ci::CI_ROOT_DOMAIN,
    )
    .unwrap();
    let expired_alias = repository
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
    sqlx::query("UPDATE ci_preview_aliases SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(expired_alias.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE ci_builds SET build_log=$2, expires_at=now()-interval '1 second' WHERE id=$1",
    )
    .bind(build.id)
    .bind("private compiler output that must not survive build/artifact expiry")
    .execute(&pool)
    .await
    .unwrap();

    repository.expire(Utc::now()).await.unwrap();
    let retained_build: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ci_builds WHERE id=$1)")
            .bind(build.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        !retained_build,
        "expired build diagnostics are deleted with the build"
    );
    let old_check_run: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ci_check_runs WHERE build_id=$1)")
            .bind(build.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!old_check_run);
    let tombstone_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ci_check_run_tombstones WHERE build_id=$1")
            .bind(build.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(tombstone_count, 1);

    let candidate = repository
        .check_run_reconciliation_candidates(8)
        .await
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.build_id == build.id)
        .expect("minimal tombstone continues terminal Check Run reconciliation");
    assert!(candidate.tombstone);
    assert_eq!(candidate.stage, CiBuildStage::Expired);
    assert_eq!(candidate.check_run_id, check_run_id);
    assert!(candidate.artifact_digest.is_none());
    assert!(candidate.failure_message.is_none());
    assert!(
        repository
            .mark_check_run_stage_synced(candidate.build_id, candidate.stage, true)
            .await
            .unwrap()
    );
    repository.expire(Utc::now()).await.unwrap();
    let deleted =
        sqlx::query_scalar::<_, bool>("SELECT NOT EXISTS(SELECT 1 FROM ci_builds WHERE id=$1)")
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

fn signed_github_request(
    delivery_id: Uuid,
    event_name: &str,
    body: Vec<u8>,
    secret: &str,
    corrupt_signature: bool,
) -> Request<Body> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(&body);
    let signature = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let signature = if corrupt_signature {
        let mut corrupt = signature.into_bytes();
        corrupt[0] = if corrupt[0] == b'0' { b'1' } else { b'0' };
        format!("sha256={}", String::from_utf8(corrupt).unwrap())
    } else {
        format!("sha256={signature}")
    };
    Request::builder()
        .method("POST")
        .uri("/webhooks/github")
        .header("x-github-delivery", delivery_id.to_string())
        .header("x-github-event", event_name)
        .header("x-hub-signature-256", signature)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn github_webhook_requires_signature_and_persists_delivery_idempotently() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::control_plane::api::router;
    use object_store::memory::InMemory;
    use std::sync::Arc;

    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let watched = repository
        .register_repository("kurbis-host/Pumpkin", "main")
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_github_webhook_deliveries WHERE repository_id=$1")
        .bind(watched.id)
        .execute(&pool)
        .await
        .unwrap();
    let state = common::api_state(
        pool.clone(),
        "webhook-internal-token",
        Arc::new(InMemory::new()),
    );
    let app = router(state);
    let secret = "test-webhook-secret";
    let delivery_id = Uuid::new_v4();
    let body = serde_json::json!({
        "action": "synchronize",
        "repository": {"full_name": "kurbis-host/Pumpkin"},
        "pull_request": {
            "number": 314,
            "base": {
                "sha": "fedcba9876543210fedcba9876543210fedcba98",
                "repo": {"full_name": "kurbis-host/Pumpkin"}
            },
            "head": {
                "sha": "0123456789abcdef0123456789abcdef01234567",
                "repo": {"full_name": "contributor/Pumpkin"}
            }
        }
    })
    .to_string()
    .into_bytes();
    let first = app
        .clone()
        .oneshot(signed_github_request(
            delivery_id,
            "pull_request",
            body.clone(),
            secret,
            false,
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::ACCEPTED);

    let duplicate = app
        .clone()
        .oneshot(signed_github_request(
            delivery_id,
            "pull_request",
            body.clone(),
            secret,
            false,
        ))
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::ACCEPTED);

    let bad_signature_id = Uuid::new_v4();
    let bad_signature = app
        .clone()
        .oneshot(signed_github_request(
            bad_signature_id,
            "pull_request",
            body.clone(),
            secret,
            true,
        ))
        .await
        .unwrap();
    assert_eq!(bad_signature.status(), StatusCode::UNAUTHORIZED);

    let mut conflict_payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    conflict_payload["pull_request"]["head"]["sha"] =
        "1123456789abcdef0123456789abcdef01234567".into();
    let conflict = app
        .clone()
        .oneshot(signed_github_request(
            delivery_id,
            "pull_request",
            conflict_payload.to_string().into_bytes(),
            secret,
            false,
        ))
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);

    let unrelated = app
        .clone()
        .oneshot(signed_github_request(
            Uuid::new_v4(),
            "ping",
            b"{}".to_vec(),
            secret,
            false,
        ))
        .await
        .unwrap();
    assert_eq!(unrelated.status(), StatusCode::NO_CONTENT);

    let oversized = app
        .oneshot(signed_github_request(
            Uuid::new_v4(),
            "pull_request",
            vec![b'x'; 1024 * 1024 + 1],
            secret,
            false,
        ))
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::BAD_REQUEST);

    let stored: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ci_github_webhook_deliveries WHERE repository_id = $1",
    )
    .bind(watched.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        stored, 1,
        "duplicate, forged, conflicting, unrelated and oversized requests cannot create extra jobs"
    );
    sqlx::query("DELETE FROM ci_github_webhook_deliveries WHERE delivery_id = $1")
        .bind(delivery_id)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

#[tokio::test]
async fn pr_updates_build_only_requested_heads_and_late_events_never_roll_back_alias() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{CiPoller, CiPollerConfig, ControlledBuildPipeline, SandboxLimits};
    use kurbis_hoster::control_plane::api::router;
    use object_store::memory::InMemory;
    use std::sync::Arc;

    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = "kurbis-host/Pumpkin";
    let watched = repository
        .register_repository(repository_name, "main")
        .await
        .unwrap();
    let unique_sha = || format!("{}{:08x}", Uuid::new_v4().simple(), rand::random::<u32>());
    let first_head = unique_sha();
    let second_head = unique_sha();
    let event_only_head = unique_sha();
    let ancestry_only = unique_sha();
    let head_sha = Arc::new(std::sync::RwLock::new(first_head.clone()));
    let fetches = Arc::new(std::sync::Mutex::new(Vec::new()));
    let source = Arc::new(PullRequestFixtureSource {
        fetches: fetches.clone(),
        head_sha: head_sha.clone(),
    });
    let components = Arc::new(TestBuildComponents::default());
    let check_runs = Arc::new(IdempotentCheckRuns {
        id: (Uuid::new_v4().as_u128() & i64::MAX as u128) as i64,
        create_count: std::sync::atomic::AtomicUsize::new(0),
        update_stages: std::sync::Mutex::new(Vec::new()),
    });
    let root_domain = unique_ci_root_domain();
    let builder = Arc::new(
        ControlledBuildPipeline::new(
            repository.clone(),
            source.clone(),
            components.clone(),
            components.clone(),
            Arc::new(TestArtifactCache),
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

    // Opening a PR with several ancestors compiles only its current head.
    poller.poll_once(watched.id).await.unwrap();
    let key_for = |sha: &str| {
        BuildKey::new(
            repository_name,
            sha,
            "patch-v1",
            "test-toolchain",
            "x86_64-linux",
        )
        .unwrap()
    };
    assert_eq!(
        repository
            .ensure_build(&key_for(&first_head))
            .await
            .unwrap()
            .0
            .stage,
        CiBuildStage::Ready
    );
    poller.poll_once(watched.id).await.unwrap();
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "repeated observation of the same opened PR head reuses the cache"
    );
    for sha in [&second_head, &event_only_head, &ancestry_only] {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM ci_builds WHERE repository=$1 AND commit_sha=$2",
        )
        .bind(repository_name)
        .bind(sha)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 0, "non-requested commit {sha} stays unbuilt");
    }

    // A synchronize update creates exactly one additional head cache entry.
    *head_sha.write().unwrap() = second_head.clone();
    poller.poll_once(watched.id).await.unwrap();
    assert_eq!(
        repository
            .ensure_build(&key_for(&second_head))
            .await
            .unwrap()
            .0
            .stage,
        CiBuildStage::Ready
    );
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        2
    );

    let mut state = common::api_state(
        pool.clone(),
        "pr-head-event-token",
        Arc::new(InMemory::new()),
    );
    let app_state = Arc::get_mut(&mut state).unwrap();
    app_state.ci_build_coordinator = Some(builder);
    app_state.ci_build_identity = Some(kurbis_hoster::ci::CiBuildIdentity {
        repository: repository_name.to_owned(),
        patch_fingerprint: "patch-v1".into(),
        toolchain_version: "test-toolchain".into(),
        target_architecture: "x86_64-linux".into(),
    });
    let app = router(state);
    let secret = "test-webhook-secret";
    let base_sha = ancestry_only.as_str();
    let mut delivery_ids = Vec::new();
    let events = [
        ("opened", first_head.as_str()),        // replay/cache hit
        ("synchronize", second_head.as_str()),  // replay/cache hit
        ("reopened", event_only_head.as_str()), // event arrives before 60s poll
        ("synchronize", second_head.as_str()),  // out-of-order older push
    ];
    for (action, sha) in events {
        let delivery_id = Uuid::new_v4();
        let body = serde_json::json!({
            "action": action,
            "repository": {"full_name": "kurbis-host/Pumpkin"},
            "pull_request": {
                "number": 314,
                "base": {"sha": base_sha, "repo": {"full_name": "kurbis-host/Pumpkin"}},
                "head": {"sha": sha, "repo": {"full_name": "contributor/Pumpkin"}}
            }
        })
        .to_string()
        .into_bytes();
        let response = app
            .clone()
            .oneshot(signed_github_request(
                delivery_id,
                "pull_request",
                body.clone(),
                secret,
                false,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        delivery_ids.push(delivery_id);
        if action == "reopened" {
            let replay = app
                .clone()
                .oneshot(signed_github_request(
                    delivery_id,
                    "pull_request",
                    body,
                    secret,
                    false,
                ))
                .await
                .unwrap();
            assert_eq!(replay.status(), StatusCode::ACCEPTED);
        }
    }
    poller.drain_github_webhooks_once().await.unwrap();
    assert_eq!(
        repository
            .ensure_build(&key_for(&event_only_head))
            .await
            .unwrap()
            .0
            .stage,
        CiBuildStage::Ready
    );
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        3,
        "each distinct opened/updated/reopened head builds once; ancestors and duplicate/out-of-order deliveries do not compile"
    );
    assert_eq!(
        check_runs
            .create_count
            .load(std::sync::atomic::Ordering::SeqCst),
        3,
        "each distinct PR head has one Check Run"
    );
    for sha in [&ancestry_only] {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM ci_builds WHERE repository=$1 AND commit_sha=$2",
        )
        .bind(repository_name)
        .bind(sha)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 0, "PR base commit remains on-demand only");
    }

    // A delivery for a newer head can precede poll observation; the stable
    // alias moves only after the poll confirms that head is authoritative.
    *head_sha.write().unwrap() = event_only_head.clone();
    poller.poll_once(watched.id).await.unwrap();
    let stable = CiHostname::for_ref(CiRefKind::PullRequest, "pr-314", &root_domain).unwrap();
    assert_eq!(
        repository
            .lookup_preview(&stable.value)
            .await
            .unwrap()
            .unwrap()
            .commit_sha,
        event_only_head
    );
    let fetched = fetches.lock().unwrap().clone();
    assert_eq!(
        fetched
            .iter()
            .map(|(_, sha)| sha.as_str())
            .collect::<std::collections::HashSet<_>>(),
        [
            first_head.as_str(),
            second_head.as_str(),
            event_only_head.as_str()
        ]
        .into_iter()
        .collect()
    );
    assert!(
        fetched
            .iter()
            .all(|(repo, _)| repo == "contributor/Pumpkin")
    );
    let processed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ci_github_webhook_deliveries WHERE repository_id=$1 AND processed_at IS NOT NULL",
    ).bind(watched.id).fetch_one(&pool).await.unwrap();
    assert_eq!(processed, delivery_ids.len() as i64);
    sqlx::query("DELETE FROM ci_github_webhook_deliveries WHERE repository_id=$1")
        .bind(watched.id)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

#[tokio::test]
async fn on_demand_full_commit_retries_only_due_transient_failures() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{CiBuildIdentity, ControlledBuildPipeline, SandboxLimits};
    use kurbis_hoster::control_plane::api::router;
    use object_store::memory::InMemory;
    use std::sync::Arc;

    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    repository
        .register_repository("kurbis-host/Pumpkin", "main")
        .await
        .unwrap();

    let transient_sha = format!("{}{:08x}", Uuid::new_v4().simple(), rand::random::<u32>());
    let transient_key = BuildKey::new(
        "kurbis-host/Pumpkin",
        &transient_sha,
        "patch-v1",
        "test-toolchain",
        "x86_64-linux",
    )
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM ci_builds WHERE repository=$1 AND commit_sha=$2"
        )
        .bind(&transient_key.repository)
        .bind(&transient_key.commit_sha)
        .fetch_one(&pool)
        .await
        .unwrap(),
        0,
        "arbitrary full commit aliases are not eagerly built",
    );

    let fetches = Arc::new(std::sync::Mutex::new(Vec::new()));
    let source = Arc::new(PullRequestFixtureSource {
        fetches: fetches.clone(),
        head_sha: Arc::new(std::sync::RwLock::new(transient_sha.clone())),
    });
    let components = Arc::new(TestBuildComponents::default());
    components
        .transient_compile_failures
        .store(1, std::sync::atomic::Ordering::SeqCst);
    let builder = Arc::new(ControlledBuildPipeline::new(
        repository.clone(),
        source,
        components.clone(),
        components.clone(),
        Arc::new(TestArtifactCache),
        SandboxLimits {
            memory_bytes: 2 * 1024 * 1024 * 1024,
            cpu_millis: 1000,
            pid_limit: 512,
            disk_bytes: 5 * 1024 * 1024 * 1024,
            process_timeout: std::time::Duration::from_secs(3600),
        },
    ));
    let mut state = common::api_state(
        pool.clone(),
        "on-demand-ci-internal-token",
        Arc::new(InMemory::new()),
    );
    let app_state = Arc::get_mut(&mut state).unwrap();
    app_state.ci_build_coordinator = Some(builder);
    app_state.ci_build_identity = Some(CiBuildIdentity {
        repository: "kurbis-host/Pumpkin".into(),
        patch_fingerprint: "patch-v1".into(),
        toolchain_version: "test-toolchain".into(),
        target_architecture: "x86_64-linux".into(),
    });
    let app = router(state);
    let transient_host = CiHostname::for_ref(
        CiRefKind::Commit,
        &transient_sha,
        kurbis_hoster::ci::CI_ROOT_DOMAIN,
    )
    .unwrap()
    .value;
    let admission = |hostname: &str| {
        Request::builder()
            .method("POST")
            .uri("/internal/ci/previews/admit")
            .header("authorization", "Bearer on-demand-ci-internal-token")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"hostname": hostname}).to_string(),
            ))
            .unwrap()
    };

    assert_eq!(
        app.clone()
            .oneshot(admission(&transient_host))
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    let failed_transient = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let build = repository.ensure_build(&transient_key).await.unwrap().0;
            if build.stage == CiBuildStage::Failed {
                break build;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("transient on-demand build failure persists");
    let (failure_class, retry_count, retry_is_future): (String, i32, bool) = sqlx::query_as(
        "SELECT failure_class, retry_count, retry_at > now() FROM ci_builds WHERE id=$1",
    )
    .bind(failed_transient.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(failure_class, "transient");
    assert_eq!(retry_count, 1);
    assert!(retry_is_future);
    assert!(
        !repository
            .is_build_retry_due(failed_transient.id)
            .await
            .unwrap()
    );

    // Demand before the durable deadline must not rerun the exact same key.
    let _ = app
        .clone()
        .oneshot(admission(&transient_host))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "an on-demand full-SHA hit cannot bypass backoff",
    );

    sqlx::query("UPDATE ci_builds SET retry_at=now() - interval '1 second' WHERE id=$1")
        .bind(failed_transient.id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        repository
            .is_build_retry_due(failed_transient.id)
            .await
            .unwrap()
    );
    let _ = app
        .clone()
        .oneshot(admission(&transient_host))
        .await
        .unwrap();
    let ready = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let build = repository.ensure_build(&transient_key).await.unwrap().0;
            let retry_count: i32 =
                sqlx::query_scalar("SELECT retry_count FROM ci_builds WHERE id=$1")
                    .bind(build.id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            if build.stage == CiBuildStage::Ready
                || (build.stage == CiBuildStage::Failed && retry_count > 1)
            {
                break build;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("due on-demand retry completes");
    assert_eq!(ready.stage, CiBuildStage::Ready);
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        2,
        "due transient retry resumes the same immutable build key exactly once",
    );
    let _ = app
        .clone()
        .oneshot(admission(&transient_host))
        .await
        .unwrap();
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        2,
        "ready immutable build hits remain cached",
    );

    // A permanent failed key remains terminal even if an operator or test
    // manually moves retry_at into the past.
    let permanent_sha = format!("{}{:08x}", Uuid::new_v4().simple(), rand::random::<u32>());
    let permanent_key = BuildKey::new(
        "kurbis-host/Pumpkin",
        &permanent_sha,
        "patch-v1",
        "test-toolchain",
        "x86_64-linux",
    )
    .unwrap();
    let permanent_host = CiHostname::for_ref(
        CiRefKind::Commit,
        &permanent_sha,
        kurbis_hoster::ci::CI_ROOT_DOMAIN,
    )
    .unwrap()
    .value;
    components
        .permanent_compile_failures
        .store(1, std::sync::atomic::Ordering::SeqCst);
    let _ = app
        .clone()
        .oneshot(admission(&permanent_host))
        .await
        .unwrap();
    let failed_permanent = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let build = repository.ensure_build(&permanent_key).await.unwrap().0;
            if build.stage == CiBuildStage::Failed {
                break build;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("permanent on-demand failure persists");
    sqlx::query("UPDATE ci_builds SET retry_at=now() - interval '1 second' WHERE id=$1")
        .bind(failed_permanent.id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !repository
            .is_build_retry_due(failed_permanent.id)
            .await
            .unwrap()
    );
    let _ = app.oneshot(admission(&permanent_host)).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        3,
        "permanent failure is never auto-retried by repeated full-SHA demand",
    );
    let (failure_class, retry_count): (String, i32) =
        sqlx::query_as("SELECT failure_class, retry_count FROM ci_builds WHERE id=$1")
            .bind(failed_permanent.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(failure_class, "permanent");
    assert_eq!(retry_count, 0);

    assert_eq!(
        fetches.lock().unwrap().as_slice(),
        &[
            ("kurbis-host/Pumpkin".into(), transient_sha.clone()),
            ("kurbis-host/Pumpkin".into(), transient_sha),
            ("kurbis-host/Pumpkin".into(), permanent_sha),
        ],
    );
    pool.close().await;
}

#[tokio::test]
async fn transient_failed_pr_build_retries_after_backoff_with_immutable_dedupe_and_lease_fencing() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("kurbis-host/Pumpkin-{}", Uuid::new_v4());
    let watched = repository
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let key = BuildKey::new(
        &repository_name,
        "fedcba9876543210fedcba9876543210fedcba98",
        "patch-v1",
        "test-toolchain",
        "x86_64-linux",
    )
    .unwrap();
    let (build, inserted) = repository.ensure_build(&key).await.unwrap();
    assert!(inserted);
    sqlx::query(
        "INSERT INTO ci_refs(repository_id,kind,name,number,source_repository,commit_sha)\n         VALUES($1,'pull_request','pr-314',314,'contributor/Pumpkin',$2)",
    )
    .bind(watched.id)
    .bind(&key.commit_sha)
    .execute(&pool)
    .await
    .unwrap();

    let initial_lease = Uuid::new_v4();
    assert!(
        repository
            .claim_build(build.id, initial_lease, chrono::Duration::minutes(5))
            .await
            .unwrap()
    );
    assert!(
        repository
            .fail_claimed_build_with_class(
                build.id,
                initial_lease,
                CiBuildStage::Packaging,
                "temporary artifact object-store outage",
                kurbis_hoster::ci::CiBuildFailureClass::Transient,
            )
            .await
            .unwrap()
    );
    repository
        .release_build(build.id, initial_lease)
        .await
        .unwrap();
    let (failure_class, retry_count, retry_is_future): (String, i32, bool) = sqlx::query_as(
        "SELECT failure_class, retry_count, retry_at > now() FROM ci_builds WHERE id=$1",
    )
    .bind(build.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(failure_class, "transient");
    assert_eq!(retry_count, 1);
    assert!(retry_is_future);
    assert!(!repository.is_build_retry_due(build.id).await.unwrap());
    assert!(
        repository
            .due_pull_request_build_retries(&repository_name)
            .await
            .unwrap()
            .is_empty()
    );

    let (cached, inserted_again) = repository.ensure_build(&key).await.unwrap();
    assert_eq!(
        cached.id, build.id,
        "the exact immutable build key is reused"
    );
    assert!(!inserted_again);
    assert_eq!(cached.stage, CiBuildStage::Failed);
    assert!(
        !repository
            .claim_build(build.id, Uuid::new_v4(), chrono::Duration::minutes(5))
            .await
            .unwrap(),
        "a failed build cannot retry before its durable retry_at"
    );

    sqlx::query("UPDATE ci_builds SET retry_at=now() - interval '1 second' WHERE id=$1")
        .bind(build.id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(repository.is_build_retry_due(build.id).await.unwrap());
    let due = repository
        .due_pull_request_build_retries(&repository_name)
        .await
        .unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].key, key);
    assert_eq!(
        due[0].source_repository.as_deref(),
        Some("contributor/Pumpkin")
    );

    let stale_lease = Uuid::new_v4();
    assert!(
        repository
            .claim_build(build.id, stale_lease, chrono::Duration::minutes(5))
            .await
            .unwrap(),
        "the immutable build becomes claimable when its backoff expires"
    );
    sqlx::query("UPDATE ci_builds SET lease_expires_at=now() - interval '1 second' WHERE id=$1")
        .bind(build.id)
        .execute(&pool)
        .await
        .unwrap();
    let active_lease = Uuid::new_v4();
    assert!(
        repository
            .claim_build(build.id, active_lease, chrono::Duration::minutes(5))
            .await
            .unwrap(),
        "a replacement worker may recover the expired retry lease"
    );
    assert!(
        !repository
            .fail_claimed_build_with_class(
                build.id,
                stale_lease,
                CiBuildStage::Packaging,
                "late stale worker failure",
                kurbis_hoster::ci::CiBuildFailureClass::Transient,
            )
            .await
            .unwrap(),
        "stale workers cannot overwrite retry state or consume retry budget"
    );

    let contender_a = Uuid::new_v4();
    let contender_b = Uuid::new_v4();
    let (claimed_a, claimed_b) = tokio::join!(
        repository.claim_build(build.id, contender_a, chrono::Duration::minutes(5)),
        repository.claim_build(build.id, contender_b, chrono::Duration::minutes(5)),
    );
    assert!(!claimed_a.unwrap() && !claimed_b.unwrap());
    let (stage, lease, retry_count): (String, Uuid, i32) =
        sqlx::query_as("SELECT stage, lease_token, retry_count FROM ci_builds WHERE id=$1")
            .bind(build.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stage, "queued");
    assert_eq!(lease, active_lease);
    assert_eq!(retry_count, 1);

    // The existing Check Run's failed conclusion is reconciled back to active
    // progress when the retry actually starts, even if the eager poll is gone.
    repository
        .transition_claimed_build(build.id, active_lease, CiBuildStage::Fetching)
        .await
        .unwrap();
    let check_run_id = i64::try_from(Uuid::new_v4().as_u128() & i64::MAX as u128).unwrap();
    repository
        .record_check_run(build.id, Some(check_run_id), None, CiBuildStage::Failed)
        .await
        .unwrap();
    repository
        .record_check_run(build.id, Some(check_run_id), None, CiBuildStage::Fetching)
        .await
        .unwrap();
    let candidates = repository
        .check_run_reconciliation_candidates(8)
        .await
        .unwrap();
    assert!(
        candidates.is_empty(),
        "successful active Check Run sync is durable"
    );
    pool.close().await;
}

#[tokio::test]
async fn permanent_build_validation_failure_never_becomes_a_retry_candidate() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("kurbis-host/Pumpkin-{}", Uuid::new_v4());
    let watched = repository
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let key = BuildKey::new(
        &repository_name,
        "0123456789abcdef0123456789abcdef01234567",
        "patch-v1",
        "test-toolchain",
        "x86_64-linux",
    )
    .unwrap();
    let (build, _) = repository.ensure_build(&key).await.unwrap();
    sqlx::query(
        "INSERT INTO ci_refs(repository_id,kind,name,number,source_repository,commit_sha)\n         VALUES($1,'pull_request','pr-314',314,'contributor/Pumpkin',$2)",
    )
    .bind(watched.id)
    .bind(&key.commit_sha)
    .execute(&pool)
    .await
    .unwrap();
    let lease = Uuid::new_v4();
    assert!(
        repository
            .claim_build(build.id, lease, chrono::Duration::minutes(5))
            .await
            .unwrap()
    );
    assert!(
        repository
            .fail_claimed_build_with_class(
                build.id,
                lease,
                CiBuildStage::Patching,
                "checked-in patch no longer applies to this commit",
                kurbis_hoster::ci::CiBuildFailureClass::Permanent,
            )
            .await
            .unwrap()
    );
    repository.release_build(build.id, lease).await.unwrap();
    sqlx::query("UPDATE ci_builds SET retry_at=now() - interval '1 second' WHERE id=$1")
        .bind(build.id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        repository
            .due_pull_request_build_retries(&repository_name)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(!repository.is_build_retry_due(build.id).await.unwrap());
    assert!(!repository.is_build_retry_due(build.id).await.unwrap());
    let expired_permanent = repository.ensure_build(&key).await.unwrap().0;
    assert_eq!(expired_permanent.stage, CiBuildStage::Failed);
    sqlx::query("UPDATE ci_builds SET expires_at=now() - interval '1 second' WHERE id=$1")
        .bind(build.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        repository.ensure_build(&key).await.unwrap().0.stage,
        CiBuildStage::Failed
    );
    assert!(
        !repository
            .claim_build(build.id, Uuid::new_v4(), chrono::Duration::minutes(5))
            .await
            .unwrap()
    );
    pool.close().await;
}

#[tokio::test]
async fn exhausted_transient_retry_budget_stays_terminal_after_build_expiry() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let key = BuildKey::new(
        format!("kurbis-host/Pumpkin-{}", Uuid::new_v4()),
        "89abcdef0123456789abcdef0123456789abcdef",
        "patch-v1",
        "test-toolchain",
        "x86_64-linux",
    )
    .unwrap();
    let (build, _) = repository.ensure_build(&key).await.unwrap();

    for attempt in 0..=kurbis_hoster::ci::CI_BUILD_MAX_RETRIES {
        if attempt > 0 {
            sqlx::query("UPDATE ci_builds SET retry_at=now() - interval '1 second' WHERE id=$1")
                .bind(build.id)
                .execute(&pool)
                .await
                .unwrap();
        }
        let lease = Uuid::new_v4();
        assert!(
            repository
                .claim_build(build.id, lease, chrono::Duration::minutes(5))
                .await
                .unwrap(),
            "attempt {attempt} is within the bounded retry budget",
        );
        assert!(
            repository
                .fail_claimed_build_with_class(
                    build.id,
                    lease,
                    CiBuildStage::Compiling,
                    "temporary host storage failure",
                    kurbis_hoster::ci::CiBuildFailureClass::Transient,
                )
                .await
                .unwrap()
        );
        repository.release_build(build.id, lease).await.unwrap();
    }

    let (stage, retry_count, retry_at): (String, i32, Option<chrono::DateTime<Utc>>) =
        sqlx::query_as("SELECT stage, retry_count, retry_at FROM ci_builds WHERE id=$1")
            .bind(build.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stage, "failed");
    assert_eq!(retry_count, kurbis_hoster::ci::CI_BUILD_MAX_RETRIES + 1);
    assert!(retry_at.is_none());
    assert!(!repository.is_build_retry_due(build.id).await.unwrap());

    sqlx::query("UPDATE ci_builds SET expires_at=now() - interval '1 second' WHERE id=$1")
        .bind(build.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        repository.ensure_build(&key).await.unwrap().0.stage,
        CiBuildStage::Failed
    );
    assert!(
        !repository
            .claim_build(build.id, Uuid::new_v4(), chrono::Duration::minutes(5))
            .await
            .unwrap(),
        "build TTL expiry cannot reset the deduplicated retry budget",
    );
    pool.close().await;
}
