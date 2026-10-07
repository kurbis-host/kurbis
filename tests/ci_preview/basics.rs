use super::*;

#[tokio::test]
async fn concurrent_build_requests_share_one_durable_ci_build() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
    let key = BuildKey::new(
        repository_name,
        "0123456789abcdef0123456789abcdef01234567",
        "patch-v1",
        "rust-1.98@sha256:test",
        "x86_64-linux",
    )
    .unwrap();

    let (left, right) = tokio::join!(repository.ensure_build(&key), repository.ensure_build(&key),);
    let (left, left_created) = left.unwrap();
    let (right, right_created) = right.unwrap();
    assert_eq!(left.id, right.id);
    assert_ne!(left_created, right_created);

    let row_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ci_builds WHERE repository = $1 AND commit_sha = $2",
    )
    .bind(&key.repository)
    .bind(&key.commit_sha)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row_count, 1);
    pool.close().await;
}

#[tokio::test]
async fn ci_builds_deduplicate_and_stable_aliases_advance_only_when_ready() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let root_domain = unique_ci_root_domain();
    let name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
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
    assert_eq!(source_repository, "Pumpkin-MC/Pumpkin");
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
async fn public_check_run_status_is_read_only_and_omits_private_diagnostics() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
    repository
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
    let hostname = CiHostname::for_ref(
        CiRefKind::Commit,
        &key.commit_sha,
        kurbis_hoster::ci::CI_ROOT_DOMAIN,
    )
    .unwrap();
    repository
        .ensure_preview(
            &repository_name,
            &key.commit_sha,
            CiRefKind::Commit,
            None,
            &hostname,
            build.id,
        )
        .await
        .unwrap();

    let state = common::api_state(
        pool.clone(),
        "public-status-internal-secret",
        std::sync::Arc::new(object_store::memory::InMemory::new()),
    );
    let service = kurbis_hoster::control_plane::api::router(state);
    let response = service
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/public/ci/previews/{}", hostname.value))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[axum::http::header::CACHE_CONTROL],
        "no-store"
    );
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["hostname"], hostname.value);
    assert_eq!(status["commit_sha"], key.commit_sha);
    assert_eq!(status["state"], "queued");
    assert!(status.get("build_log").is_none());
    assert!(status.get("failure_message").is_none());
    assert!(status.get("runtime_generation").is_none());
    assert!(status.get("worker_address").is_none());

    let hostile = service
        .oneshot(
            Request::builder()
                .uri("/public/ci/previews/commit-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.pumpkin.kurbis.host.evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(hostile.status(), StatusCode::NOT_FOUND);
    pool.close().await;
}

#[tokio::test]
async fn first_branch_join_uses_latest_observed_head_after_lobby_was_created() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
    let watched = repository
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let source = std::sync::Arc::new(FixtureSource::default());
    let root_domain = unique_ci_root_domain();
    let poller = kurbis_hoster::ci::CiPoller::new(repository.clone(), source.clone()).with_config(
        kurbis_hoster::ci::CiPollerConfig {
            root_domain: root_domain.clone(),
            patch_fingerprint: "patch-v1".into(),
            toolchain_version: "rust-1.98@sha256:test".into(),
            target_architecture: "x86_64-linux".into(),
            public_status_base_url: Some("https://panel.example.test/ci/previews".into()),
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
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
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
