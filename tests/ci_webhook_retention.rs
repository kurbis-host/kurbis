use std::{str::FromStr, sync::Arc};

use kurbis_hoster::{
    ci::{CiGitHubPrDelivery, CiGitHubPrDeliveryResult, CiPreviewRepository},
    control_plane::maintenance::WorkerMaintenance,
    db,
};
use object_store::{ObjectStore, local::LocalFileSystem};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

#[tokio::test]
async fn webhook_retention_is_opt_in_bounded_and_preserves_pending_leases() {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must point to disposable local PostgreSQL");
    let options = PgConnectOptions::from_str(&database_url).expect("valid TEST_DATABASE_URL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone().database("postgres"))
        .await
        .expect("connect to local PostgreSQL admin database");
    let database = format!("kurbis_webhook_retention_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&admin)
        .await
        .expect("create disposable retention-test database");

    let result = verify_retention(options.clone().database(&database)).await;
    admin.close().await;

    let cleanup = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.database("postgres"))
        .await
        .expect("reconnect to local PostgreSQL admin database");
    sqlx::query(&format!("DROP DATABASE {database}"))
        .execute(&cleanup)
        .await
        .expect("drop disposable retention-test database");
    cleanup.close().await;
    result.expect("webhook retention assertions");
}

async fn verify_retention(options: PgConnectOptions) -> anyhow::Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await?;
    db::migrate(&pool).await?;
    let repository = CiPreviewRepository::new(pool.clone());
    let repository_name = format!("test-org/retention-{}", Uuid::new_v4().simple());
    let watched = repository
        .register_repository(&repository_name, "main")
        .await?;

    let old_processed = insert_delivery(&repository, &repository_name, 1).await?;
    sqlx::query(
        "UPDATE ci_github_webhook_deliveries
         SET processed_at = now() - interval '8 days'
         WHERE delivery_id = $1",
    )
    .bind(old_processed)
    .execute(&pool)
    .await?;
    let current_processed = insert_delivery(&repository, &repository_name, 2).await?;
    sqlx::query(
        "UPDATE ci_github_webhook_deliveries SET processed_at = now() WHERE delivery_id = $1",
    )
    .bind(current_processed)
    .execute(&pool)
    .await?;

    // Fill one full GC batch plus one extra old receipt to prove each pass is bounded.
    sqlx::query(
        "INSERT INTO ci_github_webhook_deliveries(
             delivery_id, repository_id, action, pull_request_number, source_repository,
             base_sha, head_sha, payload_digest, received_at, processed_at
         )
         SELECT md5(random()::text || clock_timestamp()::text || number::text)::uuid,
                $1, 'synchronize', number, $2,
                '0123456789abcdef0123456789abcdef01234567',
                'fedcba9876543210fedcba9876543210fedcba98',
                decode(repeat('ab', 32), 'hex'),
                now() - interval '8 days', now() - interval '8 days'
         FROM generate_series(1, 128) AS items(number)",
    )
    .bind(watched.id)
    .bind(&repository_name)
    .execute(&pool)
    .await?;

    let active_pending = insert_delivery(&repository, &repository_name, 3).await?;
    let active_lease_token = Uuid::new_v4();
    sqlx::query(
        "UPDATE ci_github_webhook_deliveries
         SET received_at = now() - interval '8 days', claim_token = $2,
             claim_expires_at = now() + interval '10 minutes', attempts = 4
         WHERE delivery_id = $1",
    )
    .bind(active_pending)
    .bind(active_lease_token)
    .execute(&pool)
    .await?;
    let active_lease_expiry: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "SELECT claim_expires_at FROM ci_github_webhook_deliveries WHERE delivery_id = $1",
    )
    .bind(active_pending)
    .fetch_one(&pool)
    .await?;

    let expired_pending = insert_delivery(&repository, &repository_name, 4).await?;
    let expired_lease_token = Uuid::new_v4();
    sqlx::query(
        "UPDATE ci_github_webhook_deliveries
         SET received_at = now() - interval '8 days', claim_token = $2,
             claim_expires_at = now() - interval '1 minute', attempts = 2
         WHERE delivery_id = $1",
    )
    .bind(expired_pending)
    .bind(expired_lease_token)
    .execute(&pool)
    .await?;

    let objects = tempfile::tempdir()?;
    let store: Arc<dyn ObjectStore> = Arc::new(LocalFileSystem::new_with_prefix(objects.path())?);
    let disabled = WorkerMaintenance::new(pool.clone(), store.clone())
        .with_github_webhook_enabled(false)
        .run_once()
        .await?;
    assert_eq!(disabled.github_webhook_processed_gc_deleted, 0);
    let retained_while_disabled: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ci_github_webhook_deliveries WHERE repository_id = $1",
    )
    .bind(watched.id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(retained_while_disabled, 132);

    let enabled = WorkerMaintenance::new(pool.clone(), store.clone())
        .with_github_webhook_enabled(true)
        .run_once()
        .await?;
    assert_eq!(enabled.github_webhook_processed_gc_deleted, 128);
    let old_processed_remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ci_github_webhook_deliveries
         WHERE repository_id = $1 AND processed_at < now() - interval '7 days'",
    )
    .bind(watched.id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(old_processed_remaining, 1);

    let current_duplicate = repository
        .record_github_pr_delivery(&webhook_delivery(&repository_name, current_processed, 2))
        .await?;
    assert_eq!(current_duplicate, CiGitHubPrDeliveryResult::Duplicate);

    let active_lease_after_gc: (Option<Uuid>, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as(
            "SELECT claim_token, claim_expires_at FROM ci_github_webhook_deliveries
             WHERE delivery_id = $1",
        )
        .bind(active_pending)
        .fetch_one(&pool)
        .await?;
    assert_eq!(active_lease_after_gc.0, Some(active_lease_token));
    assert_eq!(active_lease_after_gc.1, Some(active_lease_expiry));

    let expired_lease_after_gc: (Option<Uuid>, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as(
            "SELECT claim_token, claim_expires_at FROM ci_github_webhook_deliveries
             WHERE delivery_id = $1",
        )
        .bind(expired_pending)
        .fetch_one(&pool)
        .await?;
    assert_eq!(expired_lease_after_gc.0, Some(expired_lease_token));
    assert!(expired_lease_after_gc.1.unwrap() < chrono::Utc::now());

    let (active_is_claimable, expired_is_claimable): (bool, bool) = sqlx::query_as(
        "SELECT
             EXISTS(SELECT 1 FROM ci_github_webhook_deliveries
                    WHERE delivery_id = $1 AND processed_at IS NULL
                      AND next_attempt_at <= now()
                      AND (claim_expires_at IS NULL OR claim_expires_at <= now())),
             EXISTS(SELECT 1 FROM ci_github_webhook_deliveries
                    WHERE delivery_id = $2 AND processed_at IS NULL
                      AND next_attempt_at <= now()
                      AND (claim_expires_at IS NULL OR claim_expires_at <= now()))",
    )
    .bind(active_pending)
    .bind(expired_pending)
    .fetch_one(&pool)
    .await?;
    assert!(!active_is_claimable, "active claim lease remains fenced");
    assert!(
        expired_is_claimable,
        "expired pending work remains claimable"
    );

    let repeated = WorkerMaintenance::new(pool.clone(), store.clone())
        .with_github_webhook_enabled(true)
        .run_once()
        .await?;
    assert_eq!(repeated.github_webhook_processed_gc_deleted, 1);
    let idempotent = WorkerMaintenance::new(pool.clone(), store)
        .with_github_webhook_enabled(true)
        .run_once()
        .await?;
    assert_eq!(idempotent.github_webhook_processed_gc_deleted, 0);
    let old_processed_remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ci_github_webhook_deliveries
         WHERE repository_id = $1 AND processed_at < now() - interval '7 days'",
    )
    .bind(watched.id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(old_processed_remaining, 0);
    pool.close().await;
    Ok(())
}

async fn insert_delivery(
    repository: &CiPreviewRepository,
    repository_name: &str,
    pull_request_number: u64,
) -> anyhow::Result<Uuid> {
    let delivery_id = Uuid::new_v4();
    let result = repository
        .record_github_pr_delivery(&webhook_delivery(
            repository_name,
            delivery_id,
            pull_request_number,
        ))
        .await?;
    assert_eq!(result, CiGitHubPrDeliveryResult::Inserted);
    Ok(delivery_id)
}

fn webhook_delivery(
    repository_name: &str,
    delivery_id: Uuid,
    pull_request_number: u64,
) -> CiGitHubPrDelivery {
    CiGitHubPrDelivery {
        delivery_id,
        repository: repository_name.to_owned(),
        action: "synchronize".to_owned(),
        pull_request_number,
        source_repository: repository_name.to_owned(),
        base_sha: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        head_sha: "fedcba9876543210fedcba9876543210fedcba98".to_owned(),
        payload_digest: vec![u8::try_from(pull_request_number).unwrap(); 32],
    }
}
