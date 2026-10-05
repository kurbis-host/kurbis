mod common;

#[path = "persistence/source_boundaries.rs"]
mod source_boundaries;

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use common::{api_state, create_user, test_pool};
use kurbis_hoster::{
    config::{CommonConfig, GatewayConfig},
    contracts::lifecycle::ServerCommandRequest,
    control_plane::api::router,
    control_plane::lifecycle::{
        Lifecycle, claim_operation, complete_start, healthy_worker_template, heartbeat_worker,
    },
    gateway::{routes::RouteCache, wake::request_wake},
    model::ObservedState,
};
use sqlx::PgPool;
use tokio::net::TcpListener;
use uuid::Uuid;

static DB_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[test]
fn gateway_routing_stays_behind_the_shared_repository_boundary() {
    source_boundaries::assert_rust_tree_excludes(
        "src/gateway",
        &["crate::control_plane", "control_plane::api", "sqlx::query"],
    );
    let routes = source_boundaries::rust_source("src/gateway/routes.rs");
    assert!(routes.contains("routing::RouteRepository"));
    assert!(routes.contains("repository: RouteRepository"));
}

async fn clean_routes(pool: &PgPool) {
    sqlx::query(
        "DELETE FROM users WHERE id IN (
             SELECT user_id FROM identities WHERE provider = 'google' AND subject LIKE 'route-test-%'
         )",
    )
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM server_instance_capabilities")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM placements")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers")
        .execute(pool)
        .await
        .unwrap();
}

async fn server(pool: &PgPool) -> (Uuid, Uuid, String) {
    let id = Uuid::new_v4();
    let subdomain = format!("route-{}", &id.simple().to_string()[..12]);
    let owner = create_user(
        pool,
        "google",
        &format!("route-test-{id}"),
        "Route test",
        "active",
        None,
    )
    .await;
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
        .bind(id)
        .bind(owner)
        .bind(&subdomain)
        .execute(pool)
        .await
        .unwrap();
    (owner, id, subdomain)
}

fn gateway_config(control_plane_url: String, internal_token: String) -> GatewayConfig {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is configured");
    GatewayConfig {
        common: CommonConfig { database_url },
        java_listen: "127.0.0.1:0".parse().unwrap(),
        java_public_port: 25565,
        bedrock_listen: "127.0.0.1:0".parse().unwrap(),
        bedrock_public_port: 19132,
        bedrock_public_address: "127.0.0.1".parse().unwrap(),
        bedrock_tls_certificate: PathBuf::new(),
        bedrock_tls_private_key: PathBuf::new(),
        waiting_java_address: "127.0.0.1:25566".parse().unwrap(),
        waiting_bedrock_address: "127.0.0.1:19133".parse().unwrap(),
        waiting_signal_address: "127.0.0.1:19134".parse().unwrap(),
        waiting_control_socket: PathBuf::new(),
        waiting_control_token: "test".into(),
        control_plane_url,
        internal_token,
        root_domain: "mc.example.test".into(),
        wake_timeout: Duration::from_secs(90),
        allow_offline_waiting_for_tests: false,
    }
}

