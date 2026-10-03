pub(crate) use std::{sync::Arc, time::Duration};

pub(crate) use crate::common::{api_state, create_user, test_pool};
pub(crate) use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
pub(crate) use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
pub(crate) use bytes::Bytes;
pub(crate) use futures::StreamExt;
pub(crate) use futures::future::join_all;
pub(crate) use kurbis_hoster::control_plane::{
    api::{AppState, router},
    lifecycle::{healthy_worker_template, heartbeat_worker},
};
pub(crate) use kurbis_hoster::snapshot::physical_object_path;
pub(crate) use object_store::ObjectStore;
pub(crate) use serde_json::{Value, json};
pub(crate) use sqlx::PgPool;
pub(crate) use tokio::sync::Mutex;
pub(crate) use tower::ServiceExt;
pub(crate) use uuid::Uuid;

pub(crate) static DB_TEST_LOCK: Mutex<()> = Mutex::const_new(());

pub(crate) fn state(pool: PgPool) -> Arc<AppState> {
    api_state(
        pool,
        "api-v1-test-token",
        Arc::new(object_store::memory::InMemory::new()),
    )
}

pub(crate) async fn user_session(pool: &PgPool, status: &str) -> (Uuid, String) {
    let identity_subject = Uuid::new_v4();
    let email = format!("{identity_subject}@example.test");
    let user_id = create_user(
        pool,
        "google",
        &format!("api-v1-test-{identity_subject}"),
        "API v1 Player",
        status,
        Some(&email),
    )
    .await;
    let token = format!("api-v1-session-{user_id}");
    sqlx::query("INSERT INTO sessions(id, token_hash, user_id) VALUES($1, $2, $3)")
        .bind(Uuid::new_v4())
        .bind(blake3::hash(token.as_bytes()).as_bytes().to_vec())
        .bind(user_id)
        .execute(pool)
        .await
        .unwrap();
    (user_id, token)
}

pub(crate) async fn create_test_server(pool: &PgPool, owner_id: Uuid, prefix: &str) -> Uuid {
    let server_id = Uuid::new_v4();
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
        .bind(server_id)
        .bind(owner_id)
        .bind(format!(
            "{prefix}-{}",
            &server_id.simple().to_string()[..12]
        ))
        .execute(pool)
        .await
        .unwrap();
    server_id
}

pub(crate) fn request(
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::COOKIE, format!("kurbis_session={token}"));
    }
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap()
}

pub(crate) fn request_with_cookies(
    method: Method,
    uri: &str,
    cookies: &str,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookies);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap()
}

pub(crate) fn bearer_request(
    method: Method,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("x-worker-lease", worker_lease_id(token).to_string());
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap()
}

pub(crate) fn worker_lease_id(token: &str) -> Uuid {
    Uuid::from_slice(&blake3::hash(token.as_bytes()).as_bytes()[..16]).unwrap()
}

pub(crate) fn server_session_request(
    method: Method,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, format!("kurbis_server_session={token}"));
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap()
}

pub(crate) fn internal_admission_request(
    hostname: &str,
    identity: Option<Value>,
    minecraft_player_id: Option<Uuid>,
) -> Request<Body> {
    let display_name = identity.as_ref().map(|_| "Admission Player");
    Request::builder()
        .method(Method::POST)
        .uri("/internal/admission")
        .header(header::AUTHORIZATION, "Bearer api-v1-test-token")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "hostname": hostname,
                "identity": identity,
                "minecraft_player_id": minecraft_player_id,
                "display_name": display_name,
            })
            .to_string(),
        ))
        .unwrap()
}

pub(crate) async fn json_body(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

pub(crate) async fn register_worker_api_token(pool: &PgPool, worker_id: Uuid) -> String {
    let token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    sqlx::query(
        "INSERT INTO worker_credentials(worker_id, api_token_hash, lease_id, lease_expires_at)
         VALUES($1, $2, $3, now() + interval '1 hour')",
    )
    .bind(worker_id)
    .bind(blake3::hash(token.as_bytes()).as_bytes().to_vec())
    .bind(worker_lease_id(&token))
    .execute(pool)
    .await
    .unwrap();
    token
}
