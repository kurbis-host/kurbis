use super::*;

#[tokio::test]
async fn production_poll_orchestration_eagerly_builds_fork_pr_once_with_one_check_run() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{CiPoller, CiPollerConfig, ControlledBuildPipeline, SandboxLimits};

    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let root_domain = unique_ci_root_domain();
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
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

#[tokio::test]
async fn polling_recovers_admitted_pr_build_after_unchanged_provider_poll() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{CiPoller, CiPollerConfig, ControlledBuildPipeline, SandboxLimits};

    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
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

#[tokio::test]
async fn pull_request_poll_continues_when_check_run_permission_is_denied() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{CiPoller, CiPollerConfig, ControlledBuildPipeline, SandboxLimits};

    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
    let watched = repository
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let root_domain = unique_ci_root_domain();
    let head_sha = "fedcba9876543210fedcba9876543210fedcba98";
    let source = std::sync::Arc::new(PullRequestFixtureSource {
        fetches: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        head_sha: std::sync::Arc::new(std::sync::RwLock::new(head_sha.to_owned())),
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
    let poller = CiPoller::new(repository.clone(), source)
        .with_config(CiPollerConfig {
            root_domain,
            patch_fingerprint: "patch-v1".into(),
            toolchain_version: "test-toolchain".into(),
            target_architecture: "x86_64-linux".into(),
            public_status_base_url: None,
        })
        .with_check_runs(check_runs.clone())
        .with_builder(builder);

    poller
        .poll_once(watched.id)
        .await
        .expect("Check Run permission failure must not abort PR polling");
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "PR build admission continues even when GitHub Check Runs are unavailable"
    );
    assert!(
        check_runs.calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "the denied Check Run request is attempted without aborting the poll"
    );
    let build_stage: String =
        sqlx::query_scalar("SELECT stage FROM ci_builds WHERE repository = $1 AND commit_sha = $2")
            .bind(&repository_name)
            .bind(head_sha)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(build_stage, "ready");
    let check_run_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ci_check_runs WHERE repository = $1 AND external_id IS NOT NULL",
    )
    .bind(&repository_name)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        check_run_rows, 0,
        "permission denial must not persist an external Check Run"
    );
    pool.close().await;
}

#[tokio::test]
async fn branch_polling_stays_lazy_after_admission_and_ignores_check_run_permission_errors() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{CiPoller, CiPollerConfig, ControlledBuildPipeline, SandboxLimits};

    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
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
