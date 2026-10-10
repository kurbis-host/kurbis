use super::*;

#[tokio::test]
async fn github_pr_3865_poll_persists_build_alias_and_gateway_route_with_bounded_remote_identity() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::execution::SshBubblewrapBuildRuntime;
    use kurbis_hoster::ci::{
        CiPoller, CiPollerConfig, ControlledBuildPipeline, GitHubSourceControl, SandboxLimits,
    };
    use kurbis_hoster::routing::RouteRepository;
    use reqwest::Client;

    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("Pumpkin-MC/Pumpkin-test-{}", Uuid::new_v4().simple());
    let watched = repository
        .register_repository(&repository_name, "master")
        .await
        .unwrap();
    let fixture = Pr3865GithubFixture::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn({
        let fixture = fixture.clone();
        async move {
            axum::serve(
                listener,
                axum::Router::new()
                    .fallback(axum::routing::any(pr3865_github_api))
                    .with_state(fixture),
            )
            .await
            .unwrap();
        }
    });
    let api_base = format!("http://{address}");
    let source = std::sync::Arc::new(
        GitHubSourceControl::new(Client::new(), "fixture-token")
            .unwrap()
            .with_api_base_url(api_base),
    );

    // The old remote Bubblewrap identity embedded all operator paths and could
    // exceed ci_builds.toolchain_version's 255-byte constraint. Use deliberately
    // long local fixture paths so this test fails if that unbounded identity is
    // ever passed back into BuildKey again.
    let path_suffix = "x".repeat(160);
    let remote = SshBubblewrapBuildRuntime::new(
        pool.clone(),
        "ci@127.0.0.1",
        format!("/home/ci/{path_suffix}/runner.py"),
        format!("/opt/ci/{path_suffix}/bubblewrap"),
        format!("/opt/ci/{path_suffix}/rootfs"),
        format!("/opt/ci/{path_suffix}/toolchain"),
        format!("/home/ci/{path_suffix}/workspace"),
        format!("/opt/ci/{path_suffix}/source-preparer"),
        "1.98.1",
        1,
    )
    .unwrap();
    let remote_identity = remote.build_identity().to_owned();
    assert!(remote_identity.len() <= 255);
    assert!(remote_identity.contains("identity_sha256="));

    let root_domain = unique_ci_root_domain();
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
            root_domain: root_domain.clone(),
            patch_fingerprint: "patch-v1".into(),
            toolchain_version: remote_identity,
            target_architecture: "x86_64-linux".into(),
            public_status_base_url: None,
        })
        .with_check_runs(check_runs.clone())
        .with_builder(builder);

    poller.poll_once(watched.id).await.unwrap();
    // A periodic poll gets a 304 and still performs the unconditional page-one
    // refetch; the durable rows must remain idempotent and ready.
    poller.poll_once(watched.id).await.unwrap();

    let head_sha = "c80f6fd0123456789abcdef0123456789abcdef0";
    let ref_row: (String, String) = sqlx::query_as(
        "SELECT kind, commit_sha FROM ci_refs WHERE repository_id = $1 AND name = 'pr-3865'",
    )
    .bind(watched.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(ref_row, ("pull_request".into(), head_sha.into()));
    let build_row: (String, Option<String>) = sqlx::query_as(
        "SELECT stage, artifact_digest FROM ci_builds WHERE repository = $1 AND commit_sha = $2",
    )
    .bind(&repository_name)
    .bind(head_sha)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(build_row.0, "ready");
    assert!(
        build_row.1.is_some(),
        "a ready PR build must publish an artifact"
    );
    let build_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ci_builds WHERE repository = $1 AND commit_sha = $2",
    )
    .bind(&repository_name)
    .bind(head_sha)
    .fetch_one(&pool)
    .await
    .unwrap();
    let alias = CiHostname::for_ref(CiRefKind::PullRequest, "pr-3865", &root_domain).unwrap();
    let commit_alias = CiHostname::for_ref(CiRefKind::Commit, head_sha, &root_domain).unwrap();
    let alias_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ci_preview_aliases
         WHERE repository = $1 AND commit_sha = $2 AND hostname = ANY($3::text[])",
    )
    .bind(&repository_name)
    .bind(head_sha)
    .bind([alias.value.clone(), commit_alias.value])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(build_count, 1);
    assert_eq!(
        alias_count, 2,
        "immutable commit and stable PR aliases are both durable for this root domain"
    );
    let preview = repository
        .lookup_preview(&alias.value)
        .await
        .unwrap()
        .expect("PR stable alias is durable after a successful build");
    assert_eq!(preview.commit_sha, head_sha);
    assert_eq!(preview.state, kurbis_hoster::ci::CiPreviewState::Ready);
    let route = RouteRepository::new(pool.clone(), "kurbis.host")
        .resolve_ci_hostname(&alias.value, &root_domain)
        .await
        .unwrap()
        .expect("gateway resolves the persisted PR alias");
    assert_eq!(route.preview_id, preview.id);
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert!(check_runs.calls.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    assert!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.contains("/pulls?") && request.contains("page=1"))
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    pool.close().await;
}

