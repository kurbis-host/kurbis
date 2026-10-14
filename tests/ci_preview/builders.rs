use super::*;

use super::*;

#[tokio::test]
async fn controlled_build_pipeline_verifies_sandbox_caches_artifact_and_deduplicates() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{
        BuildRequest, CiBuildRetention, ControlledBuildPipeline, SandboxLimits,
    };
    let pool = pool().await;
    let repo = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
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
        retention: CiBuildRetention::Preview,
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
            Some(Utc::now() + chrono::Duration::hours(BUILD_TTL_HOURS)),
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
            .put("wrong-digest", "ci/test/wrong.tar", bytes, Some(Utc::now()))
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

struct FailOnceArtifactCache {
    failures_left: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl kurbis_hoster::ci::ArtifactCache for FailOnceArtifactCache {
    async fn put(
        &self,
        digest: &str,
        object_key: &str,
        bytes: &[u8],
        _expires_at: Option<chrono::DateTime<Utc>>,
    ) -> Result<kurbis_hoster::ci::BuildOutput, kurbis_hoster::ci::CiServiceError> {
        if self
            .failures_left
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
        {
            return Err(kurbis_hoster::ci::CiServiceError::Internal(
                "temporary object-store outage".into(),
            ));
        }
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

#[tokio::test]
async fn object_store_failure_is_retryable_and_reconciles_the_existing_check_run() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{
        BuildRequest, CiBuildRetention, ControlledBuildPipeline, SandboxLimits,
    };
    let pool = pool().await;
    let repo = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
    repo.register_repository(&repository_name, "main")
        .await
        .unwrap();
    let components = std::sync::Arc::new(TestBuildComponents::default());
    let check_run_id = (Uuid::new_v4().as_u128() & i64::MAX as u128) as i64;
    let check_runs = std::sync::Arc::new(TestCheckRuns {
        id: check_run_id,
        stages: std::sync::Mutex::new(Vec::new()),
    });
    let cache = std::sync::Arc::new(FailOnceArtifactCache {
        failures_left: std::sync::atomic::AtomicUsize::new(1),
    });
    let pipeline = ControlledBuildPipeline::new(
        repo.clone(),
        std::sync::Arc::new(FixtureSource::default()),
        components.clone(),
        components,
        cache,
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
    let (queued, _) = repo.ensure_build(&key).await.unwrap();
    repo.record_check_run(queued.id, Some(check_run_id), None, CiBuildStage::Queued)
        .await
        .unwrap();
    let request = BuildRequest {
        key: key.clone(),
        source_repository: None,
        source_archive: b"pinned-source-archive".to_vec(),
        retention: CiBuildRetention::Preview,
    };
    assert!(pipeline.run(request.clone()).await.is_err());
    let (stage, failure_class, retry_count): (String, String, i32) =
        sqlx::query_as("SELECT stage, failure_class, retry_count FROM ci_builds WHERE id=$1")
            .bind(queued.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stage, "failed");
    assert_eq!(failure_class, "transient");
    assert_eq!(retry_count, 1);
    assert_eq!(
        check_runs.stages.lock().unwrap().last(),
        Some(&CiBuildStage::Failed),
        "the transient failure is visible before a retry is scheduled"
    );

    assert!(
        !repo
            .claim_build(queued.id, Uuid::new_v4(), chrono::Duration::minutes(5))
            .await
            .unwrap_or(false)
    );
    sqlx::query("UPDATE ci_builds SET retry_at=now() - interval '1 second' WHERE id=$1")
        .bind(queued.id)
        .execute(&pool)
        .await
        .unwrap();
    let rebuilt = pipeline.run(request).await.unwrap();
    assert_eq!(rebuilt.id, queued.id, "retry keeps exact build-key dedupe");
    assert_eq!(rebuilt.stage, CiBuildStage::Ready);
    assert_eq!(
        check_runs.stages.lock().unwrap().last(),
        Some(&CiBuildStage::Ready),
        "the existing Check Run is reconciled to success after retry"
    );
    pool.close().await;
}

#[tokio::test]
async fn permanent_build_promotion_reuses_ready_artifact_without_expiry() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    use kurbis_hoster::ci::{CiBuildRetention, CiBuildStage};
    let pool = pool().await;
    let repo = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
    repo.register_repository(&repository_name, "master")
        .await
        .unwrap();
    let key = BuildKey::new(
        &repository_name,
        "0123456789abcdef0123456789abcdef01234567",
        "patch-v1",
        "native",
        "x86_64-linux",
    )
    .unwrap();
    let (build, _) = repo.ensure_build(&key).await.unwrap();
    for stage in [
        CiBuildStage::Fetching,
        CiBuildStage::Patching,
        CiBuildStage::Compiling,
        CiBuildStage::Packaging,
    ] {
        repo.transition_build(build.id, stage, None).await.unwrap();
    }
    let artifact = b"stable-pumpkin-artifact";
    let digest = blake3::hash(artifact).to_hex().to_string();
    repo.attach_artifact(
        build.id,
        &digest,
        &format!("ci/{digest}.tar"),
        artifact.len() as i64,
    )
    .await
    .unwrap();
    let ready = repo
        .transition_build(build.id, CiBuildStage::Ready, None)
        .await
        .unwrap();

    let (promoted, inserted) = repo
        .ensure_build_with_retention(&key, CiBuildRetention::Permanent)
        .await
        .unwrap();
    assert!(!inserted);
    assert_eq!(promoted.id, ready.id);
    assert_eq!(promoted.stage, CiBuildStage::Ready);
    assert_eq!(promoted.artifact_digest.as_deref(), Some(digest.as_str()));
    assert!(promoted.expires_at.is_none());

    let (retention, build_expiry, artifact_expiry): (String, Option<chrono::DateTime<Utc>>, Option<chrono::DateTime<Utc>>) =
        sqlx::query_as(
            "SELECT build.retention_policy, build.expires_at, artifact.expires_at
             FROM ci_builds build JOIN ci_artifacts artifact ON artifact.digest = build.artifact_digest
             WHERE build.id = $1",
        )
        .bind(build.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(retention, "permanent");
    assert!(build_expiry.is_none());
    assert!(artifact_expiry.is_none());

    repo.expire(Utc::now() + chrono::Duration::days(365))
        .await
        .unwrap();
    let retained_stage: String = sqlx::query_scalar("SELECT stage FROM ci_builds WHERE id = $1")
        .bind(build.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(retained_stage, "ready");
    pool.close().await;
}
