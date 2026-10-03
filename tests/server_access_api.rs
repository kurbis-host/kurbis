mod common;

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use common::{api_state, create_user, test_pool};
use futures::{StreamExt, future::join_all};
use kurbis_hoster::control_plane::api::router;
use kurbis_hoster::snapshot::physical_object_path;
use object_store::{ObjectStore, PutPayload, memory::InMemory};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::Mutex;
use tower::ServiceExt;
use uuid::Uuid;

static DB_TEST_LOCK: Mutex<()> = Mutex::const_new(());

const PANEL_ORIGIN: &str = "https://panel.example.test";

#[derive(Clone, Copy)]
enum ServerCredential<'a> {
    Session(&'a str),
    Plugin(&'a str),
}

struct ApiFixture {
    pool: PgPool,
    app: Router,
    store: Arc<InMemory>,
    owner_id: Uuid,
    other_owner_id: Uuid,
    server_id: Uuid,
    other_server_id: Uuid,
    worker_id: Uuid,
    capability: String,
}

struct ServerData {
    whitelist_player_id: Uuid,
    pending_export_id: Uuid,
    completed_export_id: Uuid,
    snapshot_generation: i64,
}

impl ApiFixture {
    async fn new() -> Self {
        let pool = test_pool().await;
        let store = Arc::new(InMemory::new());
        let owner_id = new_owner(&pool).await;
        let other_owner_id = new_owner(&pool).await;
        let server_id = new_server(&pool, owner_id, "source").await;
        let other_server_id = new_server(&pool, other_owner_id, "target").await;
        let worker_id = Uuid::new_v4();
        let capability = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());

        sqlx::query(
            "INSERT INTO workers(id, name, address, signal_address, total_memory_bytes,
                                total_disk_bytes, total_cpu_millis, port_min, port_max)
             VALUES($1, $2, '127.0.0.1', '127.0.0.1', 8589934592,
                    42949672960, 4000, 24000, 24100)",
        )
        .bind(worker_id)
        .bind(format!("server-access-{worker_id}"))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE servers SET generation = 1 WHERE id = $1")
            .bind(server_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO placements(server_id, worker_id, generation, lease_token,
                                    java_port, bedrock_port, active, signal_port)
             VALUES($1, $2, 1, $3, 24000, 24001, true, 24002)",
        )
        .bind(server_id)
        .bind(worker_id)
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO server_instance_capabilities(server_id, worker_id,
                                                       placement_generation, token_hash)
             VALUES($1, $2, 1, $3)",
        )
        .bind(server_id)
        .bind(worker_id)
        .bind(token_hash(&capability))
        .execute(&pool)
        .await
        .unwrap();

        let state = api_state(pool.clone(), "server-access-test-token", store.clone());

        Self {
            pool,
            app: router(state),
            store,
            owner_id,
            other_owner_id,
            server_id,
            other_server_id,
            worker_id,
            capability,
        }
    }

    async fn mint_link_token(&self) -> String {
        let response = self
            .app
            .clone()
            .oneshot(scoped_request(
                Method::POST,
                "/api/v1/server/login-links",
                ServerCredential::Plugin(&self.capability),
                Some(json!({"server_id":self.other_server_id})),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["expires_in_seconds"], 900);
        body["url"]
            .as_str()
            .unwrap()
            .split_once("#token=")
            .expect("login link contains token fragment")
            .1
            .to_owned()
    }

    async fn redeem(&self, token: &str) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(redeem_request(token))
            .await
            .unwrap()
    }

    async fn cleanup(self) {
        sqlx::query("DELETE FROM users WHERE id IN ($1, $2)")
            .bind(self.owner_id)
            .bind(self.other_owner_id)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM workers WHERE id = $1")
            .bind(self.worker_id)
            .execute(&self.pool)
            .await
            .unwrap();
    }
}

async fn new_owner(pool: &PgPool) -> Uuid {
    let suffix = Uuid::new_v4();
    create_user(
        pool,
        "google",
        &format!("server-access-{suffix}"),
        "Server access test",
        "active",
        None,
    )
    .await
}

async fn new_server(pool: &PgPool, owner_id: Uuid, label: &str) -> Uuid {
    let server_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO servers(id, owner_id, subdomain, settings)
         VALUES($1, $2, $3, $4)",
    )
    .bind(server_id)
    .bind(owner_id)
    .bind(format!("{label}-{}", &server_id.simple().to_string()[..12]))
    .bind(json!({"motd":"before","whitelist":true}))
    .execute(pool)
    .await
    .unwrap();
    server_id
}