#[tokio::test]
async fn a_slow_provider_poll_does_not_keep_a_postgres_transaction_or_queue_pollers() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let watched = repository
        .register_repository(&format!("fixture/Pumpkin-{}", Uuid::new_v4()), "main")
        .await
        .unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    let source = std::sync::Arc::new(PausedPollSource {
        started: std::sync::Mutex::new(Some(started_tx)),
        resume: tokio::sync::Mutex::new(Some(resume_rx)),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });

    let first = {
        let repository = repository.clone();
        let source = source.clone();
        tokio::spawn(async move {
            repository
                .poll_repository(watched.id, source.as_ref())
                .await
        })
    };
    started_rx.await.unwrap();

    let idle_transactions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_stat_activity
         WHERE datname = current_database() AND pid <> pg_backend_pid()
           AND state = 'idle in transaction'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let competing_polls = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        futures::future::join_all((0..18).map(|_| {
            let repository = repository.clone();
            let source = source.clone();
            async move {
                repository
                    .poll_repository(watched.id, source.as_ref())
                    .await
            }
        })),
    )
    .await
    .expect("overlapping pollers must skip instead of queueing behind a row lock");
    let competing_polls = competing_polls
        .into_iter()
        .map(Result::unwrap)
        .collect::<Vec<_>>();

    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    drop(resume_tx);
    wait_for_poll_lease_release(&pool, watched.id).await;

    assert_eq!(
        idle_transactions, 0,
        "provider I/O must run outside a DB transaction"
    );
    assert_eq!(competing_polls.len(), 18);
    assert!(
        competing_polls.iter().all(Option::is_none),
        "only the active poll may call the provider"
    );
    assert_eq!(source.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        repository
            .poll_repository(watched.id, &FixtureSource::default())
            .await
            .unwrap()
            .is_some(),
        "a later poll may claim the repository immediately after cancellation"
    );
    pool.close().await;
}
#[tokio::test]
async fn a_provider_error_releases_the_repository_poll_lease() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let watched = repository
        .register_repository(&format!("fixture/Pumpkin-{}", Uuid::new_v4()), "main")
        .await
        .unwrap();

    assert!(
        repository
            .poll_repository(watched.id, &FailingPollSource)
            .await
            .is_err()
    );
    let released: bool = sqlx::query_scalar(
        "SELECT poll_lease_token IS NULL FROM ci_watched_repositories WHERE id = $1",
    )
    .bind(watched.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(released, "provider errors must not leave a live poll lease");
    assert!(
        repository
            .poll_repository(watched.id, &FixtureSource::default())
            .await
            .unwrap()
            .is_some(),
        "the next poll can proceed after a provider error"
    );
    pool.close().await;
}

#[tokio::test]
async fn an_expired_poll_cannot_overwrite_the_checkpoint_of_its_successor() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let watched = repository
        .register_repository(&format!("fixture/Pumpkin-{}", Uuid::new_v4()), "main")
        .await
        .unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    let slow_source = std::sync::Arc::new(PausedPollSource {
        started: std::sync::Mutex::new(Some(started_tx)),
        resume: tokio::sync::Mutex::new(Some(resume_rx)),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let stale_poll = {
        let repository = repository.clone();
        let source = slow_source.clone();
        tokio::spawn(async move {
            repository
                .poll_repository(watched.id, source.as_ref())
                .await
        })
    };
    started_rx.await.unwrap();

    sqlx::query(
        "UPDATE ci_watched_repositories SET poll_lease_expires_at = now() - interval '1 second'
         WHERE id = $1",
    )
    .bind(watched.id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        repository
            .poll_repository(watched.id, &CheckpointPollSource("new-poll"))
            .await
            .unwrap()
            .is_some()
    );

    resume_tx.send(()).unwrap();
    assert!(
        stale_poll.await.unwrap().is_err(),
        "an expired poll is fenced before it can persist"
    );
    let etag: Option<String> =
        sqlx::query_scalar("SELECT etag FROM ci_watched_repositories WHERE id = $1")
            .bind(watched.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(etag.as_deref(), Some("new-poll"));
    pool.close().await;
}

#[tokio::test]
async fn historical_closed_pull_requests_are_not_requeued_as_stale_builds() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{CiPoller, CiPollerConfig};

    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
    let watched = repository
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let mut refs = (1..=24)
        .map(|number| SourceRef {
            kind: SourceRefKind::PullRequest,
            name: format!("pr-{number}"),
            number: Some(number),
            commit_sha: format!("{number:040x}"),
            source_repository: Some(repository_name.clone()),
            closed: true,
            deleted: false,
            updated_at: None,
        })
        .collect::<Vec<_>>();
    refs.push(SourceRef {
        kind: SourceRefKind::PullRequest,
        name: "pr-25".into(),
        number: Some(25),
        commit_sha: "fedcba9876543210fedcba9876543210fedcba98".into(),
        source_repository: Some(repository_name.clone()),
        closed: false,
        deleted: false,
        updated_at: None,
    });
    refs.push(SourceRef {
        kind: SourceRefKind::Branch,
        name: "main".into(),
        number: None,
        commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
        source_repository: Some(repository_name.clone()),
        closed: false,
        deleted: false,
        updated_at: None,
    });
    let poller = CiPoller::new(
        repository.clone(),
        std::sync::Arc::new(HistoricalRefsSource { refs }),
    )
    .with_config(CiPollerConfig {
        root_domain: unique_ci_root_domain(),
        patch_fingerprint: "patch-v1".into(),
        toolchain_version: "test-toolchain".into(),
        target_architecture: "x86_64-linux".into(),
        public_status_base_url: None,
    });

    poller.poll_once(watched.id).await.unwrap();

    let build_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ci_builds WHERE repository = $1")
            .bind(&repository_name)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        build_count, 2,
        "only the current open PR head and lazy branch record may create build rows"
    );
    let queued_prs = repository
        .queued_pull_request_builds(&repository_name)
        .await
        .unwrap();
    assert_eq!(queued_prs.len(), 1);
    assert_eq!(
        queued_prs[0].key.commit_sha,
        "fedcba9876543210fedcba9876543210fedcba98"
    );
    pool.close().await;
}
