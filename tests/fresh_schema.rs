use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

#[tokio::test]
async fn fresh_database_bootstraps_canonical_api_only_schema() {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must point to a disposable local PostgreSQL instance");
    let options = PgConnectOptions::from_str(&database_url).expect("valid TEST_DATABASE_URL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone().database("postgres"))
        .await
        .expect("connect to local PostgreSQL admin database");
    let database = format!("kurbis_bootstrap_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&admin)
        .await
        .expect("create disposable bootstrap database");

    let result = verify_fresh_database(options.clone().database(&database)).await;
    admin.close().await;

    let cleanup = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.database("postgres"))
        .await
        .expect("reconnect to local PostgreSQL admin database");
    sqlx::query(&format!("DROP DATABASE {database}"))
        .execute(&cleanup)
        .await
        .expect("drop disposable bootstrap database");
    cleanup.close().await;
    result.expect("canonical schema bootstrap assertions");
}

async fn verify_fresh_database(options: PgConnectOptions) -> anyhow::Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await?;
    kurbis_hoster::db::migrate(&pool).await?;

    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await?;
    assert_eq!(
        versions,
        vec![1],
        "fresh bootstrap must be one canonical migration"
    );

    let forbidden: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables
         WHERE table_schema IN ('public', 'worker_auth')
           AND table_name IN ('worker_snapshot_cache', 'credentials')",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        forbidden, 0,
        "legacy worker DB credential/cache tables must be absent"
    );

    let local_snapshot_hints: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables
         WHERE table_schema = 'public' AND table_name = 'worker_snapshot_hints')",
    )
    .fetch_one(&pool)
    .await?;
    assert!(
        local_snapshot_hints,
        "the control plane owns generation-scoped worker cache hints"
    );

    let worker_auth_schema: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname='worker_auth')")
            .fetch_one(&pool)
            .await?;
    assert!(
        !worker_auth_schema,
        "worker database role schema must not be created"
    );
    let role_column: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.columns
         WHERE table_schema='public' AND table_name='worker_credentials' AND column_name='database_role')",
    )
    .fetch_one(&pool)
    .await?;
    assert!(
        !role_column,
        "worker API credentials cannot carry a PostgreSQL principal"
    );

    let lease_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns
         WHERE table_schema='public' AND table_name='worker_credentials'
           AND column_name IN ('worker_id', 'api_token_hash', 'lease_id', 'lease_expires_at', 'revoked_at')",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(lease_columns, 5);

    let rls_tables: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
         WHERE n.nspname='public' AND c.relkind='r' AND c.relrowsecurity",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        rls_tables, 0,
        "worker isolation must be enforced by the control-plane API"
    );

    let grant_expiry_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns
         WHERE table_schema='public' AND table_name IN ('server_session_grants', 'server_sessions')
           AND column_name='expires_at'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        grant_expiry_columns, 0,
        "server-scoped browser grants are permanent until explicit revoke"
    );

    pool.close().await;
    Ok(())
}
