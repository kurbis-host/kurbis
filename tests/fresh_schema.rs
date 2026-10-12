use std::future::Future;
use std::path::Path;
use std::str::FromStr;

use serde_json::Value;
use sha2::{Digest, Sha256, Sha384};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use uuid::Uuid;

// SHA-384 of migration 0001 recorded in production (f6b05712 / deployment 1236700).
const PRODUCTION_MIGRATION_ONE_SHA384: &str = "dd990a0fc9afdbb38a3e9b212694cf7d92675733fccf696f05778840e72eaf801213c3384efe5474dd45f89c215b0509";
const MIGRATION_MANIFEST_SHA256: &str =
    "a3810200b3ba5615ca3270be897efbdabdf2a02978a5b83463d88ddd4b6e0dc0";
const LATEST_MIGRATION: i64 = 19;

#[test]
fn production_migration_one_checksum_is_immutable() {
    let source = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations/0001_fresh_schema.sql"),
    )
    .expect("read production migration one");
    let checksum = format!("{:x}", Sha384::digest(&source));
    assert_eq!(
        checksum, PRODUCTION_MIGRATION_ONE_SHA384,
        "migration 0001 is production state and must never be edited; append a new migration instead"
    );
}

#[test]
fn migration_manifest_is_canonical_and_append_only() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations/MANIFEST.json");
    let value: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read migration manifest"))
            .expect("parse migration manifest");
    assert_eq!(value["schema"], "kurbis.migration-manifest.v1");
    assert_eq!(value["manifest_sha256"], MIGRATION_MANIFEST_SHA256);
    let migrations = value["migrations"].as_array().expect("migration records");
    let versions: Vec<i64> = migrations
        .iter()
        .map(|record| record["version"].as_i64().expect("migration version"))
        .collect();
    assert_eq!(versions, (1..=LATEST_MIGRATION).collect::<Vec<_>>());
    let mut payload = value.as_object().expect("manifest object").clone();
    payload.remove("manifest_sha256");
    let canonical = format!(
        "{}\n",
        serde_json::to_string(&payload).expect("canonical manifest")
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(canonical.as_bytes())),
        MIGRATION_MANIFEST_SHA256
    );
    for record in migrations {
        let filename = record["filename"].as_str().expect("migration filename");
        let source = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("migrations")
                .join(filename),
        )
        .expect("read migration source");
        assert_eq!(format!("{:x}", Sha384::digest(source)), record["sha384"]);
    }
}

#[tokio::test]
async fn fresh_database_bootstraps_canonical_api_only_schema() {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must point to a disposable local PostgreSQL instance");
    let options = PgConnectOptions::from_str(&database_url).expect("valid TEST_DATABASE_URL");
    with_disposable_database(options, |options| async move {
        verify_fresh_database(options).await
    })
    .await
    .expect("canonical schema bootstrap assertions");
}

#[tokio::test]
async fn production_database_with_only_migration_one_upgrades_without_editing_baseline() {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must point to a disposable local PostgreSQL instance");
    let options = PgConnectOptions::from_str(&database_url).expect("valid TEST_DATABASE_URL");
    with_disposable_database(options, |options| async move {
        verify_production_upgrade(options).await
    })
    .await
    .expect("production migration upgrade assertions");
}

#[tokio::test]
async fn production_database_with_applied_history_runs_only_pending_migrations() {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must point to a disposable local PostgreSQL instance");
    let options = PgConnectOptions::from_str(&database_url).expect("valid TEST_DATABASE_URL");
    with_disposable_database(options, |options| async move {
        verify_applied_history_upgrade(options).await
    })
    .await
    .expect("production applied-history migration assertions");
}

async fn with_disposable_database<F, Fut>(options: PgConnectOptions, test: F) -> anyhow::Result<()>
where
    F: FnOnce(PgConnectOptions) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone().database("postgres"))
        .await?;
    let database = format!("kurbis_bootstrap_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&admin)
        .await?;
    admin.close().await;

    let result = test(options.clone().database(&database)).await;

    let cleanup = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.database("postgres"))
        .await?;
    sqlx::query(&format!("DROP DATABASE {database}"))
        .execute(&cleanup)
        .await?;
    cleanup.close().await;
    result
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
    assert_eq!(versions, (1..=LATEST_MIGRATION).collect::<Vec<_>>());

    assert_migration_one_checksum(&pool).await?;
    assert_schema_objects(&pool).await?;
    pool.close().await;
    Ok(())
}

async fn verify_production_upgrade(options: PgConnectOptions) -> anyhow::Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await?;
    sqlx::raw_sql(include_str!("../migrations/0001_fresh_schema.sql"))
        .execute(&pool)
        .await?;
    sqlx::query(
        "CREATE TABLE _sqlx_migrations (
            version BIGINT PRIMARY KEY,
            description TEXT NOT NULL,
            installed_on TIMESTAMPTZ NOT NULL DEFAULT now(),
            success BOOLEAN NOT NULL,
            checksum BYTEA NOT NULL,
            execution_time BIGINT NOT NULL
        )",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO _sqlx_migrations(version, description, success, checksum, execution_time)
         VALUES (1, 'fresh_schema', true, decode($1, 'hex'), 0)",
    )
    .bind(PRODUCTION_MIGRATION_ONE_SHA384)
    .execute(&pool)
    .await?;

    kurbis_hoster::db::migrate(&pool).await?;
    assert_migration_one_checksum(&pool).await?;
    assert_schema_objects(&pool).await?;
    pool.close().await;
    Ok(())
}

