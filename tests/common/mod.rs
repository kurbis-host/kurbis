use kurbis_hoster::db;
use sqlx::PgPool;

pub async fn test_pool() -> PgPool {
    let url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must point to a disposable Postgres test database");
    let pool = db::connect(&url).await.expect("connect to test Postgres");
    db::migrate(&pool)
        .await
        .expect("apply test database migrations");
    pool
}
