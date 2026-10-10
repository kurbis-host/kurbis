use super::*;

#[tokio::test]
async fn github_webhook_requires_signature_and_persists_delivery_idempotently() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::control_plane::api::router;
    use object_store::memory::InMemory;
    use std::sync::Arc;

    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let watched = repository
        .register_repository("Pumpkin-MC/Pumpkin", "main")
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
        "repository": {"full_name": "Pumpkin-MC/Pumpkin"},
        "pull_request": {
            "number": 314,
            "base": {
                "sha": "fedcba9876543210fedcba9876543210fedcba98",
                "repo": {"full_name": "Pumpkin-MC/Pumpkin"}
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
    let repository_name = "Pumpkin-MC/Pumpkin";
    // The webhook consumer reconciles every pending Check Run in the
    // disposable database. Clear provider reconciliation state first so rows
    // left by earlier serial tests cannot leak into this mock's assertions.
    sqlx::query("DELETE FROM ci_check_runs")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_check_run_tombstones")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_github_webhook_deliveries")
        .execute(&pool)
        .await
        .unwrap();
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
            "repository": {"full_name": "Pumpkin-MC/Pumpkin"},
            "pull_request": {
                "number": 314,
                "base": {"sha": base_sha, "repo": {"full_name": "Pumpkin-MC/Pumpkin"}},
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
