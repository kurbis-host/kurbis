use super::*;
use bytes::Bytes;

#[path = "polling/support.rs"]
mod support;
pub(crate) use support::*;

#[path = "polling/admission.rs"]
mod admission;
#[path = "polling/lifecycle.rs"]
mod lifecycle;
#[path = "polling/provider.rs"]
mod provider;
#[path = "polling/retries.rs"]
mod retries;
#[path = "polling/webhooks.rs"]
mod webhooks;

#[tokio::test]
async fn main_only_mode_ignores_pull_request_heads_without_building_or_aliasing_them() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let repository = CiPreviewRepository::new(pool.clone());
    let watched = repository
        .register_repository(
            &format!("fixture/Pumpkin-main-only-{}", Uuid::new_v4()),
            "main",
        )
        .await
        .unwrap();
    let source = std::sync::Arc::new(PullRequestFixtureSource {
        fetches: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        head_sha: std::sync::Arc::new(std::sync::RwLock::new(
            "fedcba9876543210fedcba9876543210fedcba98".to_owned(),
        )),
    });
    let poller = kurbis_hoster::ci::CiPoller::new(repository.clone(), source).default_branch_only();

    poller.poll_once(watched.id).await.unwrap();

    let build_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ci_builds WHERE repository = (SELECT repository FROM ci_watched_repositories WHERE id = $1)",
    )
    .bind(watched.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let alias_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ci_preview_aliases WHERE repository = (SELECT repository FROM ci_watched_repositories WHERE id = $1)",
    )
    .bind(watched.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(build_count, 0);
    assert_eq!(alias_count, 0);
    pool.close().await;
}