fn token_hash(token: &str) -> Vec<u8> {
    blake3::hash(token.as_bytes()).as_bytes().to_vec()
}

fn scoped_request(
    method: Method,
    uri: &str,
    credential: ServerCredential<'_>,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    builder = match credential {
        ServerCredential::Session(token) => {
            builder.header(header::COOKIE, format!("kurbis_server_session={token}"))
        }
        ServerCredential::Plugin(token) => {
            builder.header(header::AUTHORIZATION, format!("Bearer {token}"))
        }
    };
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap()
}

fn redeem_request(token: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/auth/server/redeem")
        .header(header::ORIGIN, PANEL_ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"token":token}).to_string()))
        .unwrap()
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn seed_server_data(fixture: &ApiFixture, server_id: Uuid, requested_by: Uuid) -> ServerData {
    let player_id = Uuid::new_v4();
    sqlx::query("INSERT INTO server_events(server_id, event_type) VALUES($1, 'server')")
        .bind(server_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO server_logs(server_id, line) VALUES($1, 'private target log')")
        .bind(server_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO server_players(server_id, player_id, name, edition)
         VALUES($1, $2, 'PrivatePlayer', 'java')",
    )
    .bind(server_id)
    .bind(player_id)
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO whitelist_entries(server_id, player_id, name)
         VALUES($1, $2, 'PrivatePlayer')",
    )
    .bind(server_id)
    .bind(player_id)
    .execute(&fixture.pool)
    .await
    .unwrap();
    let checksum = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let manifest_key = format!("snapshots/1-{checksum}.json");
    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes,
                               checksum, reason, complete)
         VALUES($1, $2, 1, $3, 12, $4, 'manual', true)",
    )
    .bind(Uuid::new_v4())
    .bind(server_id)
    .bind(&manifest_key)
    .bind(checksum)
    .execute(&fixture.pool)
    .await
    .unwrap();

    let pending_export_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO world_exports(id, server_id, requested_by, status, stage, source_generation)
         VALUES($1, $2, $3, 'pending', 'requested', 1)",
    )
    .bind(pending_export_id)
    .bind(server_id)
    .bind(requested_by)
    .execute(&fixture.pool)
    .await
    .unwrap();
    let completed_export_id = Uuid::new_v4();
    let archive_key = format!("exports/{completed_export_id}/1.zip");
    let archive = Bytes::from_static(b"private archive");
    fixture
        .store
        .put(
            &physical_object_path(server_id, &archive_key).unwrap(),
            PutPayload::from(archive.clone()),
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO world_exports(id, server_id, requested_by, status, stage,
                                   source_generation, archive_key, archive_size,
                                   archive_checksum, expires_at)
         VALUES($1, $2, $3, 'completed', 'completed', 1, $4, $5,
                'private-archive-checksum', now() + interval '1 day')",
    )
    .bind(completed_export_id)
    .bind(server_id)
    .bind(requested_by)
    .bind(archive_key)
    .bind(archive.len() as i64)
    .execute(&fixture.pool)
    .await
    .unwrap();

    ServerData {
        whitelist_player_id: player_id,
        pending_export_id,
        completed_export_id,
        snapshot_generation: 1,
    }
}

struct RouteCase {
    method: Method,
    path: String,
    body: Option<Value>,
}

