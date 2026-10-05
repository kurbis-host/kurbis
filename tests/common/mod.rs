use std::{net::SocketAddr, sync::Arc};

use kurbis_hoster::db;
use kurbis_hoster::{
    config::{CommonConfig, ControlPlaneConfig, OAuthConfig, OAuthProviderConfig},
    control_plane::{api::AppState, lifecycle::Lifecycle},
};
use object_store::ObjectStore;
use sqlx::PgPool;
use uuid::Uuid;

#[allow(dead_code)] // Shared by integration targets; each target uses a different fixture subset.
pub fn api_state(
    pool: PgPool,
    internal_token: &str,
    object_store: Arc<dyn ObjectStore>,
) -> Arc<AppState> {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is configured");
    Arc::new(AppState {
        ci_build_coordinator: None,
        ci_build_identity: None,
        lifecycle: Lifecycle::new(pool.clone()),
        pool,
        config: ControlPlaneConfig {
            common: CommonConfig { database_url },
            listen: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            public_base_url: "https://panel.example.test".to_owned(),
            root_domain: "mc.example.test".to_owned(),
            internal_token: internal_token.to_owned(),
            ci_github_webhook_enabled: true,
            ci_github_webhook_secret: Some(kurbis_hoster::config::SecretString::new(
                "test-webhook-secret",
            )),
            oauth: OAuthConfig {
                google: OAuthProviderConfig {
                    client_id: "google-client".to_owned(),
                    client_secret: "google-secret".to_owned(),
                    authorize_url: "https://identity.example.test/authorize".to_owned(),
                    token_url: "https://identity.example.test/token".to_owned(),
                    userinfo_url: "https://identity.example.test/userinfo".to_owned(),
                    scopes: "openid email profile".to_owned(),
                },
                discord: OAuthProviderConfig {
                    client_id: "discord-client".to_owned(),
                    client_secret: "discord-secret".to_owned(),
                    authorize_url: "https://identity.example.test/authorize".to_owned(),
                    token_url: "https://identity.example.test/token".to_owned(),
                    userinfo_url: "https://identity.example.test/userinfo".to_owned(),
                    scopes: "identify email".to_owned(),
                },
            },
        },
        http: reqwest::Client::new(),
        object_store,
    })
}

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
