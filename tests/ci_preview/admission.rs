use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kurbis_hoster::ci::{
    BuildCoordinator, BuildRequest, CI_ROOT_DOMAIN, CiBuild, CiBuildIdentity, CiBuildStage,
    CiPreviewState, CiServiceError,
};
use kurbis_hoster::control_plane::api;
use object_store::local::LocalFileSystem;

#[derive(Clone)]
struct RecordingBuilder {
    repository: CiPreviewRepository,
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<BuildKey>>>,
}

#[async_trait]
impl BuildCoordinator for RecordingBuilder {
    async fn ensure_build(&self, request: BuildRequest) -> Result<CiBuild, CiServiceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request.key.clone());
        self.repository
            .ensure_build(&request.key)
            .await
            .map(|(build, _)| build)
            .map_err(|error| CiServiceError::Internal(error.to_string()))
    }

    async fn transition(
        &self,
        _build_id: Uuid,
        _stage: CiBuildStage,
        _error: Option<&str>,
    ) -> Result<CiBuild, CiServiceError> {
        Err(CiServiceError::Internal(
            "unused in admission fixture".into(),
        ))
    }
}

async fn admission_app(
    pool: PgPool,
    repository: CiPreviewRepository,
    repository_name: &str,
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<BuildKey>>>,
    patch_fingerprint: &str,
) -> axum::Router {
    let temp = tempfile::tempdir().unwrap();
    // Leak the temporary directory for the lifetime of this test's router. The
    // local object store is only needed to construct AppState; admission never
    // touches it.
    let temp = Box::leak(Box::new(temp));
    let object_store: Arc<dyn object_store::ObjectStore> =
        Arc::new(LocalFileSystem::new_with_prefix(temp.path()).unwrap());
    let mut state = (*common::api_state(pool, "ci-admission-secret", object_store)).clone();
    state.ci_build_identity = Some(CiBuildIdentity {
        repository: repository_name.to_owned(),
        patch_fingerprint: patch_fingerprint.to_owned(),
        toolchain_version: "test-toolchain".into(),
        target_architecture: "x86_64-linux".into(),
    });
    state.ci_build_coordinator = Some(Arc::new(RecordingBuilder {
        repository,
        calls,
        requests,
    }));
    api::router(Arc::new(state))
}

async fn post_admission(app: axum::Router, hostname: &str) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/internal/ci/previews/admit")
            .header("authorization", "Bearer ci-admission-secret")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"hostname": hostname}).to_string(),
            ))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn wait_for_builder_call(calls: &AtomicUsize) {
    for _ in 0..100 {
        if calls.load(Ordering::SeqCst) > 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("admission did not request the current immutable build");
}

#[tokio::test]
async fn stale_queued_pr_alias_stays_on_old_build_until_current_candidate_is_ready() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
    let watched = repository
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let commit_sha = "fedcba9876543210fedcba9876543210fedcba98";
    sqlx::query(
        "INSERT INTO ci_refs(repository_id, kind, name, number, source_repository, commit_sha)
         VALUES($1, 'pull_request', 'pr-3865', 3865, 'Pumpkin-MC/Pumpkin', $2)",
    )
    .bind(watched.id)
    .bind(commit_sha)
    .execute(&pool)
    .await
    .unwrap();

    let old_key = BuildKey::new(
        &repository_name,
        commit_sha,
        "old-patch-fingerprint",
        "test-toolchain",
        "x86_64-linux",
    )
    .unwrap();
    let (old_build, _) = repository.ensure_build(&old_key).await.unwrap();
    let alias = CiHostname::for_ref(CiRefKind::PullRequest, "pr-3865", CI_ROOT_DOMAIN).unwrap();
    let old_preview = repository
        .ensure_preview(
            &repository_name,
            commit_sha,
            CiRefKind::PullRequest,
            Some("pr-3865"),
            &alias,
            old_build.id,
        )
        .await
        .unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let app = admission_app(
        pool.clone(),
        repository.clone(),
        &repository_name,
        calls.clone(),
        requests.clone(),
        "current-patch-fingerprint",
    )
    .await;
    let response = post_admission(app, &alias.value).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    wait_for_builder_call(&calls).await;

    let current_key = BuildKey::new(
        &repository_name,
        commit_sha,
        "current-patch-fingerprint",
        "test-toolchain",
        "x86_64-linux",
    )
    .unwrap();
    let current_build: CiBuild = repository.ensure_build(&current_key).await.unwrap().0;
    let persisted = repository
        .lookup_preview(&alias.value)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.id, old_preview.id);
    assert_eq!(persisted.build_id, old_build.id);
    assert_eq!(persisted.state, CiPreviewState::Queued);
    assert_eq!(requests.lock().unwrap().as_slice(), &[current_key]);
    assert_eq!(current_build.stage, CiBuildStage::Queued);

    let current_build = repository
        .transition_build(current_build.id, CiBuildStage::Fetching, None)
        .await
        .unwrap();
    let current_build = repository
        .transition_build(current_build.id, CiBuildStage::Patching, None)
        .await
        .unwrap();
    let current_build = repository
        .transition_build(current_build.id, CiBuildStage::Compiling, None)
        .await
        .unwrap();
    let current_build = repository
        .transition_build(current_build.id, CiBuildStage::Packaging, None)
        .await
        .unwrap();
    let current_build = repository
        .transition_build(current_build.id, CiBuildStage::Ready, None)
        .await
        .unwrap();
    repository
        .attach_artifact(current_build.id, "sha256:current", "ci/current.tar", 1)
        .await
        .unwrap();
    repository
        .advance_stable_aliases_for_build(current_build.id, CI_ROOT_DOMAIN)
        .await
        .unwrap();
    let advanced = repository
        .lookup_preview(&alias.value)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(advanced.build_id, current_build.id);
    assert_eq!(advanced.state, CiPreviewState::Ready);

    pool.close().await;
}