fn server_route_matrix(server_id: Uuid, data: &ServerData) -> Vec<RouteCase> {
    let prefix = format!("/api/v1/servers/{server_id}");
    vec![
        RouteCase {
            method: Method::GET,
            path: prefix.clone(),
            body: None,
        },
        RouteCase {
            method: Method::PATCH,
            path: prefix.clone(),
            body: Some(json!({"settings":{"motd":"stolen"}})),
        },
        RouteCase {
            method: Method::POST,
            path: format!("{prefix}/commands"),
            body: Some(json!({"command":"start"})),
        },
        RouteCase {
            method: Method::POST,
            path: format!("{prefix}/commands"),
            body: Some(json!({"command":"reset", "mode":"world"})),
        },
        RouteCase {
            method: Method::POST,
            path: format!("{prefix}/commands"),
            body: Some(json!({"command":"restore", "generation":data.snapshot_generation})),
        },
        RouteCase {
            method: Method::POST,
            path: format!("{prefix}/commands"),
            body: Some(json!({"command":"delete"})),
        },
        RouteCase {
            method: Method::GET,
            path: format!("{prefix}/events"),
            body: None,
        },
        RouteCase {
            method: Method::GET,
            path: format!("{prefix}/logs"),
            body: None,
        },
        RouteCase {
            method: Method::GET,
            path: format!("{prefix}/activity"),
            body: None,
        },
        RouteCase {
            method: Method::GET,
            path: format!("{prefix}/snapshots"),
            body: None,
        },
        RouteCase {
            method: Method::GET,
            path: format!("{prefix}/exports"),
            body: None,
        },
        RouteCase {
            method: Method::POST,
            path: format!("{prefix}/exports"),
            body: None,
        },
        RouteCase {
            method: Method::GET,
            path: format!("{prefix}/exports/{}", data.pending_export_id),
            body: None,
        },
        RouteCase {
            method: Method::DELETE,
            path: format!("{prefix}/exports/{}", data.pending_export_id),
            body: None,
        },
        RouteCase {
            method: Method::GET,
            path: format!("{prefix}/exports/{}/download", data.completed_export_id),
            body: None,
        },
        RouteCase {
            method: Method::GET,
            path: format!("{prefix}/players"),
            body: None,
        },
        RouteCase {
            method: Method::GET,
            path: format!("{prefix}/whitelist"),
            body: None,
        },
        RouteCase {
            method: Method::GET,
            path: format!("{prefix}/operators"),
            body: None,
        },
        RouteCase {
            method: Method::POST,
            path: format!("{prefix}/operators"),
            body: Some(json!({"name":"Unauthorized"})),
        },
        RouteCase {
            method: Method::DELETE,
            path: format!("{prefix}/operators?player_id={}", data.whitelist_player_id),
            body: None,
        },
        RouteCase {
            method: Method::POST,
            path: format!("{prefix}/whitelist"),
            body: Some(json!({"name":"Unauthorized"})),
        },
        RouteCase {
            method: Method::DELETE,
            path: format!("{prefix}/whitelist?player_id={}", data.whitelist_player_id),
            body: None,
        },
    ]
}

#[tokio::test]
async fn lifecycle_alias_routes_are_removed_in_favor_of_commands() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = ApiFixture::new().await;
    for path in [
        format!("/api/v1/servers/{}/actions", fixture.server_id),
        format!("/api/v1/servers/{}/reset", fixture.server_id),
        format!("/api/v1/servers/{}/snapshots/1/restore", fixture.server_id),
    ] {
        let response = fixture
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(&path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "legacy route remains available: {path}"
        );
    }
    fixture.cleanup().await;
}

async fn assert_denied_matrix(
    app: &Router,
    server_id: Uuid,
    data: &ServerData,
    credential: ServerCredential<'_>,
    label: &str,
) {
    for case in server_route_matrix(server_id, data) {
        let method = case.method.clone();
        let path = case.path.clone();
        let response = app
            .clone()
            .oneshot(scoped_request(method.clone(), &path, credential, case.body))
            .await
            .unwrap();
        assert!(
            matches!(
                response.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
            ),
            "{label} must not access {method} {path} (got {})",
            response.status()
        );
    }
}

async fn assert_account_apis_denied(
    app: &Router,
    credential: ServerCredential<'_>,
    server_id: Uuid,
    label: &str,
) {
    let suffix = Uuid::new_v4().simple().to_string();
    let cases = [
        (Method::GET, "/api/v1/session".to_owned(), None),
        (Method::GET, "/api/v1/account".to_owned(), None),
        (
            Method::PUT,
            format!("/api/v1/account/worlds/{server_id}/default"),
            None,
        ),
        (
            Method::DELETE,
            format!("/api/v1/account/sessions/{}", Uuid::new_v4()),
            None,
        ),
        (
            Method::POST,
            "/api/v1/servers".to_owned(),
            Some(json!({
                "subdomain": format!("acc-{}", &suffix[..12]),
                "settings": {
                    "motd":"Unauthorized",
                    "difficulty":"Normal",
                    "gamemode":"Survival",
                    "view_distance":12,
                    "simulation_distance":8,
                    "hardcore":false,
                    "whitelist":false
                }
            })),
        ),
    ];
    for (method, path, body) in cases {
        let response = app
            .clone()
            .oneshot(scoped_request(method, &path, credential, body))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{label} must not access account API {path}"
        );
    }
}

