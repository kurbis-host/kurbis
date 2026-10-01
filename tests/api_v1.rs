mod common;

use std::{sync::Arc, time::Duration};

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use common::{api_state, create_user, test_pool};
use futures::StreamExt;
use futures::future::join_all;
use kurbis_hoster::control_plane::{
    api::{AppState, router},
    lifecycle::{healthy_worker_template, heartbeat_worker},
};
use kurbis_hoster::worker::snapshot::physical_object_path;
use object_store::ObjectStore;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::Mutex;
use tower::ServiceExt;
use uuid::Uuid;

static DB_TEST_LOCK: Mutex<()> = Mutex::const_new(());

fn state(pool: PgPool) -> Arc<AppState> {
    api_state(
        pool,
        "api-v1-test-token",
        Arc::new(object_store::memory::InMemory::new()),
    )
}

async fn user_session(pool: &PgPool, status: &str) -> (Uuid, String) {
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

fn request(method: Method, uri: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
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

fn bearer_request(method: Method, uri: &str, token: &str, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap()
}

fn server_session_request(
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

fn internal_admission_request(
    hostname: &str,
    identity: Option<Value>,
    minecraft_player_id: Option<Uuid>,
) -> Request<Body> {
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
            })
            .to_string(),
        ))
        .unwrap()
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn register_worker_api_token(pool: &PgPool, worker_id: Uuid) -> String {
    let token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    sqlx::query(
        "INSERT INTO worker_auth.credentials(worker_id, api_token_hash, database_role)
         VALUES($1, $2, $3)",
    )
    .bind(worker_id)
    .bind(blake3::hash(token.as_bytes()).as_bytes().to_vec())
    .bind(format!("test_worker_{}", worker_id.simple()))
    .execute(pool)
    .await
    .unwrap();
    token
}