#[tokio::test]
async fn running_pr_alias_is_not_rebound_to_a_new_patch_candidate() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
    let watched = repository
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let commit_sha = "0123456789abcdef0123456789abcdef01234567";
    sqlx::query(
        "INSERT INTO ci_refs(repository_id, kind, name, number, source_repository, commit_sha)
         VALUES($1, 'pull_request', 'pr-3866', 3866, 'Pumpkin-MC/Pumpkin', $2)",
    )
    .bind(watched.id)
    .bind(commit_sha)
    .execute(&pool)
    .await
    .unwrap();
    let old_key = BuildKey::new(
        &repository_name,
        commit_sha,
        "old-patch-fingerprint",
        "test-toolchain",
        "x86_64-linux",
    )
    .unwrap();
    let (old_build, _) = repository.ensure_build(&old_key).await.unwrap();
    for stage in [
        CiBuildStage::Fetching,
        CiBuildStage::Patching,
        CiBuildStage::Compiling,
        CiBuildStage::Packaging,
        CiBuildStage::Ready,
    ] {
        repository
            .transition_build(old_build.id, stage, None)
            .await
            .unwrap();
    }
    repository
        .attach_artifact(old_build.id, "sha256:running", "ci/running.tar", 1)
        .await
        .unwrap();
    let alias = CiHostname::for_ref(CiRefKind::PullRequest, "pr-3866", CI_ROOT_DOMAIN).unwrap();
    repository
        .ensure_preview(
            &repository_name,
            commit_sha,
            CiRefKind::PullRequest,
            Some("pr-3866"),
            &alias,
            old_build.id,
        )
        .await
        .unwrap();
    repository
        .advance_stable_aliases_for_build(old_build.id, CI_ROOT_DOMAIN)
        .await
        .unwrap();
    let ready = repository
        .lookup_preview(&alias.value)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready.state, CiPreviewState::Ready);
    let running = repository.mark_preview_running(ready.id).await.unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let app = admission_app(
        pool.clone(),
        repository.clone(),
        &repository_name,
        calls.clone(),
        requests.clone(),
        "current-patch-fingerprint",
    )
    .await;
    let _response = post_admission(app, &alias.value).await;
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;

    let persisted = repository
        .lookup_preview(&alias.value)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.id, running.id);
    assert_eq!(persisted.build_id, old_build.id);
    assert_eq!(persisted.state, CiPreviewState::Running);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(requests.lock().unwrap().is_empty());
    let candidate_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ci_builds WHERE repository=$1 AND commit_sha=$2 AND patch_fingerprint=$3",
    )
    .bind(&repository_name)
    .bind(commit_sha)
    .bind("current-patch-fingerprint")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(candidate_count, 0);

    pool.close().await;
}
