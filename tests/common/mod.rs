use kurbis_hoster::db;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn test_pool() -> PgPool {
    let url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must point to a disposable Postgres test database");
    let pool = db::connect(&url).await.expect("connect to test Postgres");
    db::migrate(&pool)
        .await
        .expect("apply test database migrations");
    pool
}

#[allow(dead_code)] // Shared by integration targets; each target uses a different fixture subset.
pub async fn create_user(
    pool: &PgPool,
    provider: &str,
    subject: &str,
    display_name: &str,
    account_status: &str,
    email: Option<&str>,
) -> Uuid {
    let user_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO users(id, display_name, account_status)
         VALUES($1, $2, $3)",
    )
    .bind(user_id)
    .bind(display_name)
    .bind(account_status)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO identities(id, user_id, provider, subject, display_name,
                                email, email_normalized, email_verified)
         VALUES($1, $2, $3, $4, $5, $6, lower(btrim($6)), $7)",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(provider)
    .bind(subject)
    .bind(display_name)
    .bind(email)
    .bind(email.is_some())
    .execute(pool)
    .await
    .unwrap();
    user_id
}
