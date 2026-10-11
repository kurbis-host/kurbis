use super::*;

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
        .register_repository("Pumpkin-MC/Pumpkin", "main")
        .await
        .unwrap();

    let transient_sha = format!("{}{:08x}", Uuid::new_v4().simple(), rand::random::<u32>());
    let transient_key = BuildKey::new(
        "Pumpkin-MC/Pumpkin",
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
        repository: "Pumpkin-MC/Pumpkin".into(),
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
        "Pumpkin-MC/Pumpkin",
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
            ("Pumpkin-MC/Pumpkin".into(), transient_sha.clone()),
            ("Pumpkin-MC/Pumpkin".into(), transient_sha),
            ("Pumpkin-MC/Pumpkin".into(), permanent_sha),
        ],
    );
    pool.close().await;
}
#[tokio::test]
async fn transient_failed_pr_build_retries_after_backoff_with_immutable_dedupe_and_lease_fencing() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
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
        .check_run_reconciliation_candidates_for_repository(8, &repository_name)
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
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
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
        format!("fixture/Pumpkin-{}", Uuid::new_v4()),
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
