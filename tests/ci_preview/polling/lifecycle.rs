use super::*;

#[tokio::test]
async fn default_branch_poll_eagerly_builds_permanent_main_artifact_without_preview_alias() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{CiBuildStage, ControlledBuildPipeline, SandboxLimits};

    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
    let watched = repository
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let source = std::sync::Arc::new(FixtureSource::default());
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
    let poller = kurbis_hoster::ci::CiPoller::new(repository.clone(), source.clone())
        .with_config(kurbis_hoster::ci::CiPollerConfig {
            root_domain: unique_ci_root_domain(),
            patch_fingerprint: "patch-main-v1".into(),
            toolchain_version: "test-toolchain".into(),
            target_architecture: "x86_64-linux".into(),
            public_status_base_url: Some("https://panel.example.test/ci/previews".into()),
        })
        .with_builder(builder);

    poller.poll_once(watched.id).await.unwrap();
    let commit_sha = source.commit_sha.read().unwrap().clone();
    let (stage, retention, expires_at, artifact_digest): (
        String,
        String,
        Option<chrono::DateTime<Utc>>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT stage, retention_policy, expires_at, artifact_digest FROM ci_builds
         WHERE repository = $1 AND commit_sha = $2",
    )
    .bind(&repository_name)
    .bind(&commit_sha)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stage, "ready");
    assert_eq!(retention, "permanent");
    assert!(expires_at.is_none());
    assert!(artifact_digest.is_some());
    let aliases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ci_preview_aliases WHERE repository = $1 AND commit_sha = $2",
    )
    .bind(&repository_name)
    .bind(&commit_sha)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(aliases, 0, "main build must not create a preview alias");
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );

    // Re-polling the unchanged default branch reuses the same immutable build
    // and never recompiles or changes its permanent retention policy.
    poller.poll_once(watched.id).await.unwrap();
    assert_eq!(
        components
            .compile_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let (build_count, permanent_count): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE retention_policy = 'permanent')
         FROM ci_builds WHERE repository = $1 AND commit_sha = $2",
    )
    .bind(&repository_name)
    .bind(&commit_sha)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((build_count, permanent_count), (1, 1));
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
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
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
        .check_run_reconciliation_candidates_for_repository(8, &repository_name)
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
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
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
