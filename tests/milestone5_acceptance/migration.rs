//! Upgrade coverage from the production migration-one snapshot.
//!
//! The fixture is intentionally data-bearing rather than a schema-only smoke
//! test: it represents the rows a live hoster can have when CI-preview
//! migrations are appended.  The test proves that migration one is immutable,
//! application rows survive every append-only upgrade, and the new CI/runtime
//! tables are available with their fail-closed constraints.

use std::{future::Future, path::Path, str::FromStr};

use sha2::{Digest, Sha384};
use sqlx::{
    Row,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

const PRODUCTION_MIGRATION_ONE_SHA384: &str = "dd990a0fc9afdbb38a3e9b212694cf7d92675733fccf696f05778840e72eaf801213c3384efe5474dd45f89c215b0509";

async fn with_disposable_database<F, Fut>(options: PgConnectOptions, test: F) -> anyhow::Result<()>
where
    F: FnOnce(PgConnectOptions) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone().database("postgres"))
        .await?;
    let database = format!("kurbis_m5_migration_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&admin)
        .await?;
    admin.close().await;

    let result = test(options.clone().database(&database)).await;
    let cleanup = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.database("postgres"))
        .await?;
    sqlx::query(&format!("DROP DATABASE IF EXISTS {database}"))
        .execute(&cleanup)
        .await?;
    cleanup.close().await;
    result
}

#[tokio::test]
async fn production_like_snapshot_upgrades_without_losing_runtime_rows_or_provenance() {
    let url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must point to disposable PostgreSQL for migration acceptance");
    let options = PgConnectOptions::from_str(&url).expect("valid TEST_DATABASE_URL");
    with_disposable_database(options, |options| async move {
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::raw_sql(include_str!("../../migrations/0001_fresh_schema.sql"))
            .execute(&pool)
            .await?;
        sqlx::raw_sql(include_str!("../fixtures/production_like_m5_snapshot.sql"))
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
            "INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time)
             VALUES (1,'fresh_schema',true,decode($1,'hex'),0)",
        )
        .bind(PRODUCTION_MIGRATION_ONE_SHA384)
        .execute(&pool)
        .await?;

        kurbis_hoster::db::migrate(&pool).await?;

        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&pool)
                .await?;
        assert!(versions.last().copied().unwrap_or_default() >= 17);
        assert_eq!(
            versions,
            (1..=versions.last().copied().unwrap_or_default()).collect::<Vec<_>>()
        );
        let checksum: Vec<u8> =
            sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version=1 AND success")
                .fetch_one(&pool)
                .await?;
        assert_eq!(hex(&checksum), PRODUCTION_MIGRATION_ONE_SHA384);

        let snapshot = sqlx::query(
            "SELECT s.owner_id, s.subdomain, s.current_snapshot_generation,
                    p.worker_id, p.generation, p.active, sn.complete
             FROM servers s
             JOIN placements p ON p.server_id=s.id
             JOIN snapshots sn ON sn.server_id=s.id AND sn.generation=s.current_snapshot_generation
             WHERE s.id='00000000-0000-0000-0000-000000000202'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            snapshot.get::<Uuid, _>("owner_id"),
            Uuid::parse_str("00000000-0000-0000-0000-000000000101")?
        );
        assert_eq!(snapshot.get::<String, _>("subdomain"), "production-world");
        assert_eq!(snapshot.get::<i64, _>("current_snapshot_generation"), 7);
        assert_eq!(
            snapshot.get::<Uuid, _>("worker_id"),
            Uuid::parse_str("00000000-0000-0000-0000-000000000303")?
        );
        assert_eq!(snapshot.get::<i64, _>("generation"), 12);
        assert!(snapshot.get::<bool, _>("active"));
        assert!(snapshot.get::<bool, _>("complete"));

        let worker_capabilities: (String, bool) = sqlx::query_as(
            "SELECT runtime_architecture, supports_ci_previews FROM workers
             WHERE id='00000000-0000-0000-0000-000000000303'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(worker_capabilities, ("unknown".to_owned(), false));

        for table in [
            "ci_watched_repositories",
            "ci_refs",
            "ci_builds",
            "ci_artifacts",
            "ci_preview_aliases",
            "ci_runtime_placements",
            "ci_world_snapshots",
            "ci_github_webhook_deliveries",
            "ci_build_admission_leases",
            "ci_github_rate_limits",
            "ci_check_run_tombstones",
        ] {
            let present: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM information_schema.tables
                 WHERE table_schema='public' AND table_name=$1)",
            )
            .bind(table)
            .fetch_one(&pool)
            .await?;
            assert!(present, "production upgrade must create {table}");
        }
        let legacy_table: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.tables
             WHERE table_schema='public' AND table_name='worker_snapshot_cache')",
        )
        .fetch_one(&pool)
        .await?;
        assert!(
            !legacy_table,
            "legacy snapshot cache must not be recreated by upgrade"
        );
        pool.close().await;
        Ok(())
    })
    .await
    .expect("production-like snapshot migration acceptance");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn migration_one_source_checksum_matches_the_production_attestation() {
    let source = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations/0001_fresh_schema.sql"),
    )
    .expect("read migration one");
    assert_eq!(
        format!("{:x}", Sha384::digest(source)),
        PRODUCTION_MIGRATION_ONE_SHA384
    );
}
