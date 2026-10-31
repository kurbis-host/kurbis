mod common;

use std::sync::Arc;

use axum::{Router, extract::State, http::HeaderMap, routing::post};
use common::test_pool;
use kurbis_hoster::config::AlertConfig;
use kurbis_hoster::control_plane::alerts::WorkerAlerts;
use sqlx::PgPool;
use tokio::{net::TcpListener, sync::Mutex};
use uuid::Uuid;

static DB_TEST_LOCK: Mutex<()> = Mutex::const_new(());

#[derive(Debug, Clone)]
struct Received {
    title: String,
    priority: String,
    authorization: Option<String>,
    body: String,
}

async fn ntfy(State(received): State<Arc<Mutex<Vec<Received>>>>, headers: HeaderMap, body: String) {
    let header = |name: &str| {
        headers
            .get(name)
            .map(|value| value.to_str().unwrap().to_owned())
    };
    received.lock().await.push(Received {
        title: header("title").unwrap(),
        priority: header("priority").unwrap(),
        authorization: header("authorization"),
        body,
    });
}

async fn set_heartbeat(pool: &PgPool, worker_id: Uuid, seconds_ago: i64) {
    sqlx::query(
        "UPDATE workers SET heartbeat_at = now() - make_interval(secs => $2) WHERE id = $1",
    )
    .bind(worker_id)
    .bind(seconds_ago as f64)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn a_silent_fleet_is_reported_once_and_its_recovery_once() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    sqlx::query("DELETE FROM workers")
        .execute(&pool)
        .await
        .unwrap();

    let received = Arc::new(Mutex::new(Vec::<Received>::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/kurbis-ops", post(ntfy))
        .with_state(received.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let alerts = WorkerAlerts::new(
        pool.clone(),
        AlertConfig {
            ntfy_url: format!("http://{address}/kurbis-ops"),
            ntfy_token: Some(kurbis_hoster::config::SecretString::new("tk_test")),
        },
    )
    .unwrap();

    // No worker registered yet: nothing to miss.
    alerts.run_once().await.unwrap();
    assert!(received.lock().await.is_empty());

    let worker_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workers(id, name, address, signal_address, total_memory_bytes,
                            total_disk_bytes, total_cpu_millis, port_min, port_max)
         VALUES($1, $2, '203.0.113.10', '10.20.0.10', 8589934592, 42949672960, 4000, 25000, 25100)",
    )
    .bind(worker_id)
    .bind(format!("alerts-{worker_id}"))
    .execute(&pool)
    .await
    .unwrap();

    // A worker restarting during a release stays below the threshold.
    set_heartbeat(&pool, worker_id, 90).await;
    alerts.run_once().await.unwrap();
    assert!(received.lock().await.is_empty());

    set_heartbeat(&pool, worker_id, 46 * 3600).await;
    alerts.run_once().await.unwrap();
    alerts.run_once().await.unwrap();
    {
        let sent = received.lock().await;
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].title, "Kurbis: no worker is running");
        assert_eq!(sent[0].priority, "urgent");
        assert_eq!(sent[0].authorization.as_deref(), Some("Bearer tk_test"));
        assert!(
            sent[0].body.contains("Servers cannot start"),
            "{}",
            sent[0].body
        );
    }

    set_heartbeat(&pool, worker_id, 0).await;
    alerts.run_once().await.unwrap();
    alerts.run_once().await.unwrap();
    let sent = received.lock().await;
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert_eq!(sent[1].title, "Kurbis: a worker is back");
}

#[tokio::test]
async fn an_unreachable_topic_fails_the_check_so_it_is_retried() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    // Nothing listens on a port that was just released.
    let address = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap();
    let worker_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workers(id, name, address, signal_address, total_memory_bytes,
                            total_disk_bytes, total_cpu_millis, port_min, port_max, heartbeat_at)
         VALUES($1, $2, '203.0.113.11', '10.20.0.11', 8589934592, 42949672960, 4000, 25000, 25100,
                now() - interval '1 hour')",
    )
    .bind(worker_id)
    .bind(format!("alerts-unreachable-{worker_id}"))
    .execute(&pool)
    .await
    .unwrap();
    let alerts = WorkerAlerts::new(
        pool.clone(),
        AlertConfig {
            ntfy_url: format!("http://{address}/kurbis-ops"),
            ntfy_token: None,
        },
    )
    .unwrap();
    assert!(alerts.run_once().await.is_err());
    sqlx::query("DELETE FROM workers WHERE id = $1")
        .bind(worker_id)
        .execute(&pool)
        .await
        .unwrap();
}