#[tokio::test]
async fn versioned_api_covers_panel_resources_and_lifecycle_boundaries() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, token) = user_session(&pool, "active").await;
    let (_, other_token) = user_session(&pool, "active").await;
    let worker_id = Uuid::new_v4();
    let worker = healthy_worker_template(
        worker_id,
        format!("api-v1-worker-{worker_id}"),
        "127.0.0.1".to_owned(),
        8 * 1024 * 1024 * 1024,
        40 * 1024 * 1024 * 1024,
        4000,
        34000,
        34100,
    );
    heartbeat_worker(&pool, &worker).await.unwrap();
    let app = router(state(pool.clone()));

    let anonymous = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/session", None, None))
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    let session = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/session", Some(&token), None))
        .await
        .unwrap();
    assert_eq!(session.status(), StatusCode::OK);
    let session = json_body(session).await;
    assert_eq!(session["user"]["account_status"], "active");
    assert_eq!(
        session["limits"]["memory_bytes"],
        2_i64 * 1024 * 1024 * 1024
    );
    assert_eq!(
        session["limits"]["storage_bytes"],
        5_i64 * 1024 * 1024 * 1024
    );
    assert_eq!(session["limits"]["idle_seconds"], 300);

    let empty = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/servers", Some(&token), None))
        .await
        .unwrap();
    assert_eq!(json_body(empty).await, json!([]));

    let subdomain = format!("api-{}", &owner_id.simple().to_string()[..12]);
    let created = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/servers",
            Some(&token),
            Some(json!({
                "subdomain": subdomain,
                "settings": {
                    "motd": "API integration",
                    "difficulty": "Normal",
                    "gamemode": "Survival",
                    "view_distance": 12,
                    "simulation_distance": 8,
                    "hardcore": false,
                    "whitelist": true
                }
            })),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    let server_id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
    assert_eq!(created["observed_state"], "sleeping");
    assert_eq!(created["is_default"], true);
    assert_eq!(
        created["java_address"],
        format!("{subdomain}.mc.example.test")
    );
    assert_eq!(created["storage_limit_bytes"], 5_i64 * 1024 * 1024 * 1024);

    let isolated = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}"),
            Some(&other_token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(isolated.status(), StatusCode::NOT_FOUND);

    let patched = app
        .clone()
        .oneshot(request(
            Method::PATCH,
            &format!("/api/v1/servers/{server_id}"),
            Some(&token),
            Some(json!({"settings":{"motd":"Changed through v1"}})),
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    assert_eq!(
        json_body(patched).await["settings"]["motd"],
        "Changed through v1"
    );

    let invalid_snapshot = app
        .clone()
        .oneshot(request(
            Method::POST,
            &format!("/api/v1/servers/{server_id}/actions"),
            Some(&token),
            Some(json!({"action":"snapshot"})),
        ))
        .await
        .unwrap();
    assert_eq!(invalid_snapshot.status(), StatusCode::CONFLICT);

    let mut log_ids = Vec::new();
    for line in ["first", "second", "third"] {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO server_logs(server_id, line) VALUES($1,$2) RETURNING id",
        )
        .bind(server_id)
        .bind(line)
        .fetch_one(&pool)
        .await
        .unwrap();
        log_ids.push(id);
    }
    let logs = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!(
                "/api/v1/servers/{server_id}/logs?after_id={}&limit=1",
                log_ids[0]
            ),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    let logs = json_body(logs).await;
    assert_eq!(logs["items"].as_array().unwrap().len(), 1);
    assert_eq!(logs["items"][0]["line"], "second");
    assert_eq!(logs["next_after_id"], log_ids[1]);

    let player_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO server_players(server_id, player_id, name, edition)
         VALUES($1,$2,'Alex','java')",
    )
    .bind(server_id)
    .bind(player_id)
    .execute(&pool)
    .await
    .unwrap();
    let players = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/players"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    let players = json_body(players).await;
    assert_eq!(players[0]["player_id"], player_id.to_string());
    assert_eq!(players[0]["edition"], "java");

    let whitelist = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/whitelist"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    let whitelist = json_body(whitelist).await;
    assert_eq!(whitelist["enabled"], true);
    assert_eq!(whitelist["revision"], 0);
    assert_eq!(whitelist["applied_revision"], 0);
    assert_eq!(whitelist["synchronized"], true);

    let added = app
        .clone()
        .oneshot(request(
            Method::POST,
            &format!("/api/v1/servers/{server_id}/whitelist"),
            Some(&token),
            Some(json!({"player_id":player_id,"name":"Alex"})),
        ))
        .await
        .unwrap();
    assert_eq!(added.status(), StatusCode::CREATED);
    let added = json_body(added).await;
    assert_eq!(added["revision"], 1);
    assert_eq!(added["applied_revision"], 0);
    assert_eq!(added["synchronized"], false);
    assert_eq!(added["entries"][0]["player_id"], player_id.to_string());

    let activity = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/activity?limit=8"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    let activity = json_body(activity).await;
    assert!(!activity.as_array().unwrap().is_empty());

    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete)
         VALUES($1,$2,1,'complete-one',10,'one','manual',true),
               ($3,$2,2,'incomplete-two',20,'two','manual',false)",
    )
    .bind(Uuid::new_v4())
    .bind(server_id)
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();
    let snapshots = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/snapshots"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    let snapshots = json_body(snapshots).await;
    assert_eq!(snapshots.as_array().unwrap().len(), 1);
    assert_eq!(snapshots[0]["generation"], 1);

    let restore = app
        .clone()
        .oneshot(request(
            Method::POST,
            &format!("/api/v1/servers/{server_id}/snapshots/1/restore"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(restore.status(), StatusCode::OK);
    assert_eq!(json_body(restore).await["observed_state"], "restoring");

    let delete_whitelist = app
        .clone()
        .oneshot(request(
            Method::DELETE,
            &format!("/api/v1/servers/{server_id}/whitelist?player_id={player_id}"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(delete_whitelist.status(), StatusCode::OK);
    assert!(
        json_body(delete_whitelist).await["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    sqlx::query(
        "DELETE FROM users WHERE id IN (
             SELECT user_id FROM identities WHERE provider = 'google' AND subject LIKE 'api-v1-test-%'
         )",
    )
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers WHERE id = $1")
        .bind(worker_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn account_can_switch_default_between_merged_worlds_without_cross_owner_access() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, token) = user_session(&pool, "active").await;
    let (other_owner_id, _) = user_session(&pool, "active").await;
    let suffix = Uuid::new_v4().simple().to_string();
    let browser_world = Uuid::new_v4();
    let merged_world = Uuid::new_v4();
    let other_world = Uuid::new_v4();
    for (server_id, owner, label) in [
        (browser_world, owner_id, "browser"),
        (merged_world, owner_id, "merged"),
        (other_world, other_owner_id, "other"),
    ] {
        sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1,$2,$3)")
            .bind(server_id)
            .bind(owner)
            .bind(format!("world-{label}-{}", &suffix[..8]))
            .execute(&pool)
            .await
            .unwrap();
    }
    let app = router(state(pool.clone()));

    let account = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/account", Some(&token), None))
        .await
        .unwrap();
    assert_eq!(account.status(), StatusCode::OK);
    let account = json_body(account).await;
    assert!(
        account["identities"][0]["id"].is_string(),
        "account identities must include their IDs"
    );
    let worlds = account["worlds"].as_array().unwrap();
    assert_eq!(worlds.len(), 2);
    assert_eq!(
        worlds
            .iter()
            .filter(|world| world["is_default"] == true)
            .count(),
        1
    );
    assert!(
        worlds
            .iter()
            .any(|world| world["id"] == browser_world.to_string())
    );
    assert!(
        worlds
            .iter()
            .any(|world| world["id"] == merged_world.to_string())
    );
    assert!(
        worlds.iter().any(|world| {
            world["is_default"] == true && world["id"] == browser_world.to_string()
        })
    );

    let switched = app
        .clone()
        .oneshot(request(
            Method::PUT,
            &format!("/api/v1/account/worlds/{merged_world}/default"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(switched.status(), StatusCode::NO_CONTENT);

    let default_servers = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/servers", Some(&token), None))
        .await
        .unwrap();
    let default_servers = json_body(default_servers).await;
    assert!(default_servers.as_array().unwrap().iter().any(|server| {
        server["id"] == merged_world.to_string() && server["is_default"] == true
    }));

    let foreign_selection = app
        .clone()
        .oneshot(request(
            Method::PUT,
            &format!("/api/v1/account/worlds/{other_world}/default"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(foreign_selection.status(), StatusCode::NOT_FOUND);
    let default_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM servers WHERE owner_id=$1 AND is_default")
            .bind(owner_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(default_count, 1);

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind([owner_id, other_owner_id].as_slice())
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn world_exports_require_a_retained_source_and_enforce_ownership_cancellation_and_expiry() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, token) = user_session(&pool, "active").await;
    let (_, other_token) = user_session(&pool, "active").await;
    let server_id = Uuid::new_v4();
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1,$2,$3)")
        .bind(server_id)
        .bind(owner_id)
        .bind(format!("export-{}", &server_id.simple().to_string()[..12]))
        .execute(&pool)
        .await
        .unwrap();
    let app_state = state(pool.clone());
    let object_store = app_state.object_store.clone();
    let app = router(app_state);
    let endpoint = format!("/api/v1/servers/{server_id}/exports");

    let uninitialized = app
        .clone()
        .oneshot(request(Method::POST, &endpoint, Some(&token), None))
        .await
        .unwrap();
    assert_eq!(uninitialized.status(), StatusCode::CONFLICT);
    let export_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM world_exports WHERE server_id = $1")
            .bind(server_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(export_count, 0);

    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete)
         VALUES($1, $2, 1, 'export-test-manifest', 100, 'manifest-checksum', 'manual', true)",
    )
    .bind(Uuid::new_v4())
    .bind(server_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE servers SET current_snapshot_generation = 1 WHERE id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();

    let created = app
        .clone()
        .oneshot(request(Method::POST, &endpoint, Some(&token), None))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::ACCEPTED);
    let created = json_body(created).await;
    let export_id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
    assert_eq!(created["source_generation"], 1);
    assert_eq!(created["status"], "pending");

    let foreign = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("{endpoint}/{export_id}"),
            Some(&other_token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);

    let cancelled = app
        .clone()
        .oneshot(request(
            Method::DELETE,
            &format!("{endpoint}/{export_id}"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(json_body(cancelled).await["status"], "cancelled");

    let completed_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO world_exports(
            id, server_id, requested_by, status, stage, source_generation
         ) VALUES($1, $2, $3, 'pending', 'ready', 1)",
    )
    .bind(completed_id)
    .bind(server_id)
    .bind(owner_id)
    .execute(&pool)
    .await
    .unwrap();
    let key = format!("exports/{completed_id}/1.zip");
    let archive = Bytes::from_static(b"zip64 archive test fixture");
    object_store
        .put(
            &physical_object_path(server_id, &key).unwrap(),
            archive.clone().into(),
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE world_exports SET status = 'completed', stage = 'completed',
             archive_key = $2, archive_size = $3, archive_checksum = $4,
             expires_at = now() + interval '24 hours'
         WHERE id = $1",
    )
    .bind(completed_id)
    .bind(&key)
    .bind(archive.len() as i64)
    .bind(blake3::hash(&archive).to_hex().to_string())
    .execute(&pool)
    .await
    .unwrap();

    let download = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("{endpoint}/{completed_id}/download"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    assert_eq!(download.headers()[header::CONTENT_TYPE], "application/zip");
    assert_eq!(to_bytes(download.into_body(), 1024).await.unwrap(), archive);

    let foreign_download = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("{endpoint}/{completed_id}/download"),
            Some(&other_token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(foreign_download.status(), StatusCode::NOT_FOUND);

    sqlx::query("UPDATE world_exports SET expires_at = now() - interval '1 second' WHERE id = $1")
        .bind(completed_id)
        .execute(&pool)
        .await
        .unwrap();
    let expired = app
        .oneshot(request(
            Method::GET,
            &format!("{endpoint}/{completed_id}/download"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(expired.status(), StatusCode::CONFLICT);

    sqlx::query("DELETE FROM users WHERE id IN (SELECT user_id FROM identities WHERE subject LIKE 'api-v1-test-%')")
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn every_server_route_requires_ownership_and_legacy_singular_routes_are_gone() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, _) = user_session(&pool, "active").await;
    let (foreign_owner_id, foreign_token) = user_session(&pool, "active").await;
    let server_id = Uuid::new_v4();
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1,$2,$3)")
        .bind(server_id)
        .bind(owner_id)
        .bind(format!(
            "isolation-{}",
            &server_id.simple().to_string()[..12]
        ))
        .execute(&pool)
        .await
        .unwrap();

    let export_id = Uuid::new_v4();
    let player_id = Uuid::new_v4();
    let prefix = format!("/api/v1/servers/{server_id}");
    let app = router(state(pool.clone()));
    let protected_routes = [
        (Method::GET, prefix.clone(), None),
        (
            Method::PATCH,
            prefix.clone(),
            Some(json!({"settings":{"motd":"foreign"}})),
        ),
        (Method::DELETE, prefix.clone(), None),
        (
            Method::POST,
            format!("{prefix}/actions"),
            Some(json!({"action":"start"})),
        ),
        (
            Method::POST,
            format!("{prefix}/reset"),
            Some(json!({"mode":"world"})),
        ),
        (Method::GET, format!("{prefix}/events"), None),
        (Method::GET, format!("{prefix}/logs"), None),
        (Method::GET, format!("{prefix}/activity"), None),
        (Method::GET, format!("{prefix}/snapshots"), None),
        (Method::POST, format!("{prefix}/snapshots/1/restore"), None),
        (Method::GET, format!("{prefix}/exports"), None),
        (Method::POST, format!("{prefix}/exports"), None),
        (Method::GET, format!("{prefix}/exports/{export_id}"), None),
        (
            Method::DELETE,
            format!("{prefix}/exports/{export_id}"),
            None,
        ),
        (
            Method::GET,
            format!("{prefix}/exports/{export_id}/download"),
            None,
        ),
        (Method::GET, format!("{prefix}/players"), None),
        (Method::GET, format!("{prefix}/whitelist"), None),
        (
            Method::POST,
            format!("{prefix}/whitelist"),
            Some(json!({"player_id":player_id,"name":"Foreign"})),
        ),
        (
            Method::DELETE,
            format!("{prefix}/whitelist?player_id={player_id}"),
            None,
        ),
    ];

    for (method, uri, body) in protected_routes {
        let response = app
            .clone()
            .oneshot(request(method, &uri, Some(&foreign_token), body))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "foreign account must not access {uri}"
        );
    }

    for (method, uri) in [
        (Method::GET, "/api/v1/server"),
        (Method::POST, "/api/v1/server"),
        (Method::GET, "/api/v1/server/actions"),
        (Method::GET, "/api/v1/server/events"),
        (Method::GET, "/api/v1/server/logs"),
        (Method::GET, "/api/v1/server/activity"),
        (Method::GET, "/api/v1/server/snapshots"),
        (Method::POST, "/api/v1/server/snapshots/1/restore"),
        (Method::GET, "/api/v1/server/players"),
        (Method::GET, "/api/v1/server/whitelist"),
    ] {
        let response = app
            .clone()
            .oneshot(request(method, uri, Some(&foreign_token), None))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "legacy route {uri}"
        );
    }

    let (desired_state, whitelist_count, export_count): (String, i64, i64) = sqlx::query_as(
        "SELECT s.desired_state::text,
                (SELECT count(*) FROM whitelist_entries WHERE server_id = s.id),
                (SELECT count(*) FROM world_exports WHERE server_id = s.id)
         FROM servers s WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(desired_state, "stopped");
    assert_eq!(whitelist_count, 0);
    assert_eq!(export_count, 0);

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind([owner_id, foreign_owner_id].as_slice())
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn authenticated_admission_serializes_creation_redirects_and_keeps_existing_host_access() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let identity_id = Uuid::new_v4();
    let (owner_id, _) = user_session(&pool, "active").await;
    let java_uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO identities(id, user_id, provider, subject, display_name, minecraft_player_id)
         VALUES($1, $2, 'minecraft_java', $3, 'Admission Java', $4)",
    )
    .bind(identity_id)
    .bind(owner_id)
    .bind(java_uuid.to_string())
    .bind(java_uuid)
    .execute(&pool)
    .await
    .unwrap();

    let app = router(state(pool.clone()));
    let hosts = (0..8)
        .map(|_| format!("race-{}", &Uuid::new_v4().simple().to_string()[..12]))
        .collect::<Vec<_>>();
    let identity = json!({"kind":"java", "uuid":java_uuid});
    let responses = join_all(hosts.iter().map(|subdomain| {
        let app = app.clone();
        let identity = identity.clone();
        let hostname = format!("{subdomain}.mc.example.test");
        async move {
            app.oneshot(internal_admission_request(
                &hostname,
                Some(identity),
                Some(java_uuid),
            ))
            .await
            .unwrap()
        }
    }))
    .await;

    let mut admitted_server = None;
    let mut created_count = 0;
    let mut redirect_count = 0;
    for response in responses {
        assert_eq!(response.status(), StatusCode::OK);
        let result = json_body(response).await;
        let server_id = Uuid::parse_str(result["target"]["server_id"].as_str().unwrap()).unwrap();
        if let Some(expected) = admitted_server {
            assert_eq!(server_id, expected);
        } else {
            admitted_server = Some(server_id);
        }
        if result["created"] == true {
            created_count += 1;
        }
        if let Some(message) = result["private_message"].as_str() {
            redirect_count += 1;
            let address = result["target"]["subdomain"].as_str().unwrap();
            assert_eq!(
                message,
                format!(
                    "You already have a server at {address}.mc.example.test; connecting you there."
                )
            );
        }
    }
    assert_eq!(created_count, 1);
    assert_eq!(redirect_count, hosts.len() - 1);
    let server_id = admitted_server.unwrap();
    let (server_count, default_count): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE is_default)
         FROM servers WHERE owner_id = $1",
    )
    .bind(owner_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((server_count, default_count), (1, 1));

    let hostname: String = sqlx::query_scalar("SELECT subdomain FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let existing_guest = app
        .clone()
        .oneshot(internal_admission_request(
            &format!("{hostname}.mc.example.test"),
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(existing_guest.status(), StatusCode::OK);
    let existing_guest = json_body(existing_guest).await;
    assert_eq!(existing_guest["target"]["server_id"], server_id.to_string());
    assert_eq!(existing_guest["created"], false);

    let guest_unknown = app
        .clone()
        .oneshot(internal_admission_request(
            &format!(
                "guest-{}.mc.example.test",
                &Uuid::new_v4().simple().to_string()[..12]
            ),
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(guest_unknown.status(), StatusCode::NOT_FOUND);

    let forged_profile = app
        .clone()
        .oneshot(internal_admission_request(
            &format!(
                "forged-{}.mc.example.test",
                &Uuid::new_v4().simple().to_string()[..12]
            ),
            Some(identity),
            Some(Uuid::new_v4()),
        ))
        .await
        .unwrap();
    assert_eq!(forged_profile.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let (bedrock_owner, _) = user_session(&pool, "active").await;
    let xuid = format!("{}", Uuid::new_v4().as_u64_pair().0);
    let bedrock_profile = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO identities(id, user_id, provider, subject, display_name, minecraft_player_id)
         VALUES($1, $2, 'minecraft_bedrock', $3, 'Admission Bedrock', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(bedrock_owner)
    .bind(&xuid)
    .bind(bedrock_profile)
    .execute(&pool)
    .await
    .unwrap();
    let bedrock = app
        .clone()
        .oneshot(internal_admission_request(
            &format!(
                "bedrock-{}.mc.example.test",
                &Uuid::new_v4().simple().to_string()[..12]
            ),
            Some(json!({"kind":"bedrock", "xuid":xuid})),
            Some(bedrock_profile),
        ))
        .await
        .unwrap();
    assert_eq!(bedrock.status(), StatusCode::OK);

    let (mixed_owner, mixed_token) = user_session(&pool, "active").await;
    let mixed_java_uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO identities(id, user_id, provider, subject, display_name, minecraft_player_id)
         VALUES($1, $2, 'minecraft_java', $3, 'Mixed Race Java', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(mixed_owner)
    .bind(mixed_java_uuid.to_string())
    .bind(mixed_java_uuid)
    .execute(&pool)
    .await
    .unwrap();
    let mixed_identity = json!({"kind":"java", "uuid":mixed_java_uuid});
    let requests = (0..8).map(|index| {
        let app = app.clone();
        let token = mixed_token.clone();
        let identity = mixed_identity.clone();
        async move {
            let subdomain = format!(
                "mixed-{}-{}",
                index,
                &Uuid::new_v4().simple().to_string()[..8]
            );
            if index % 2 == 0 {
                app.oneshot(request(
                    Method::POST,
                    "/api/v1/servers",
                    Some(&token),
                    Some(json!({"subdomain":subdomain})),
                ))
                .await
                .unwrap()
            } else {
                app.oneshot(internal_admission_request(
                    &format!("{subdomain}.mc.example.test"),
                    Some(identity),
                    Some(mixed_java_uuid),
                ))
                .await
                .unwrap()
            }
        }
    });
    for response in join_all(requests).await {
        assert!(
            matches!(
                response.status(),
                StatusCode::CREATED | StatusCode::CONFLICT | StatusCode::OK
            ),
            "mixed web/admission request returned {}",
            response.status()
        );
    }
    let (mixed_server_count, mixed_default_count): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE is_default)
         FROM servers WHERE owner_id = $1",
    )
    .bind(mixed_owner)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((mixed_server_count, mixed_default_count), (1, 1));

    let (suspended_owner, _) = user_session(&pool, "suspended").await;
    let suspended_uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO identities(id, user_id, provider, subject, display_name, minecraft_player_id)
         VALUES($1, $2, 'minecraft_java', $3, 'Suspended Java', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(suspended_owner)
    .bind(suspended_uuid.to_string())
    .bind(suspended_uuid)
    .execute(&pool)
    .await
    .unwrap();
    let suspended_admission = app
        .clone()
        .oneshot(internal_admission_request(
            &format!(
                "suspended-{}.mc.example.test",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            Some(json!({"kind":"java", "uuid":suspended_uuid})),
            Some(suspended_uuid),
        ))
        .await
        .unwrap();
    assert_eq!(suspended_admission.status(), StatusCode::FORBIDDEN);
    let suspended_server_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM servers WHERE owner_id = $1")
            .bind(suspended_owner)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(suspended_server_count, 0);

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind([owner_id, bedrock_owner, mixed_owner, suspended_owner].as_slice())
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn sse_replays_durable_events_after_last_event_id() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, token) = user_session(&pool, "active").await;
    let server_id = Uuid::new_v4();
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1,$2,$3)")
        .bind(server_id)
        .bind(owner_id)
        .bind(format!("sse-{}", &server_id.simple().to_string()[..12]))
        .execute(&pool)
        .await
        .unwrap();
    let baseline: i64 =
        sqlx::query_scalar("SELECT max(id) FROM server_events WHERE server_id = $1")
            .bind(server_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let event_id: i64 = sqlx::query_scalar(
        "INSERT INTO server_events(server_id, event_type) VALUES($1,'players') RETURNING id",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let app = router(state(pool.clone()));
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/api/v1/servers/{server_id}/events"))
                .header(header::COOKIE, format!("kurbis_session={token}"))
                .header("last-event-id", baseline.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/event-stream"
    );
    let mut stream = response.into_body().into_data_stream();
    let frame = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("SSE catch-up frame should arrive")
        .expect("SSE stream should remain open")
        .expect("SSE frame should be readable");
    let text = String::from_utf8(frame.to_vec()).unwrap();
    assert!(text.contains(&format!("id:{event_id}")) || text.contains(&format!("id: {event_id}")));
    assert!(text.contains("event:players") || text.contains("event: players"));
    assert!(text.contains("\"type\":\"players\""));

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn current_placement_capability_and_redeemed_session_share_server_scope() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, owner_token) = user_session(&pool, "active").await;
    let (other_owner_id, other_owner_token) = user_session(&pool, "active").await;
    let app = router(state(pool.clone()));

    let suffix = Uuid::new_v4().simple().to_string();
    let created = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/servers",
            Some(&owner_token),
            Some(json!({
                "subdomain": format!("scope-{}", &suffix[..12]),
                "settings": {
                    "motd": "Scoped API",
                    "difficulty": "Normal",
                    "gamemode": "Survival",
                    "view_distance": 12,
                    "simulation_distance": 8,
                    "hardcore": false,
                    "whitelist": true
                }
            })),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let server_id = Uuid::parse_str(json_body(created).await["id"].as_str().unwrap()).unwrap();
    let other_created = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/servers",
            Some(&other_owner_token),
            Some(json!({
                "subdomain": format!("other-{}", &suffix[..12]),
                "settings": {
                    "motd": "Other server",
                    "difficulty": "Normal",
                    "gamemode": "Survival",
                    "view_distance": 12,
                    "simulation_distance": 8,
                    "hardcore": false,
                    "whitelist": true
                }
            })),
        ))
        .await
        .unwrap();
    assert_eq!(other_created.status(), StatusCode::CREATED);
    let other_server_id =
        Uuid::parse_str(json_body(other_created).await["id"].as_str().unwrap()).unwrap();

    let mut worker_id = Uuid::new_v4();
    let original_worker_id = worker_id;
    let worker = healthy_worker_template(
        worker_id,
        format!("scoped-api-worker-{worker_id}"),
        "127.0.0.1".to_owned(),
        8 * 1024 * 1024 * 1024,
        40 * 1024 * 1024 * 1024,
        4000,
        24000,
        24100,
    );
    heartbeat_worker(&pool, &worker).await.unwrap();
    let worker_api_token = register_worker_api_token(&pool, worker_id).await;
    sqlx::query("UPDATE servers SET generation = 1 WHERE id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO placements(server_id, worker_id, generation, lease_token,
                                java_port, bedrock_port, signal_port, active)
         VALUES($1, $2, 1, $3, 24000, 24001, 24002, true)",
    )
    .bind(server_id)
    .bind(worker_id)
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();

    let shared_token_response = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            "/internal/worker/server-capabilities",
            "api-v1-test-token",
            Some(json!({"server_id":server_id,"generation":1})),
        ))
        .await
        .unwrap();
    assert_eq!(shared_token_response.status(), StatusCode::UNAUTHORIZED);
    let spoofed_worker_response = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            "/internal/worker/server-capabilities",
            &worker_api_token,
            Some(json!({
                "server_id": server_id,
                "worker_identity": Uuid::new_v4(),
                "generation": 1
            })),
        ))
        .await
        .unwrap();
    assert!(spoofed_worker_response.status().is_client_error());

    let capability_response = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            "/internal/worker/server-capabilities",
            &worker_api_token,
            Some(json!({
                "server_id": server_id,
                "generation": 1
            })),
        ))
        .await
        .unwrap();
    assert_eq!(capability_response.status(), StatusCode::OK);
    assert_eq!(
        capability_response
            .headers()
            .get(header::CACHE_CONTROL)
            .unwrap(),
        "no-store"
    );
    let capability = json_body(capability_response).await["capability"]
        .as_str()
        .unwrap()
        .to_owned();

    let scoped_paths = [
        format!("/api/v1/servers/{server_id}"),
        format!("/api/v1/servers/{server_id}/players"),
        format!("/api/v1/servers/{server_id}/logs"),
        format!("/api/v1/servers/{server_id}/activity"),
        format!("/api/v1/servers/{server_id}/snapshots"),
        format!("/api/v1/servers/{server_id}/whitelist"),
    ];
    for path in scoped_paths {
        let response = app
            .clone()
            .oneshot(bearer_request(Method::GET, &path, &capability, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
    let events = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/events"),
            &capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);
    let first_event = tokio::time::timeout(
        Duration::from_secs(2),
        events.into_body().into_data_stream().next(),
    )
    .await
    .expect("server event stream should deliver the durable catch-up event")
    .expect("server event stream should remain open")
    .unwrap();
    assert!(String::from_utf8_lossy(&first_event).contains("event: server"));

    let patched = app
        .clone()
        .oneshot(bearer_request(
            Method::PATCH,
            &format!("/api/v1/servers/{server_id}"),
            &capability,
            Some(json!({"settings":{"motd":"Capability updated"}})),
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);

    let action = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            &format!("/api/v1/servers/{server_id}/actions"),
            &capability,
            Some(json!({"action":"snapshot"})),
        ))
        .await
        .unwrap();
    assert_eq!(action.status(), StatusCode::CONFLICT);

    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes,
                               checksum, reason, complete)
         VALUES($1, $2, 1, 'scoped-restore', 10, 'scoped-checksum', 'manual', true)",
    )
    .bind(Uuid::new_v4())
    .bind(server_id)
    .execute(&pool)
    .await
    .unwrap();
    let restored = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            &format!("/api/v1/servers/{server_id}/snapshots/1/restore"),
            &capability,
            None,
        ))
        .await
        .unwrap();
    // The route accepts the capability, then the lifecycle rejects the restore
    // because a live fenced placement must stop before snapshot recovery.
    assert_eq!(restored.status(), StatusCode::CONFLICT);

    let account_route = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            "/api/v1/servers",
            &capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(account_route.status(), StatusCode::UNAUTHORIZED);
    let cross_server = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/api/v1/servers/{other_server_id}/players"),
            &capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(cross_server.status(), StatusCode::UNAUTHORIZED);

    // The handler does not extract a body: a forged identifier may be sent but
    // cannot override scope, which must remain derived from the bearer token.
    let minted = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            "/api/v1/server/login-links",
            &capability,
            Some(json!({"server_id": other_server_id})),
        ))
        .await
        .unwrap();
    assert_eq!(minted.status(), StatusCode::OK);
    assert_eq!(
        minted.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let login_url = json_body(minted).await["url"].as_str().unwrap().to_owned();
    let login_token = login_url
        .split_once("#token=")
        .expect("login link includes its one-time fragment")
        .1
        .to_owned();

    let expiring_link = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            "/api/v1/server/login-links",
            &capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(expiring_link.status(), StatusCode::OK);
    let expiring_token = json_body(expiring_link).await["url"]
        .as_str()
        .unwrap()
        .split_once("#token=")
        .unwrap()
        .1
        .to_owned();
    sqlx::query(
        "UPDATE server_login_tokens SET expires_at = now() - interval '1 second'
         WHERE token_hash = $1",
    )
    .bind(blake3::hash(expiring_token.as_bytes()).as_bytes().to_vec())
    .execute(&pool)
    .await
    .unwrap();
    let expired_redemption = Request::builder()
        .method(Method::POST)
        .uri("/auth/server/redeem")
        .header(header::ORIGIN, "https://panel.example.test")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"token":&expiring_token}).to_string()))
        .unwrap();
    assert_eq!(
        app.clone()
            .oneshot(expired_redemption)
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    for attempt in 3..=6 {
        let response = app
            .clone()
            .oneshot(bearer_request(
                Method::POST,
                "/api/v1/server/login-links",
                &capability,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if attempt == 6 {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::OK
            },
            "login link mint attempt {attempt}"
        );
    }

    let cross_origin = Request::builder()
        .method(Method::POST)
        .uri("/auth/server/redeem")
        .header(header::ORIGIN, "https://attacker.example")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"token":&login_token}).to_string()))
        .unwrap();
    let rejected = app.clone().oneshot(cross_origin).await.unwrap();
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);

    let redemption = Request::builder()
        .method(Method::POST)
        .uri("/auth/server/redeem")
        .header(header::ORIGIN, "https://panel.example.test")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"token":&login_token}).to_string()))
        .unwrap();
    let redeemed = app.clone().oneshot(redemption).await.unwrap();
    assert_eq!(redeemed.status(), StatusCode::OK);
    assert_eq!(
        redeemed.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let server_session = redeemed
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("kurbis_server_session=")
        .unwrap()
        .to_owned();
    assert_eq!(
        json_body(redeemed).await["server_id"],
        server_id.to_string()
    );

    let session_access = app
        .clone()
        .oneshot(server_session_request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/players"),
            &server_session,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(session_access.status(), StatusCode::OK);
    let session_cross_server = app
        .clone()
        .oneshot(server_session_request(
            Method::GET,
            &format!("/api/v1/servers/{other_server_id}"),
            &server_session,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(session_cross_server.status(), StatusCode::UNAUTHORIZED);
    let session_account_route = app
        .clone()
        .oneshot(server_session_request(
            Method::GET,
            "/api/v1/servers",
            &server_session,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(session_account_route.status(), StatusCode::UNAUTHORIZED);

    sqlx::query(
        "UPDATE server_sessions SET expires_at = now() - interval '1 second'
         WHERE token_hash = $1",
    )
    .bind(blake3::hash(server_session.as_bytes()).as_bytes().to_vec())
    .execute(&pool)
    .await
    .unwrap();
    let expired_session = app
        .clone()
        .oneshot(server_session_request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/players"),
            &server_session,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(expired_session.status(), StatusCode::UNAUTHORIZED);

    let replay = Request::builder()
        .method(Method::POST)
        .uri("/auth/server/redeem")
        .header(header::ORIGIN, "https://panel.example.test")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"token":&login_token}).to_string()))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(replay).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );

    let relocated_worker_id = Uuid::new_v4();
    let relocated_worker = healthy_worker_template(
        relocated_worker_id,
        format!("scoped-api-relocated-worker-{relocated_worker_id}"),
        "127.0.0.1".to_owned(),
        8 * 1024 * 1024 * 1024,
        40 * 1024 * 1024 * 1024,
        4000,
        40000,
        40100,
    );
    heartbeat_worker(&pool, &relocated_worker).await.unwrap();
    let relocated_worker_api_token = register_worker_api_token(&pool, relocated_worker_id).await;
    sqlx::query("UPDATE placements SET worker_id = $2 WHERE server_id = $1")
        .bind(server_id)
        .bind(relocated_worker_id)
        .execute(&pool)
        .await
        .unwrap();

    let stale_link_after_relocation = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            "/api/v1/server/login-links",
            &capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        stale_link_after_relocation.status(),
        StatusCode::UNAUTHORIZED
    );

    let relocated_response = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            "/internal/worker/server-capabilities",
            &relocated_worker_api_token,
            Some(json!({
                "server_id": server_id,
                "generation": 1,
                "current_capability": capability
            })),
        ))
        .await
        .unwrap();
    assert_eq!(relocated_response.status(), StatusCode::OK);
    let relocated_capability = json_body(relocated_response).await["capability"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(relocated_capability, capability);
    worker_id = relocated_worker_id;

    sqlx::query("UPDATE servers SET generation = 2 WHERE id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE placements SET generation = 2 WHERE server_id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    let rotated_response = router(state(pool.clone()))
        .oneshot(bearer_request(
            Method::POST,
            "/internal/worker/server-capabilities",
            &relocated_worker_api_token,
            Some(json!({
                "server_id": server_id,
                "generation": 2,
                "current_capability": relocated_capability
            })),
        ))
        .await
        .unwrap();
    assert_eq!(rotated_response.status(), StatusCode::OK);
    let rotated_capability = json_body(rotated_response).await["capability"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(rotated_capability, capability);

    let stale_capability = router(state(pool.clone()))
        .oneshot(bearer_request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}"),
            &capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(stale_capability.status(), StatusCode::UNAUTHORIZED);
    let current_capability = router(state(pool.clone()))
        .oneshot(bearer_request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}"),
            &rotated_capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(current_capability.status(), StatusCode::OK);

    sqlx::query("UPDATE placements SET active = false WHERE server_id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    let inactive_capability = router(state(pool.clone()))
        .oneshot(bearer_request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}"),
            &rotated_capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(inactive_capability.status(), StatusCode::UNAUTHORIZED);
    let owner_access = router(state(pool.clone()))
        .oneshot(request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}"),
            Some(&owner_token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(owner_access.status(), StatusCode::OK);

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(vec![owner_id, other_owner_id])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers WHERE id = ANY($1)")
        .bind(vec![original_worker_id, worker_id])
        .execute(&pool)
        .await
        .unwrap();
}