#[tokio::test]
async fn server_sessions_and_plugin_capabilities_are_confined_to_one_server() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = ApiFixture::new().await;
    let source_data = seed_server_data(&fixture, fixture.server_id, fixture.owner_id).await;
    let data = seed_server_data(&fixture, fixture.other_server_id, fixture.other_owner_id).await;
    let login_token = fixture.mint_link_token().await;
    let redeemed = fixture.redeem(&login_token).await;
    assert_eq!(redeemed.status(), StatusCode::OK);
    assert_eq!(
        redeemed.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let cookie = redeemed
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("Secure"));
    assert!(cookie.contains("SameSite=Lax"));
    assert!(cookie.contains("Max-Age=2147483647"));
    let redeemed_body = json_body(redeemed).await;
    assert_eq!(redeemed_body["server_id"], fixture.server_id.to_string());
    let session_token = cookie
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("kurbis_server_session=")
        .unwrap()
        .to_owned();

    let scoped_list = fixture
        .app
        .clone()
        .oneshot(scoped_request(
            Method::GET,
            "/api/v1/servers",
            ServerCredential::Session(&session_token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(scoped_list.status(), StatusCode::OK);
    let scoped_list = json_body(scoped_list).await;
    assert_eq!(scoped_list.as_array().unwrap().len(), 1);
    assert_eq!(scoped_list[0]["id"], fixture.server_id.to_string());

    for credential in [
        ServerCredential::Session(&session_token),
        ServerCredential::Plugin(&fixture.capability),
    ] {
        assert_denied_matrix(
            &fixture.app,
            fixture.other_server_id,
            &data,
            credential,
            "server-scoped credential",
        )
        .await;
        assert_account_apis_denied(
            &fixture.app,
            credential,
            fixture.other_server_id,
            "server-scoped credential",
        )
        .await;
    }

    for path in [
        format!("/api/v1/servers/{}", fixture.server_id),
        format!("/api/v1/servers/{}/logs", fixture.server_id),
        format!("/api/v1/servers/{}/activity", fixture.server_id),
        format!("/api/v1/servers/{}/snapshots", fixture.server_id),
        format!("/api/v1/servers/{}/exports", fixture.server_id),
        format!(
            "/api/v1/servers/{}/exports/{}",
            fixture.server_id, source_data.pending_export_id
        ),
        format!("/api/v1/servers/{}/players", fixture.server_id),
        format!("/api/v1/servers/{}/whitelist", fixture.server_id),
    ] {
        for credential in [
            ServerCredential::Session(&session_token),
            ServerCredential::Plugin(&fixture.capability),
        ] {
            let response = fixture
                .app
                .clone()
                .oneshot(scoped_request(Method::GET, &path, credential, None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
        }
    }

    let session_operators = fixture
        .app
        .clone()
        .oneshot(scoped_request(
            Method::GET,
            &format!("/api/v1/servers/{}/operators", fixture.server_id),
            ServerCredential::Session(&session_token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(session_operators.status(), StatusCode::OK);
    let plugin_operators = fixture
        .app
        .clone()
        .oneshot(scoped_request(
            Method::GET,
            &format!("/api/v1/servers/{}/operators", fixture.server_id),
            ServerCredential::Plugin(&fixture.capability),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(plugin_operators.status(), StatusCode::NOT_FOUND);
    let plugin_add_operator = fixture
        .app
        .clone()
        .oneshot(scoped_request(
            Method::POST,
            &format!("/api/v1/servers/{}/operators", fixture.server_id),
            ServerCredential::Plugin(&fixture.capability),
            Some(json!({"name":"PrivatePlayer"})),
        ))
        .await
        .unwrap();
    assert_eq!(plugin_add_operator.status(), StatusCode::NOT_FOUND);
    let plugin_remove_operator = fixture
        .app
        .clone()
        .oneshot(scoped_request(
            Method::DELETE,
            &format!(
                "/api/v1/servers/{}/operators?player_id={}",
                fixture.server_id, source_data.whitelist_player_id
            ),
            ServerCredential::Plugin(&fixture.capability),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(plugin_remove_operator.status(), StatusCode::NOT_FOUND);

    for credential in [
        ServerCredential::Session(&session_token),
        ServerCredential::Plugin(&fixture.capability),
    ] {
        let response = fixture
            .app
            .clone()
            .oneshot(scoped_request(
                Method::GET,
                &format!(
                    "/api/v1/servers/{}/exports/{}/download",
                    fixture.server_id, source_data.completed_export_id
                ),
                credential,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
            b"private archive"
        );
    }

    let events = fixture
        .app
        .clone()
        .oneshot(scoped_request(
            Method::GET,
            &format!("/api/v1/servers/{}/events", fixture.server_id),
            ServerCredential::Session(&session_token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);
    let event_chunk = tokio::time::timeout(
        Duration::from_secs(2),
        events.into_body().into_data_stream().next(),
    )
    .await
    .expect("authorized server event stream should emit durable history")
    .expect("server event stream should remain open")
    .unwrap();
    assert!(String::from_utf8_lossy(&event_chunk).contains("event: server"));

    let link_from_session = fixture
        .app
        .clone()
        .oneshot(scoped_request(
            Method::POST,
            "/api/v1/server/login-links",
            ServerCredential::Session(&session_token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(link_from_session.status(), StatusCode::UNAUTHORIZED);

    sqlx::query(
        "DELETE FROM server_sessions ss USING server_session_grants g
         WHERE ss.grant_id = g.id AND g.token_hash = $1 AND ss.server_id = $2",
    )
    .bind(token_hash(&session_token))
    .bind(fixture.server_id)
    .execute(&fixture.pool)
    .await
    .unwrap();
    let expired_session = fixture
        .app
        .clone()
        .oneshot(scoped_request(
            Method::GET,
            &format!("/api/v1/servers/{}", fixture.server_id),
            ServerCredential::Session(&session_token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(expired_session.status(), StatusCode::UNAUTHORIZED);

    sqlx::query("UPDATE users SET account_status = 'suspended' WHERE id = $1")
        .bind(fixture.owner_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    for credential in [
        ServerCredential::Session(&session_token),
        ServerCredential::Plugin(&fixture.capability),
    ] {
        let response = fixture
            .app
            .clone()
            .oneshot(scoped_request(
                Method::GET,
                &format!("/api/v1/servers/{}", fixture.server_id),
                credential,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "server access must be fenced when its owner is suspended"
        );
    }

    sqlx::query("UPDATE users SET account_status = 'active' WHERE id = $1")
        .bind(fixture.owner_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE servers SET desired_state = 'deleted' WHERE id = $1")
        .bind(fixture.server_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    for credential in [
        ServerCredential::Session(&session_token),
        ServerCredential::Plugin(&fixture.capability),
    ] {
        let response = fixture
            .app
            .clone()
            .oneshot(scoped_request(
                Method::GET,
                &format!("/api/v1/servers/{}", fixture.server_id),
                credential,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "deleted server credentials must be fenced"
        );
    }

    let state_before: (String, Value, i64, i64, i64) = sqlx::query_as(
        "SELECT desired_state::text, settings,
                (SELECT count(*) FROM operations WHERE server_id = $1),
                (SELECT count(*) FROM whitelist_entries WHERE server_id = $1),
                (SELECT count(*) FROM world_exports WHERE server_id = $1)
         FROM servers WHERE id = $1",
    )
    .bind(fixture.other_server_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(state_before.0, "stopped");
    assert_eq!(state_before.1["motd"], "before");
    assert_eq!(state_before.2, 0);
    assert_eq!(state_before.3, 1);
    assert_eq!(state_before.4, 2);
    let whitelist_entry_survived: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM whitelist_entries
            WHERE server_id = $1 AND player_id = $2 AND name = 'PrivatePlayer'
         )",
    )
    .bind(fixture.other_server_id)
    .bind(data.whitelist_player_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert!(whitelist_entry_survived);
    let pending_export_status: String =
        sqlx::query_scalar("SELECT status FROM world_exports WHERE id = $1 AND server_id = $2")
            .bind(data.pending_export_id)
            .bind(fixture.other_server_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let completed_export_status: String =
        sqlx::query_scalar("SELECT status FROM world_exports WHERE id = $1 AND server_id = $2")
            .bind(data.completed_export_id)
            .bind(fixture.other_server_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(pending_export_status, "pending");
    assert_eq!(completed_export_status, "completed");
    fixture.cleanup().await;
}

#[tokio::test]
async fn malformed_server_credentials_and_redemption_requests_are_rejected() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = ApiFixture::new().await;

    let malformed_token = "!".repeat(43);
    for token in ["", "short", malformed_token.as_str()] {
        let response = fixture
            .app
            .clone()
            .oneshot(scoped_request(
                Method::GET,
                &format!("/api/v1/servers/{}", fixture.server_id),
                ServerCredential::Session(token),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    for authorization in ["Bearer short", "bearer valid-token"] {
        let response = fixture
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/server/login-links")
                    .header(header::AUTHORIZATION, authorization)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    let invalid_tokens = ["short", malformed_token.as_str()];
    for token in invalid_tokens {
        let status = fixture.redeem(token).await.status();
        assert!(
            matches!(
                status,
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
            ),
            "expected BAD_REQUEST or UNPROCESSABLE_ENTITY, got {status}"
        );
    }
    let valid_shape_unknown_token = URL_SAFE_NO_PAD.encode([99_u8; 32]);
    assert_eq!(
        fixture.redeem(&valid_shape_unknown_token).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let extra_field = Request::builder()
        .method(Method::POST)
        .uri("/auth/server/redeem")
        .header(header::ORIGIN, PANEL_ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"token":valid_shape_unknown_token,"server_id":fixture.other_server_id})
                .to_string(),
        ))
        .unwrap();
    let extra_status = fixture
        .app
        .clone()
        .oneshot(extra_field)
        .await
        .unwrap()
        .status();
    assert!(
        matches!(
            extra_status,
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ),
        "expected BAD_REQUEST or UNPROCESSABLE_ENTITY, got {extra_status}"
    );
    let malformed_json = Request::builder()
        .method(Method::POST)
        .uri("/auth/server/redeem")
        .header(header::ORIGIN, PANEL_ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{\"token\":"))
        .unwrap();
    assert_eq!(
        fixture
            .app
            .clone()
            .oneshot(malformed_json)
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );

    let before =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM server_sessions WHERE server_id = $1")
            .bind(fixture.server_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(before, 0);
    fixture.cleanup().await;
}

#[tokio::test]
async fn concurrent_login_link_redemption_creates_exactly_one_session_cookie() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = ApiFixture::new().await;
    let token = fixture.mint_link_token().await;
    let requests = (0..12).map(|_| fixture.app.clone().oneshot(redeem_request(&token)));
    let responses = join_all(requests).await;
    let mut successes = 0;
    let mut cookies = 0;
    for response in responses {
        let response = response.unwrap();
        if response.status() == StatusCode::OK {
            successes += 1;
            cookies += response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .count();
        } else {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
    }
    assert_eq!(successes, 1);
    assert_eq!(cookies, 1);
    let session_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM server_sessions WHERE server_id = $1")
            .bind(fixture.server_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(session_count, 1);
    fixture.cleanup().await;
}

#[tokio::test]
async fn concurrent_plugin_login_link_mints_enforce_the_per_server_limit() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = ApiFixture::new().await;
    let requests = (0..24).map(|_| {
        fixture.app.clone().oneshot(scoped_request(
            Method::POST,
            "/api/v1/server/login-links",
            ServerCredential::Plugin(&fixture.capability),
            None,
        ))
    });
    let responses = join_all(requests).await;
    let mut created_tokens = Vec::new();
    let mut rate_limited = 0;
    for response in responses {
        let response = response.unwrap();
        match response.status() {
            StatusCode::OK => {
                let body = json_body(response).await;
                let login_url = body["url"].as_str().unwrap();
                assert!(login_url.starts_with("https://"));
                let token = login_url.split_once("#token=").unwrap().1.to_owned();
                assert_eq!(body["expires_in_seconds"], 900);
                created_tokens.push(token);
            }
            StatusCode::TOO_MANY_REQUESTS => rate_limited += 1,
            status => panic!("unexpected concurrent mint response: {status}"),
        }
    }
    assert_eq!(created_tokens.len(), 5);
    assert_eq!(rate_limited, 19);
    created_tokens.sort();
    created_tokens.dedup();
    assert_eq!(created_tokens.len(), 5);

    let stored_tokens: i64 =
        sqlx::query_scalar("SELECT count(*) FROM server_login_tokens WHERE server_id = $1")
            .bind(fixture.server_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(stored_tokens, 5);
    let request_count: i32 = sqlx::query_scalar(
        "SELECT request_count FROM server_login_rate_limits WHERE server_id = $1",
    )
    .bind(fixture.server_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(request_count, 6);
    fixture.cleanup().await;
}