#[tokio::test]
async fn gateway_join_starts_server_only_through_the_canonical_command_route() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_routes(&pool).await;
    let (owner, server_id, subdomain) = server(&pool).await;
    let token = "gateway-command-test-token";
    let app = router(api_state(
        pool.clone(),
        token,
        Arc::new(object_store::memory::InMemory::new()),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    let server_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = gateway_config(format!("http://{address}"), token.to_owned());

    request_wake(&config, server_id).await.unwrap();
    request_wake(&config, server_id).await.unwrap();

    let (state, desired, queued): (ObservedState, String, bool) = sqlx::query_as(
        "SELECT s.observed_state, s.desired_state::text,
                EXISTS(SELECT 1 FROM start_requests r WHERE r.server_id = s.id)
         FROM servers s WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (state, desired.as_str(), queued),
        (ObservedState::Queued, "running", true)
    );
    let request_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM start_requests WHERE server_id = $1")
            .bind(server_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(request_count, 1, "repeat joins share the queued start");

    let client = reqwest::Client::new();
    let forbidden = client
        .post(format!(
            "http://{address}/api/v1/servers/{server_id}/commands"
        ))
        .bearer_auth(token)
        .json(&ServerCommandRequest::Start {
            reason: Some("manual".into()),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    let removed_wake_route = client
        .post(format!(
            "http://{address}/internal/wake/{subdomain}.mc.example.test"
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(removed_wake_route.status(), reqwest::StatusCode::NOT_FOUND);

    server_task.abort();
    assert!(server_task.await.unwrap_err().is_cancelled());
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn suspended_owner_cannot_wake_or_block_other_queued_accounts() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_routes(&pool).await;
    let (owner, id, subdomain) = server(&pool).await;
    let lifecycle = Lifecycle::new(pool.clone());
    assert_eq!(
        lifecycle.request_start(id, "join").await.unwrap(),
        ObservedState::Queued
    );
    sqlx::query("UPDATE users SET account_status = 'suspended' WHERE id = $1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        lifecycle
            .request_start(id, "join")
            .await
            .unwrap_err()
            .to_string()
            .contains("suspended")
    );
    let routes = RouteCache::new(pool.clone(), "example.test".into());
    let target = routes
        .resolve(&format!("{subdomain}.example.test"))
        .await
        .unwrap()
        .unwrap();
    assert!(target.account_suspended);
    assert!(target.worker_address.is_none());
    assert_eq!(lifecycle.drain_queue_once().await.unwrap(), 0);
    let (state, generation, queued): (ObservedState, i64, bool) = sqlx::query_as(
        "SELECT observed_state, generation, EXISTS(SELECT 1 FROM start_requests WHERE server_id = $1) FROM servers WHERE id = $1")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(
        (state, generation, queued),
        (ObservedState::Sleeping, 0, false)
    );
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn route_notifications_cover_account_worker_and_server_changes() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_routes(&pool).await;
    let (owner, id, subdomain) = server(&pool).await;
    let routes = RouteCache::new(pool.clone(), "example.test".into());
    let host = format!("{subdomain}.example.test");
    // Subscribe before serving any cache reads, just as gateway startup does.
    let listener = routes.invalidation_listener().await.unwrap();
    let worker_id = Uuid::new_v4();
    heartbeat_worker(
        &pool,
        &healthy_worker_template(
            worker_id,
            format!("route-worker-{worker_id}"),
            "127.0.0.1".into(),
            2147483648,
            5368709120,
            1000,
            30000,
            30010,
        ),
    )
    .await
    .unwrap();
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(id, "test").await.unwrap();
    let op = claim_operation(&pool, worker_id).await.unwrap().unwrap();
    complete_start(&pool, &op, 0).await.unwrap();
    // Drain prior placement/server notifications so each next event proves
    // the corresponding mutation actually publishes its own signal.
    let mut barrier = routes.invalidation_listener().await.unwrap();
    for mutation in [
        "UPDATE users SET account_status = 'suspended' WHERE id = $1",
        "UPDATE users SET account_status = 'active' WHERE id = $1",
    ] {
        let before = routes.resolve(&host).await.unwrap().unwrap();
        sqlx::query(mutation)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), barrier.recv())
            .await
            .unwrap()
            .unwrap();
        routes.invalidate_all().await;
        let after = routes.resolve(&host).await.unwrap().unwrap();
        assert_ne!(before.account_suspended, after.account_suspended);
        assert_eq!(after.worker_address.is_none(), after.account_suspended);
    }
    sqlx::query("UPDATE workers SET address = '192.0.2.1' WHERE id = $1")
        .bind(worker_id)
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), barrier.recv())
        .await
        .unwrap()
        .unwrap();
    routes.invalidate_all().await;
    assert_eq!(
        routes
            .resolve(&host)
            .await
            .unwrap()
            .unwrap()
            .worker_address
            .as_deref(),
        Some("192.0.2.1")
    );
    sqlx::query("UPDATE workers SET heartbeat_at = now() - interval '31 seconds' WHERE id = $1")
        .bind(worker_id)
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), barrier.recv())
        .await
        .unwrap()
        .unwrap();
    routes.invalidate_all().await;
    let unavailable = routes.resolve(&host).await.unwrap().unwrap();
    assert_eq!(unavailable.worker_healthy, Some(false));
    assert!(unavailable.worker_address.is_none());
    sqlx::query("UPDATE servers SET desired_state = 'deleted' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), barrier.recv())
        .await
        .unwrap()
        .unwrap();
    routes.invalidate_all().await;
    assert!(routes.resolve(&host).await.unwrap().is_none());
    // Exercise the production invalidation task using the pre-subscribed connection.
    let watched = routes.clone();
    let task = tokio::spawn(async move { watched.listen_for_invalidations(listener).await });
    sqlx::query("UPDATE servers SET desired_state = 'running' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while routes.resolve(&host).await.unwrap().is_none() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner)
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
async fn cached_route_expires_at_the_worker_deadline_without_a_notification() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_routes(&pool).await;
    let (owner, id, subdomain) = server(&pool).await;
    let worker = Uuid::new_v4();
    heartbeat_worker(
        &pool,
        &healthy_worker_template(
            worker,
            format!("route-worker-{worker}"),
            "127.0.0.1".into(),
            2147483648,
            5368709120,
            1000,
            30100,
            30110,
        ),
    )
    .await
    .unwrap();
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(id, "test").await.unwrap();
    let operation = claim_operation(&pool, worker).await.unwrap().unwrap();
    complete_start(&pool, &operation, 0).await.unwrap();
    sqlx::query("UPDATE workers SET heartbeat_at = now() - interval '29 seconds' WHERE id = $1")
        .bind(worker)
        .execute(&pool)
        .await
        .unwrap();
    // No invalidation listener is running: the health deadline itself must
    // prevent the cache from extending a stale worker's routing authority.
    let routes = RouteCache::new(pool.clone(), "example.test".into());
    let host = format!("{subdomain}.example.test");
    let ready = routes.resolve(&host).await.unwrap().unwrap();
    assert_eq!(ready.worker_healthy, Some(true));
    assert!(ready.worker_address.is_some());
    let remaining = (ready.worker_valid_until.unwrap() - chrono::Utc::now())
        .to_std()
        .unwrap_or_default();
    tokio::time::sleep(remaining + std::time::Duration::from_millis(50)).await;
    let stale = routes.resolve(&host).await.unwrap().unwrap();
    assert_eq!(stale.worker_healthy, Some(false));
    assert!(stale.worker_address.is_none());
    let active: bool = sqlx::query_scalar("SELECT active FROM placements WHERE server_id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(active, "unreachable runtime remains fenced");
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers WHERE id = $1")
        .bind(worker)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn listener_reconnection_invalidates_routes_changed_during_a_notification_gap() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_routes(&pool).await;
    let (owner, _, subdomain) = server(&pool).await;
    let routes = RouteCache::new(pool.clone(), "example.test".into());
    let host = format!("{subdomain}.example.test");
    let mut listener = routes.invalidation_listener().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut listener)
        .await
        .unwrap();
    // Suppress actual subscription delivery while keeping PgListener's desired
    // channels, as during a network gap. Its reconnect will restore LISTEN.
    sqlx::query("UNLISTEN *")
        .execute(&mut listener)
        .await
        .unwrap();
    assert!(
        !routes
            .resolve(&host)
            .await
            .unwrap()
            .unwrap()
            .account_suspended
    );
    sqlx::query("UPDATE users SET account_status = 'suspended' WHERE id = $1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !routes
            .resolve(&host)
            .await
            .unwrap()
            .unwrap()
            .account_suspended
    );
    let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(pid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(terminated);
    let watched = routes.clone();
    let task = tokio::spawn(async move { watched.listen_for_invalidations(listener).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !routes
            .resolve(&host)
            .await
            .unwrap()
            .unwrap()
            .account_suspended
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // Prove the replacement connection resubscribed and remains supervised.
    sqlx::query("UPDATE users SET account_status = 'active' WHERE id = $1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while routes
            .resolve(&host)
            .await
            .unwrap()
            .unwrap()
            .account_suspended
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
}
