mod common;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use common::{create_user, test_pool};
use futures::StreamExt;
use kurbis_hoster::{
    config::{CommonConfig, ControlPlaneConfig, OAuthConfig, OAuthProviderConfig},
    control_plane::{
        api::{AppState, router},
        lifecycle::{Lifecycle, healthy_worker_template, heartbeat_worker},
    },
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::Mutex;
use tower::ServiceExt;
use uuid::Uuid;

static DB_TEST_LOCK: Mutex<()> = Mutex::const_new(());

fn state(pool: PgPool) -> Arc<AppState> {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is configured");
    Arc::new(AppState {
        lifecycle: Lifecycle::new(pool.clone()),
        pool,
        config: ControlPlaneConfig {
            common: CommonConfig { database_url },
            listen: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            public_base_url: "https://panel.example.test".to_owned(),
            root_domain: "mc.example.test".to_owned(),
            internal_token: "api-v1-test-token".to_owned(),
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
    })
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

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
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
        .oneshot(request(Method::GET, "/api/v1/server", Some(&token), None))
        .await
        .unwrap();
    assert_eq!(json_body(empty).await, Value::Null);

    let subdomain = format!("api-{}", &owner_id.simple().to_string()[..12]);
    let created = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/server",
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
    assert_eq!(
        created["java_address"],
        format!("{subdomain}.mc.example.test")
    );
    assert_eq!(created["storage_limit_bytes"], 5_i64 * 1024 * 1024 * 1024);

    let isolated = app
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/v1/server",
            Some(&other_token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(json_body(isolated).await, Value::Null);

    let patched = app
        .clone()
        .oneshot(request(
            Method::PATCH,
            "/api/v1/server",
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
            "/api/v1/server/actions",
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
            &format!("/api/v1/server/logs?after_id={}&limit=1", log_ids[0]),
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
            "/api/v1/server/players",
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
            "/api/v1/server/whitelist",
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
            "/api/v1/server/whitelist",
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
            "/api/v1/server/activity?limit=8",
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
            "/api/v1/server/snapshots",
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
            "/api/v1/server/snapshots/1/restore",
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
            &format!("/api/v1/server/whitelist?player_id={player_id}"),
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

    let default_server = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/server", Some(&token), None))
        .await
        .unwrap();
    assert_eq!(
        json_body(default_server).await["id"],
        merged_world.to_string()
    );

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
                .uri("/api/v1/server/events")
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