async fn verify_applied_history_upgrade(options: PgConnectOptions) -> anyhow::Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await?;
    let migrator =
        sqlx::migrate::Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
            .await?;
    for migration in migrator.iter().filter(|migration| migration.version < 17) {
        sqlx::raw_sql(migration.sql.as_ref()).execute(&pool).await?;
    }
    sqlx::query(
        "CREATE TABLE _sqlx_migrations (
            version BIGINT PRIMARY KEY, description TEXT NOT NULL,
            installed_on TIMESTAMPTZ NOT NULL DEFAULT now(), success BOOLEAN NOT NULL,
            checksum BYTEA NOT NULL, execution_time BIGINT NOT NULL
        )",
    )
    .execute(&pool)
    .await?;
    for migration in migrator.iter().filter(|migration| migration.version < 17) {
        sqlx::query(
            "INSERT INTO _sqlx_migrations(version, description, success, checksum, execution_time)
             VALUES ($1, $2, true, $3, 0)",
        )
        .bind(migration.version)
        .bind(migration.description.as_ref())
        .bind(migration.checksum.as_ref())
        .execute(&pool)
        .await?;
    }
    kurbis_hoster::db::migrate(&pool).await?;
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await?;
    assert_eq!(versions, (1..=LATEST_MIGRATION).collect::<Vec<_>>());
    assert_migration_one_checksum(&pool).await?;
    assert_schema_objects(&pool).await?;
    pool.close().await;
    Ok(())
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn assert_migration_one_checksum(pool: &PgPool) -> anyhow::Result<()> {
    let checksum: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 1 AND success")
            .fetch_one(pool)
            .await?;
    assert_eq!(hex_string(&checksum), PRODUCTION_MIGRATION_ONE_SHA384);
    Ok(())
}

async fn assert_schema_objects(pool: &PgPool) -> anyhow::Result<()> {
    let snapshot_gc_table: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables
         WHERE table_schema='public' AND table_name='ci_world_snapshot_gc')",
    )
    .fetch_one(pool)
    .await?;
    assert!(snapshot_gc_table);

    let poll_lease_columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'ci_watched_repositories'
           AND column_name IN ('poll_lease_token', 'poll_lease_expires_at')
         ORDER BY column_name",
    )
    .fetch_all(pool)
    .await?;
    assert_eq!(
        poll_lease_columns,
        ["poll_lease_expires_at", "poll_lease_token"]
    );

    let poll_lease_constraint: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pg_constraint
         WHERE conrelid = 'public.ci_watched_repositories'::regclass
           AND conname = 'ci_watched_repositories_poll_lease_check')",
    )
    .fetch_one(pool)
    .await?;
    assert!(
        poll_lease_constraint,
        "poll lease token and expiry must remain paired"
    );

    let retention_policy: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'ci_builds'
           AND column_name = 'retention_policy')",
    )
    .fetch_one(pool)
    .await?;
    assert!(
        retention_policy,
        "stable build retention migration must be present"
    );

    let requested_at_column: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'ci_builds'
           AND column_name = 'requested_at')",
    )
    .fetch_one(pool)
    .await?;
    assert!(
        requested_at_column,
        "CI queue demand marker must be migrated"
    );

    let build_queue_table: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables
         WHERE table_schema = 'public' AND table_name = 'ci_build_queue')",
    )
    .fetch_one(pool)
    .await?;
    assert!(
        build_queue_table,
        "fenced CI build queue migration must be present"
    );

    let expected_tables = [
        "worker_snapshot_hints",
        "ci_github_rate_limits",
        "operator_entries",
        "server_known_players",
        "console_commands",
        "server_runtime_stats",
        "ai_server_settings",
        "ai_conversations",
        "ai_messages",
        "ai_jobs",
        "ai_tool_calls",
        "ai_job_events",
        "ai_usage",
    ];
    for table in expected_tables {
        let present: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.tables
             WHERE table_schema = 'public' AND table_name = $1)",
        )
        .bind(table)
        .fetch_one(pool)
        .await?;
        assert!(present, "expected migrated table {table}");
    }

    let forbidden: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables
         WHERE table_schema IN ('public', 'worker_auth')
           AND table_name IN ('worker_snapshot_cache', 'credentials')",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(forbidden, 0);

    let worker_auth_schema: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname='worker_auth')")
            .fetch_one(pool)
            .await?;
    assert!(!worker_auth_schema);

    let role_column: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.columns
         WHERE table_schema='public' AND table_name='worker_credentials' AND column_name='database_role')",
    )
    .fetch_one(pool)
    .await?;
    assert!(!role_column);

    let lease_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns
         WHERE table_schema='public' AND table_name='worker_credentials'
           AND column_name IN ('worker_id', 'api_token_hash', 'lease_id', 'lease_expires_at', 'revoked_at')",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(lease_columns, 5);

    let rls_tables: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
         WHERE n.nspname='public' AND c.relkind='r' AND c.relrowsecurity",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(rls_tables, 0);

    let grant_expiry_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns
         WHERE table_schema='public' AND table_name IN ('server_session_grants', 'server_sessions')
           AND column_name='expires_at'",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(grant_expiry_columns, 0);
    Ok(())
}
