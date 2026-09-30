use std::{net::SocketAddr, sync::Arc};

use super::events::{EventsQuery, event_cursor, next_event_row};
use super::internal::subdomain_from_hostname;
use super::types::{ServerSettings, ServerSettingsPatch};
use super::{AppState, router};
use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
};
use sqlx::PgPool;
use tokio::sync::Mutex;
use tower::ServiceExt;
use uuid::Uuid;

use crate::{
    config::{CommonConfig, ControlPlaneConfig, OAuthConfig},
    control_plane::lifecycle::Lifecycle,
    db,
};

static DB_TEST_LOCK: Mutex<()> = Mutex::const_new(());

async fn test_pool() -> PgPool {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must point to a disposable Postgres test database");
    let pool = db::connect(&database_url)
        .await
        .expect("connect to test Postgres");
    db::migrate(&pool)
        .await
        .expect("apply test database migrations");
    pool
}

fn test_state(pool: PgPool) -> Arc<AppState> {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is configured");
    Arc::new(AppState {
        lifecycle: Lifecycle::new(pool.clone()),
        pool,
        config: ControlPlaneConfig {
            common: CommonConfig { database_url },
            listen: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            public_base_url: "https://kurbis.example".to_owned(),
            root_domain: "mc.example".to_owned(),
            internal_token: "test-internal-token".to_owned(),
            oauth: OAuthConfig {
                provider: "test".to_owned(),
                client_id: "client".to_owned(),
                client_secret: "secret".to_owned(),
                authorize_url: "https://id.example/authorize".to_owned(),
                token_url: "https://id.example/token".to_owned(),
                userinfo_url: "https://id.example/userinfo".to_owned(),
                scopes: "openid".to_owned(),
            },
        },
        http: reqwest::Client::new(),
    })
}

async fn create_user_session(pool: &PgPool, status: &str) -> (Uuid, String) {
    let user_id = Uuid::new_v4();
    let token = format!("api-test-session-{user_id}");
    sqlx::query(
        "INSERT INTO users(id, oauth_provider, oauth_subject, display_name, account_status)
             VALUES($1, 'api-test', $2, 'API Test', $3)",
    )
    .bind(user_id)
    .bind(user_id.to_string())
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sessions(token_hash, user_id, expires_at)
             VALUES($1, $2, now() + interval '1 hour')",
    )
    .bind(blake3::hash(token.as_bytes()).as_bytes().to_vec())
    .bind(user_id)
    .execute(pool)
    .await
    .unwrap();
    (user_id, token)
}

async fn create_server(pool: &PgPool, owner_id: Uuid) -> Uuid {
    let server_id = Uuid::new_v4();
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
        .bind(server_id)
        .bind(owner_id)
        .bind(format!("api-{}", &server_id.simple().to_string()[..12]))
        .execute(pool)
        .await
        .unwrap();
    server_id
}

fn authenticated_request(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::COOKIE, format!("kurbis_session={token}"))
        .body(Body::empty())
        .unwrap()
}

#[test]
fn extracts_only_one_valid_label() {
    assert_eq!(
        subdomain_from_hostname("abc.play.example.com", "play.example.com"),
        Some("abc")
    );
    assert_eq!(
        subdomain_from_hostname("abc.def.play.example.com", "play.example.com"),
        None
    );
    assert_eq!(
        subdomain_from_hostname("play.example.com", "play.example.com"),
        None
    );
    assert_eq!(
        subdomain_from_hostname("AbC.PLAY.EXAMPLE.COM", "play.example.com"),
        Some("AbC")
    );
}

#[test]
fn settings_patch_preserves_unspecified_values() {
    let mut settings: ServerSettings = serde_json::from_value(serde_json::json!({
        "motd": "hello",
        "difficulty": "Normal",
        "gamemode": "Survival",
        "view_distance": 10,
        "simulation_distance": 8,
        "hardcore": false,
        "whitelist": false
    }))
    .unwrap();
    let patch: ServerSettingsPatch = serde_json::from_value(serde_json::json!({
        "view_distance": 16,
        "whitelist": true
    }))
    .unwrap();
    settings.merge(patch);
    assert_eq!(
        serde_json::to_value(settings).unwrap(),
        serde_json::json!({
            "motd": "hello",
            "difficulty": "Normal",
            "gamemode": "Survival",
            "view_distance": 16,
            "simulation_distance": 8,
            "hardcore": false,
            "whitelist": true
        })
    );
}

#[test]
fn last_event_id_takes_precedence_over_query_cursor() {
    let mut headers = HeaderMap::new();
    headers.insert("last-event-id", HeaderValue::from_static("42"));
    let query = EventsQuery { after_id: Some(7) };
    assert_eq!(event_cursor(&headers, &query).unwrap(), 42);
}

#[test]
fn event_cursor_rejects_negative_values() {
    let headers = HeaderMap::new();
    let query = EventsQuery { after_id: Some(-1) };
    assert!(event_cursor(&headers, &query).is_err());
}

#[tokio::test]
async fn suspended_session_is_visible_but_control_plane_access_is_forbidden() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (user_id, token) = create_user_session(&pool, "suspended").await;
    let app = router(test_state(pool.clone()));

    let session_response = app
        .clone()
        .oneshot(authenticated_request("/api/v1/session", &token))
        .await
        .unwrap();
    assert_eq!(session_response.status(), StatusCode::OK);

    let server_response = app
        .clone()
        .oneshot(authenticated_request("/api/v1/server", &token))
        .await
        .unwrap();
    assert_eq!(server_response.status(), StatusCode::FORBIDDEN);

    let legacy_response = app
        .oneshot(authenticated_request("/api/me", &token))
        .await
        .unwrap();
    assert_eq!(legacy_response.status(), StatusCode::NOT_FOUND);

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn server_read_is_scoped_to_the_authenticated_owner() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_a, token_a) = create_user_session(&pool, "active").await;
    let (owner_b, _) = create_user_session(&pool, "active").await;
    create_server(&pool, owner_b).await;
    let app = router(test_state(pool.clone()));

    let response = app
        .oneshot(authenticated_request("/api/v1/server", &token_a))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(&body[..], b"null");

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(vec![owner_a, owner_b])
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn durable_event_catch_up_enforces_cursor_owner_and_suspension() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner, _) = create_user_session(&pool, "active").await;
    let (other_owner, _) = create_user_session(&pool, "active").await;
    let server_id = create_server(&pool, owner).await;
    let baseline: i64 =
        sqlx::query_scalar("SELECT max(id) FROM server_events WHERE server_id = $1")
            .bind(server_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let first: i64 = sqlx::query_scalar(
        "INSERT INTO server_events(server_id, event_type)
             VALUES($1, 'log') RETURNING id",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let second: i64 = sqlx::query_scalar(
        "INSERT INTO server_events(server_id, event_type)
             VALUES($1, 'players') RETURNING id",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let row = next_event_row(&pool, server_id, owner, baseline)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.id, first);
    let row = next_event_row(&pool, server_id, owner, first)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.id, second);
    assert!(
        next_event_row(&pool, server_id, other_owner, baseline)
            .await
            .unwrap()
            .is_none()
    );

    sqlx::query("UPDATE users SET account_status = 'suspended' WHERE id = $1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        next_event_row(&pool, server_id, owner, baseline)
            .await
            .unwrap()
            .is_none()
    );

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(vec![owner, other_owner])
        .execute(&pool)
        .await
        .unwrap();
}
