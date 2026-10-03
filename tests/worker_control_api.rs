mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path as ObjectPath};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use common::{api_state, create_user, test_pool};
use kurbis_hoster::control_plane::{
    api::router,
    lifecycle::{Lifecycle, LifecycleActor},
};

struct TestWorker {
    id: Uuid,
    token: String,
    lease_id: Uuid,
}

async fn create_worker(pool: &PgPool, label: &str) -> TestWorker {
    let id = Uuid::new_v4();
    let token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let lease_id = worker_lease_id(&token);
    let name = format!("{label}-{}", id.simple());
    sqlx::query(
        "INSERT INTO workers(
             id, name, address, signal_address, total_memory_bytes, total_disk_bytes,
             total_cpu_millis, port_min, port_max, heartbeat_at
         ) VALUES($1, $2, '127.0.0.1', '127.0.0.1', 8589934592, 21474836480,
                  4000, 22000, 22100, now())",
    )
    .bind(id)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO worker_credentials(worker_id, api_token_hash, lease_id, lease_expires_at)
         VALUES($1, $2, $3, now() + interval '1 hour')",
    )
    .bind(id)
    .bind(blake3::hash(token.as_bytes()).as_bytes().to_vec())
    .bind(lease_id)
    .execute(pool)
    .await
    .unwrap();
    TestWorker {
        id,
        token,
        lease_id,
    }
}

async fn create_server(pool: &PgPool, label: &str) -> Uuid {
    let owner = create_user(
        pool,
        "google",
        &format!("worker-control-{label}-{}", Uuid::new_v4()),
        "Worker Control Test",
        "active",
        None,
    )
    .await;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
        .bind(id)
        .bind(owner)
        .bind(format!("wc-{}", &id.simple().to_string()[..16]))
        .execute(pool)
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn lifecycle_stop_actor_path_is_shared_and_worker_fenced() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "actor-stop").await;
    let other_worker = create_worker(&pool, "actor-stop-other").await;
    let server_id = create_server(&pool, "actor-stop").await;
    set_server_state(&pool, server_id, 1, "running", "online").await;
    add_placement(&pool, server_id, worker.id, 1, true).await;

    let lifecycle = Lifecycle::new(pool.clone());
    let stale = lifecycle
        .request_stop_for_actor(
            LifecycleActor::AssignedWorker {
                worker_id: other_worker.id,
                expected_generation: 1,
                lease_id: other_worker.lease_id,
            },
            server_id,
            "idle_timeout",
        )
        .await;
    assert!(stale.is_err());

    let observed = lifecycle
        .request_stop_for_actor(
            LifecycleActor::AssignedWorker {
                worker_id: worker.id,
                expected_generation: 1,
                lease_id: worker.lease_id,
            },
            server_id,
            "idle_timeout",
        )
        .await
        .unwrap();
    assert_eq!(observed, kurbis_hoster::model::ObservedState::Stopping);
}

#[tokio::test]
async fn worker_stop_command_uses_the_shared_command_route_and_rejects_foreign_actor() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "route-stop").await;
    let foreign = create_worker(&pool, "route-stop-foreign").await;
    let server_id = create_server(&pool, "route-stop").await;
    set_server_state(&pool, server_id, 1, "running", "online").await;
    add_placement(&pool, server_id, worker.id, 1, true).await;
    let service = app(pool.clone(), Arc::new(InMemory::new()));
    let uri = format!("/api/v1/servers/{server_id}/commands");
    let body = json!({"command":"stop", "generation": 1, "reason": "idle_timeout"});

    let (status, _) = send_json(
        service.clone(),
        Method::POST,
        &uri,
        Some(&foreign.token),
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = send_json(service, Method::POST, &uri, Some(&worker.token), body).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let observed: String =
        sqlx::query_scalar("SELECT observed_state::text FROM servers WHERE id = $1")
            .bind(server_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(observed, "stopping");
}

#[tokio::test]
async fn worker_command_rechecks_lease_after_waiting_for_server_lock() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "command-lease-race").await;
    let server_id = create_server(&pool, "command-lease-race").await;
    set_server_state(&pool, server_id, 1, "running", "online").await;
    let placement_lease = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO placements(
             server_id, worker_id, generation, lease_token, java_port, bedrock_port,
             signal_port, active
         ) VALUES($1, $2, 1, $3, 22000, 22001, 22002, true)",
    )
    .bind(server_id)
    .bind(worker.id)
    .bind(placement_lease)
    .execute(&pool)
    .await
    .unwrap();

    let mut server_lock = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM servers WHERE id = $1 FOR UPDATE")
        .bind(server_id)
        .execute(&mut *server_lock)
        .await
        .unwrap();

    let service = app(pool.clone(), Arc::new(InMemory::new()));
    let token = worker.token.clone();
    let command_uri = format!("/api/v1/servers/{server_id}/commands");
    let request = tokio::spawn(async move {
        worker_api_request(
            service,
            &command_uri,
            &token,
            Some(worker_lease_id(&token)),
            json!({"command":"stop", "generation":1, "reason":"idle_timeout"}),
        )
        .await
    });

    let blocked = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                     SELECT 1 FROM pg_stat_activity
                     WHERE datname = current_database()
                       AND wait_event_type = 'Lock'
                       AND query LIKE '%FROM servers WHERE id = $1 FOR UPDATE%'
                 )",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if blocked.is_err() {
        request.abort();
        server_lock.rollback().await.unwrap();
        let _ = request.await;
        panic!("worker command did not block on the held server row lock");
    }

    let next_lease = Uuid::new_v4();
    let mut credential_rotation = pool.begin().await.unwrap();
    sqlx::query(
        "UPDATE worker_credentials
         SET lease_id = $2, lease_expires_at = now() + interval '1 hour'
         WHERE worker_id = $1",
    )
    .bind(worker.id)
    .bind(next_lease)
    .execute(&mut *credential_rotation)
    .await
    .unwrap();
    credential_rotation.commit().await.unwrap();
    server_lock.commit().await.unwrap();

    assert_eq!(request.await.unwrap(), StatusCode::CONFLICT);
    let unchanged: (String, String, i64, i64, Uuid, i64, Uuid, bool) = sqlx::query_as(
        "SELECT s.desired_state::text, s.observed_state::text, s.generation,
                (SELECT count(*) FROM operations WHERE server_id = s.id),
                p.worker_id, p.generation, p.lease_token, p.active
         FROM servers s JOIN placements p ON p.server_id = s.id
         WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        unchanged,
        (
            "running".to_owned(),
            "online".to_owned(),
            1,
            0,
            worker.id,
            1,
            placement_lease,
            true,
        )
    );

    assert_eq!(
        worker_api_request(
            app(pool.clone(), Arc::new(InMemory::new())),
            &format!("/api/v1/servers/{server_id}/commands"),
            &worker.token,
            Some(next_lease),
            json!({"command":"stop", "generation":1, "reason":"idle_timeout"}),
        )
        .await,
        StatusCode::ACCEPTED
    );
    let stopped: (String, String, i64) = sqlx::query_as(
        "SELECT desired_state::text, observed_state::text,
                (SELECT count(*) FROM operations WHERE server_id = $1)
         FROM servers WHERE id = $1",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stopped, ("stopped".to_owned(), "stopping".to_owned(), 1));
}

#[tokio::test]
async fn workers_cannot_reset_or_delete_through_the_shared_command_route() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "destructive-denial").await;
    let server_id = create_server(&pool, "destructive-denial").await;
    set_server_state(&pool, server_id, 1, "stopped", "sleeping").await;
    add_placement(&pool, server_id, worker.id, 1, true).await;
    let service = app(pool.clone(), Arc::new(InMemory::new()));
    let uri = format!("/api/v1/servers/{server_id}/commands");

    for command in [
        json!({"command":"reset", "mode":"world"}),
        json!({"command":"delete"}),
    ] {
        let (status, _) = send_json(
            service.clone(),
            Method::POST,
            &uri,
            Some(&worker.token),
            command,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    let (observed, desired, generation): (String, String, i64) = sqlx::query_as(
        "SELECT observed_state::text, desired_state::text, generation FROM servers WHERE id = $1",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (observed.as_str(), desired.as_str(), generation),
        ("sleeping", "stopped", 1)
    );
}

#[tokio::test]
async fn snapshot_cache_hints_require_the_worker_lease_assignment_and_complete_generation() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "snapshot-hint").await;
    let foreign = create_worker(&pool, "snapshot-hint-foreign").await;
    let server_id = create_server(&pool, "snapshot-hint").await;
    set_server_state(&pool, server_id, 1, "stopped", "sleeping").await;
    add_placement(&pool, server_id, worker.id, 1, false).await;
    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes,
                               checksum, reason, complete)
         VALUES($1, $2, 1, 'snapshots/1-complete.json', 12, 'complete-checksum', 'shutdown', true),
               ($3, $2, 2, 'snapshots/2-complete.json', 16, 'newer-checksum', 'shutdown', true),
               ($4, $2, 3, 'snapshots/3-incomplete.json', 20, 'incomplete-checksum', 'periodic', false)",
    )
    .bind(Uuid::new_v4())
    .bind(server_id)
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE servers SET current_snapshot_generation = 2 WHERE id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    let service = app(pool.clone(), Arc::new(InMemory::new()));
    let endpoint = format!("/internal/worker/control/servers/{server_id}/snapshots/cache-hint");
    let placement_hint = |snapshot_generation, generation| {
        json!({
            "snapshot_generation": snapshot_generation,
            "assignment": {"placement": {"generation": generation}}
        })
    };

    assert_eq!(
        worker_api_request(
            service.clone(),
            &endpoint,
            &worker.token,
            None,
            placement_hint(1, 1),
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        worker_api_request(
            service.clone(),
            &endpoint,
            &worker.token,
            Some(worker.lease_id),
            placement_hint(1, 1),
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        worker_api_request(
            service.clone(),
            &endpoint,
            &foreign.token,
            Some(foreign.lease_id),
            placement_hint(1, 1),
        )
        .await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        worker_api_request(
            service.clone(),
            &endpoint,
            &worker.token,
            Some(worker.lease_id),
            placement_hint(1, 99),
        )
        .await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        worker_api_request(
            service.clone(),
            &endpoint,
            &worker.token,
            Some(worker.lease_id),
            placement_hint(3, 1),
        )
        .await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        worker_api_request(
            service.clone(),
            &endpoint,
            &worker.token,
            Some(worker.lease_id),
            placement_hint(2, 1),
        )
        .await,
        StatusCode::NO_CONTENT
    );
    let replaced_generation: i64 = sqlx::query_scalar(
        "SELECT snapshot_generation FROM worker_snapshot_hints
         WHERE worker_id = $1 AND server_id = $2",
    )
    .bind(worker.id)
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(replaced_generation, 2);

    let operation_id = add_operation(
        &pool,
        server_id,
        worker.id,
        1,
        "snapshot",
        "running",
        json!({"reason":"manual"}),
    )
    .await;
    assert_eq!(
        worker_api_request(
            service,
            &endpoint,
            &worker.token,
            Some(worker.lease_id),
            json!({
                "snapshot_generation": 2,
                "assignment": {"operation": {
                    "operation_id": operation_id,
                    "generation": 1,
                    "attempt": 1
                }}
            }),
        )
        .await,
        StatusCode::NO_CONTENT
    );

    let export_id = Uuid::new_v4();
    let owner_id: Uuid = sqlx::query_scalar("SELECT owner_id FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO world_exports(id, server_id, requested_by, status, stage,
                                   source_generation, worker_id, attempt)
         VALUES($1, $2, $3, 'running', 'archiving', 2, $4, 1)",
    )
    .bind(export_id)
    .bind(server_id)
    .bind(owner_id)
    .bind(worker.id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        worker_api_request(
            app(pool.clone(), Arc::new(InMemory::new())),
            &endpoint,
            &worker.token,
            Some(worker.lease_id),
            json!({
                "snapshot_generation": 2,
                "assignment": {"export": {
                    "export_id": export_id,
                    "generation": 2,
                    "attempt": 1
                }}
            }),
        )
        .await,
        StatusCode::CONFLICT
    );

    let hints: Vec<(Uuid, Uuid, i64)> = sqlx::query_as(
        "SELECT worker_id, server_id, snapshot_generation FROM worker_snapshot_hints
         WHERE server_id = $1 ORDER BY worker_id",
    )
    .bind(server_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(hints, vec![(worker.id, server_id, 2)]);

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers WHERE id = ANY($1::uuid[])")
        .bind(vec![worker.id, foreign.id])
        .execute(&pool)
        .await
        .unwrap();
}

async fn set_server_state(
    pool: &PgPool,
    server_id: Uuid,
    generation: i64,
    desired: &str,
    observed: &str,
) {
    sqlx::query(
        "UPDATE servers SET generation = $2, desired_state = $3::server_desired_state,
             observed_state = $4::server_observed_state WHERE id = $1",
    )
    .bind(server_id)
    .bind(generation)
    .bind(desired)
    .bind(observed)
    .execute(pool)
    .await
    .unwrap();
}

async fn add_placement(
    pool: &PgPool,
    server_id: Uuid,
    worker_id: Uuid,
    generation: i64,
    active: bool,
) {
    sqlx::query(
        "INSERT INTO placements(
             server_id, worker_id, generation, lease_token, java_port, bedrock_port,
             signal_port, active
         ) VALUES($1, $2, $3, $4, 22000, 22001, 22002, $5)",
    )
    .bind(server_id)
    .bind(worker_id)
    .bind(generation)
    .bind(Uuid::new_v4())
    .bind(active)
    .execute(pool)
    .await
    .unwrap();
}

async fn add_operation(
    pool: &PgPool,
    server_id: Uuid,
    worker_id: Uuid,
    generation: i64,
    kind: &str,
    status: &str,
    payload: Value,
) -> Uuid {
    let operation_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO operations(id, server_id, worker_id, kind, status, generation,
                                attempt, payload, claimed_at)
         VALUES($1, $2, $3, $4::operation_kind, $5::operation_status, $6, 1, $7, now())",
    )
    .bind(operation_id)
    .bind(server_id)
    .bind(worker_id)
    .bind(kind)
    .bind(status)
    .bind(generation)
    .bind(payload)
    .execute(pool)
    .await
    .unwrap();
    operation_id
}

fn app(pool: PgPool, store: Arc<dyn ObjectStore>) -> Router {
    router(api_state(pool, "shared-internal-test-token", store))
}

async fn send_json(
    app: Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        builder = builder.header("x-worker-lease", worker_lease_id(token).to_string());
    }
    let request = builder.body(Body::from(body.to_string())).unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

fn worker_lease_id(token: &str) -> Uuid {
    Uuid::from_slice(&blake3::hash(token.as_bytes()).as_bytes()[..16]).unwrap()
}

async fn worker_api_request(
    app: Router,
    uri: &str,
    token: &str,
    lease_id: Option<Uuid>,
    body: Value,
) -> StatusCode {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(lease_id) = lease_id {
        builder = builder.header("x-worker-lease", lease_id.to_string());
    }
    app.oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn control_plane_lease_excludes_a_second_process_and_fences_expired_owners() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "lease-api").await;
    sqlx::query("UPDATE worker_credentials SET lease_id = NULL, lease_expires_at = NULL WHERE worker_id = $1")
        .bind(worker.id).execute(&pool).await.unwrap();
    let service = app(pool.clone(), Arc::new(InMemory::new()));
    let endpoint = "/internal/worker/control/lease";
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let acquire = |lease_id| json!({"lease_id": lease_id, "signal_address":"127.0.0.1"});

    let (status, response) = send_json(
        service.clone(),
        Method::POST,
        endpoint,
        Some(&worker.token),
        acquire(first),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["worker_id"], worker.id.to_string());
    assert_eq!(
        worker_api_request(
            service.clone(),
            endpoint,
            &worker.token,
            None,
            acquire(second)
        )
        .await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        worker_api_request(
            service.clone(),
            "/internal/worker/control/heartbeat",
            &worker.token,
            Some(first),
            json!({})
        )
        .await,
        StatusCode::NO_CONTENT
    );

    sqlx::query("UPDATE worker_credentials SET lease_expires_at = now() - interval '1 second' WHERE worker_id = $1")
        .bind(worker.id).execute(&pool).await.unwrap();
    assert_eq!(
        worker_api_request(
            service.clone(),
            "/internal/worker/control/heartbeat",
            &worker.token,
            Some(first),
            json!({})
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        worker_api_request(
            service.clone(),
            endpoint,
            &worker.token,
            Some(first),
            acquire(second)
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        worker_api_request(
            service,
            "/internal/worker/control/heartbeat",
            &worker.token,
            Some(first),
            json!({})
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn worker_state_reads_and_runtime_sync_are_assignment_scoped() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "state-worker").await;
    let foreign = create_worker(&pool, "state-foreign").await;
    let server = create_server(&pool, "state-owned").await;
    let foreign_server = create_server(&pool, "state-foreign-world").await;
    set_server_state(&pool, server, 3, "running", "online").await;
    set_server_state(&pool, foreign_server, 7, "running", "online").await;
    add_placement(&pool, server, worker.id, 3, true).await;
    add_placement(&pool, foreign_server, foreign.id, 7, true).await;
    let app = app(pool.clone(), Arc::new(InMemory::new()));

    let (status, states) = send_json(
        app.clone(),
        Method::GET,
        "/internal/worker/control/servers",
        Some(&worker.token),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(states.as_array().unwrap().len(), 1);
    assert_eq!(states[0]["placement"]["server_id"], server.to_string());
    assert_eq!(states[0]["placement"]["generation"], 3);
    assert_eq!(states[0]["observed_state"], "online");

    let context_uri = format!(
        "/internal/worker/control/servers/{server}/context?assignment=placement&generation=3"
    );
    let (status, context) = send_json(
        app.clone(),
        Method::GET,
        &context_uri,
        Some(&worker.token),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(context["placement"]["worker_id"], worker.id.to_string());
    assert_eq!(context["settings"], json!({}));
    assert!(context["owners"].as_array().unwrap().is_empty());

    let wrong_worker_uri = format!(
        "/internal/worker/control/servers/{server}/context?assignment=placement&generation=3"
    );
    let (status, _) = send_json(
        app.clone(),
        Method::GET,
        &wrong_worker_uri,
        Some(&foreign.token),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let stale_uri = format!(
        "/internal/worker/control/servers/{server}/context?assignment=placement&generation=2"
    );
    let (status, _) = send_json(
        app.clone(),
        Method::GET,
        &stale_uri,
        Some(&worker.token),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let player_id = Uuid::new_v4();
    let players_uri = format!("/internal/worker/control/servers/{server}/players");
    let (status, _) = send_json(
        app.clone(),
        Method::POST,
        &players_uri,
        Some(&worker.token),
        json!({
            "generation":3,
            "players":[{
                "connection_id":Uuid::new_v4(),
                "id":player_id,
                "name":"CurrentPlayer",
                "edition":"java",
                "server_address":"play.example.test",
                "ready_for_transfer":true
            }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let players: (i64, Option<String>) =
        sqlx::query_as("SELECT count(*), max(name) FROM server_players WHERE server_id=$1")
            .bind(server)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(players, (1, Some("CurrentPlayer".to_owned())));
    let known_player: Option<(String, String)> = sqlx::query_as(
        "SELECT name, edition FROM server_known_players
         WHERE server_id = $1 AND player_id = $2",
    )
    .bind(server)
    .bind(player_id)
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(
        known_player,
        Some(("CurrentPlayer".to_owned(), "java".to_owned()))
    );

    let (status, _) = send_json(
        app.clone(),
        Method::POST,
        &players_uri,
        Some(&worker.token),
        json!({"generation":3,"players":[]}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (current_count, known_count): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM server_players WHERE server_id = $1),
                (SELECT count(*) FROM server_known_players WHERE server_id = $1)",
    )
    .bind(server)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((current_count, known_count), (0, 1));

    let logs_uri = format!("/internal/worker/control/servers/{server}/logs");
    let (status, _) = send_json(
        app.clone(),
        Method::POST,
        &logs_uri,
        Some(&worker.token),
        json!({"generation":3,"lines":["one line", "another line"]}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let logs: i64 = sqlx::query_scalar("SELECT count(*) FROM server_logs WHERE server_id=$1")
        .bind(server)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(logs, 2);

    let (status, _) = send_json(
        app,
        Method::POST,
        &format!("/internal/worker/control/servers/{server}/players"),
        Some(&worker.token),
        json!({"generation":2,"players":[]}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn worker_claims_are_authenticated_scoped_and_replayable() {
    let pool = test_pool().await;
    let worker_a = create_worker(&pool, "claim-a").await;
    let worker_b = create_worker(&pool, "claim-b").await;
    let server_a = create_server(&pool, "claim-a").await;
    let server_b = create_server(&pool, "claim-b").await;
    set_server_state(&pool, server_a, 1, "running", "starting").await;
    set_server_state(&pool, server_b, 1, "running", "starting").await;
    let op_a = Uuid::new_v4();
    let op_b = Uuid::new_v4();
    for (id, server, worker) in [(op_a, server_a, &worker_a), (op_b, server_b, &worker_b)] {
        sqlx::query(
            "INSERT INTO operations(id, server_id, worker_id, kind, status, generation)
             VALUES($1, $2, $3, 'start', 'pending', 1)",
        )
        .bind(id)
        .bind(server)
        .bind(worker.id)
        .execute(&pool)
        .await
        .unwrap();
    }

    let service = app(pool.clone(), Arc::new(InMemory::new()));
    let claim_uri = "/internal/worker/control/operations/claim";
    let (status, claim) = send_json(
        service.clone(),
        Method::POST,
        claim_uri,
        Some(&worker_a.token),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(claim["id"], op_a.to_string());
    assert_eq!(claim["worker_id"], worker_a.id.to_string());

    let (status, replay) = send_json(
        service.clone(),
        Method::POST,
        claim_uri,
        Some(&worker_a.token),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["id"], op_a.to_string());
    assert_eq!(replay["attempt"], 1);

    let (status, _) = send_json(
        service.clone(),
        Method::POST,
        claim_uri,
        Some("shared-internal-test-token"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let forged_uri = format!("/internal/worker/control/operations/{op_a}/progress");
    let (status, _) = send_json(
        service,
        Method::POST,
        &forged_uri,
        Some(&worker_a.token),
        json!({ "worker_id": worker_b.id, "generation": 1, "attempt": 1, "patch": {"stage":"starting"} }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let states: Vec<(Uuid, String, i32)> = sqlx::query_as(
        "SELECT id, status::text, attempt FROM operations WHERE id = ANY($1::uuid[]) ORDER BY id",
    )
    .bind(vec![op_a, op_b])
    .fetch_all(&pool)
    .await
    .unwrap();
    let a = states.iter().find(|row| row.0 == op_a).unwrap();
    let b = states.iter().find(|row| row.0 == op_b).unwrap();
    assert_eq!((&*a.1, a.2), ("running", 1));
    assert_eq!((&*b.1, b.2), ("pending", 0));
}

#[tokio::test]
async fn a_worker_cannot_mutate_another_workers_operation_or_server() {
    let pool = test_pool().await;
    let worker_a = create_worker(&pool, "foreign-a").await;
    let worker_b = create_worker(&pool, "foreign-b").await;
    let server_b = create_server(&pool, "foreign-b").await;
    set_server_state(&pool, server_b, 7, "running", "restoring").await;
    add_placement(&pool, server_b, worker_b.id, 7, false).await;
    let operation_b = add_operation(
        &pool,
        server_b,
        worker_b.id,
        7,
        "start",
        "running",
        json!({}),
    )
    .await;
    let export_b = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO world_exports(
             id, server_id, requested_by, status, stage, source_generation,
             worker_id, attempt
         ) VALUES($1, $2, $3, 'running', 'archiving', 2, $4, 1)",
    )
    .bind(export_b)
    .bind(server_b)
    .bind(Uuid::new_v4())
    .bind(worker_b.id)
    .execute(&pool)
    .await
    .unwrap();
    let service = app(pool.clone(), Arc::new(InMemory::new()));

    let (status, _) = send_json(
        service.clone(),
        Method::POST,
        &format!("/internal/worker/control/operations/{operation_b}/complete"),
        Some(&worker_a.token),
        json!({ "generation": 7, "attempt": 1, "completion": {"kind":"start", "storage_bytes": 12} }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send_json(
        service.clone(),
        Method::POST,
        &format!("/internal/worker/control/exports/{export_b}/complete"),
        Some(&worker_a.token),
        json!({
            "source_generation": 2,
            "attempt": 1,
            "archive_key": format!("exports/{export_b}/1.zip"),
            "archive_size": 0,
            "archive_checksum": "0".repeat(64)
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send_json(
        service.clone(),
        Method::POST,
        &format!("/api/v1/servers/{server_b}/commands"),
        Some(&worker_a.token),
        json!({ "command":"stop", "generation": 7, "reason":"idle_timeout" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, _) = send_json(
        service.clone(),
        Method::POST,
        &format!("/internal/worker/control/servers/{server_b}/snapshots/publish"),
        Some(&worker_a.token),
        json!({
            "operation_id": operation_b,
            "generation": 7,
            "attempt": 1,
            "snapshot_generation": 1,
            "size_bytes": 0
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let operation: (String, String) = sqlx::query_as(
        "SELECT o.status::text, s.observed_state::text FROM operations o
         JOIN servers s ON s.id = o.server_id WHERE o.id = $1",
    )
    .bind(operation_b)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(operation, ("running".to_owned(), "restoring".to_owned()));
    let export_status: String =
        sqlx::query_scalar("SELECT status FROM world_exports WHERE id = $1")
            .bind(export_b)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(export_status, "running");
}

#[tokio::test]
async fn current_worker_can_publish_verified_snapshot_then_complete_operation() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "publish").await;
    let server_id = create_server(&pool, "publish").await;
    set_server_state(&pool, server_id, 4, "running", "snapshotting").await;
    add_placement(&pool, server_id, worker.id, 4, false).await;
    let operation = add_operation(
        &pool,
        server_id,
        worker.id,
        4,
        "snapshot",
        "running",
        json!({"reason":"periodic"}),
    )
    .await;

    let manifest = json!({
        "server_id": server_id,
        "generation": 5,
        "files": [{"path":"world/region/r.0.0.mca", "size":3, "blake3":blake3::hash(b"abc").to_hex().to_string()}]
    });
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let checksum = blake3::hash(&manifest_bytes).to_hex().to_string();
    let manifest_key = format!("snapshots/5-{checksum}.json");
    let store = Arc::new(InMemory::new());
    store
        .put(
            &ObjectPath::from(format!("servers/{server_id}/{manifest_key}")),
            PutPayload::from(manifest_bytes),
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes, checksum,
                               reason, complete, created_at)
         VALUES($1, $2, 5, $3, 3, $4, 'periodic', true, now())",
    )
    .bind(Uuid::new_v4())
    .bind(server_id)
    .bind(manifest_key)
    .bind(checksum)
    .execute(&pool)
    .await
    .unwrap();
    let service = app(pool.clone(), store);
    let publish_uri = format!("/internal/worker/control/servers/{server_id}/snapshots/publish");
    let publish = json!({
        "operation_id": operation,
        "generation": 4,
        "attempt": 1,
        "snapshot_generation": 5,
        "size_bytes": 3
    });
    let (status, _) = send_json(
        service.clone(),
        Method::POST,
        &publish_uri,
        Some(&worker.token),
        publish.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (generation, size): (Option<i64>, i64) = sqlx::query_as(
        "SELECT current_snapshot_generation, storage_bytes FROM servers WHERE id = $1",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((generation, size), (Some(5), 3));

    let (status, _) = send_json(
        service.clone(),
        Method::POST,
        &publish_uri,
        Some(&worker.token),
        publish,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = send_json(
        service,
        Method::POST,
        &format!("/internal/worker/control/operations/{operation}/complete"),
        Some(&worker.token),
        json!({ "generation":4, "attempt":1, "completion":{"kind":"snapshot", "snapshot_generation":5} }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let final_state: (String, bool, bool) = sqlx::query_as(
        "SELECT s.observed_state::text, p.active, o.status = 'succeeded'
         FROM servers s JOIN placements p ON p.server_id=s.id
         JOIN operations o ON o.server_id=s.id WHERE s.id=$1 AND o.id=$2",
    )
    .bind(server_id)
    .bind(operation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(final_state, ("online".to_owned(), true, true));
}

#[tokio::test]
async fn stale_operation_attempt_cannot_complete_after_reassignment() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "stale").await;
    let server_id = create_server(&pool, "stale").await;
    set_server_state(&pool, server_id, 3, "running", "restoring").await;
    add_placement(&pool, server_id, worker.id, 3, false).await;
    let operation = add_operation(
        &pool,
        server_id,
        worker.id,
        3,
        "start",
        "running",
        json!({}),
    )
    .await;
    let service = app(pool.clone(), Arc::new(InMemory::new()));
    let (status, _) = send_json(
        service,
        Method::POST,
        &format!("/internal/worker/control/operations/{operation}/complete"),
        Some(&worker.token),
        json!({ "generation":2, "attempt":1, "completion":{"kind":"start", "storage_bytes":12} }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (generation, observed): (i64, String) =
        sqlx::query_as("SELECT generation, observed_state::text FROM servers WHERE id=$1")
            .bind(server_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((generation, observed.as_str()), (3, "restoring"));
}

#[tokio::test]
async fn current_worker_can_complete_start_after_runtime_readiness() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "start-complete").await;
    let server_id = create_server(&pool, "start-complete").await;
    set_server_state(&pool, server_id, 1, "running", "starting").await;
    add_placement(&pool, server_id, worker.id, 1, false).await;
    let operation = add_operation(
        &pool,
        server_id,
        worker.id,
        1,
        "start",
        "running",
        json!({ "reason": "gateway_join" }),
    )
    .await;

    let service = app(pool.clone(), Arc::new(InMemory::new()));
    let (status, _) = send_json(
        service,
        Method::POST,
        &format!("/internal/worker/control/operations/{operation}/complete"),
        Some(&worker.token),
        json!({
            "generation": 1,
            "attempt": 1,
            "completion": {"kind": "start", "storage_bytes": 0}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let state: (String, bool, String) = sqlx::query_as(
        "SELECT s.observed_state::text, p.active, o.status::text
         FROM servers s JOIN placements p ON p.server_id = s.id
         JOIN operations o ON o.server_id = s.id WHERE s.id = $1 AND o.id = $2",
    )
    .bind(server_id)
    .bind(operation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, ("online".to_owned(), true, "succeeded".to_owned()));
}

#[tokio::test]
async fn export_completion_fences_writes_and_verifies_the_uploaded_archive() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "export-verify").await;
    let server_id = create_server(&pool, "export-verify").await;
    let export_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO world_exports(
             id, server_id, requested_by, status, stage, source_generation,
             worker_id, attempt
         ) VALUES($1, $2, $3, 'running', 'archiving', 8, $4, 2)",
    )
    .bind(export_id)
    .bind(server_id)
    .bind(Uuid::new_v4())
    .bind(worker.id)
    .execute(&pool)
    .await
    .unwrap();
    let archive = b"world archive fixture";
    let archive_key = format!("exports/{export_id}/2.zip");
    let checksum = blake3::hash(archive).to_hex().to_string();
    let store = Arc::new(InMemory::new());
    store
        .put(
            &ObjectPath::from(format!("servers/{server_id}/{archive_key}")),
            PutPayload::from(archive.to_vec()),
        )
        .await
        .unwrap();
    let service = app(pool.clone(), store);
    let uri = format!("/internal/worker/control/exports/{export_id}/complete");
    let request = json!({
        "source_generation": 8,
        "attempt": 2,
        "archive_key": archive_key.clone(),
        "archive_size": archive.len(),
        "archive_checksum": checksum
    });

    let (status, _) = send_json(
        service.clone(),
        Method::POST,
        &uri,
        Some(&worker.token),
        request.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = send_json(service, Method::POST, &uri, Some(&worker.token), request).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let result: (String, String, String, Option<i64>) = sqlx::query_as(
        "SELECT status, stage, archive_key, archive_size
         FROM world_exports WHERE id = $1",
    )
    .bind(export_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        result,
        (
            "completed".into(),
            "completed".into(),
            archive_key,
            Some(archive.len() as i64)
        )
    );
}

#[tokio::test]
async fn retry_stores_bounded_worker_error_but_rejects_oversized_direct_error() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "retry-error-boundary").await;
    let server_id = create_server(&pool, "retry-error-boundary").await;
    set_server_state(&pool, server_id, 1, "running", "starting").await;
    let operation = add_operation(
        &pool,
        server_id,
        worker.id,
        1,
        "start",
        "running",
        json!({ "reason": "gateway_join" }),
    )
    .await;
    let service = app(pool.clone(), Arc::new(InMemory::new()));
    let uri = format!("/internal/worker/control/operations/{operation}/retry");
    let oversized = "x".repeat(1_001);
    let (status, _) = send_json(
        service.clone(),
        Method::POST,
        &uri,
        Some(&worker.token),
        json!({ "generation": 1, "attempt": 1, "error": oversized }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let state: (String, Option<String>) =
        sqlx::query_as("SELECT status::text, last_error FROM operations WHERE id = $1")
            .bind(operation)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, ("running".to_owned(), None));

    let bounded = format!(
        "Pumpkin readiness failed: {}...[truncated]",
        "x".repeat(960)
    );
    assert_eq!(bounded.len(), 1_000);
    let (status, body) = send_json(
        service,
        Method::POST,
        &uri,
        Some(&worker.token),
        json!({ "generation": 1, "attempt": 1, "error": bounded }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["retry_scheduled"], true);
    let state: (String, Option<String>) =
        sqlx::query_as("SELECT status::text, last_error FROM operations WHERE id = $1")
            .bind(operation)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state.0, "pending");
    assert_eq!(state.1.as_deref(), Some(bounded.as_str()));
}

#[tokio::test]
async fn console_commands_are_durable_claimed_once_and_completed_under_generation_fence() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "console-command").await;
    let foreign_worker = create_worker(&pool, "console-command-foreign").await;
    let server_id = create_server(&pool, "console-command").await;
    set_server_state(&pool, server_id, 1, "running", "online").await;
    add_placement(&pool, server_id, worker.id, 1, true).await;
    let owner_id: Uuid = sqlx::query_scalar("SELECT owner_id FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let session_token = format!("console-owner-session-{server_id}");
    sqlx::query("INSERT INTO sessions(id, token_hash, user_id) VALUES($1, $2, $3)")
        .bind(Uuid::new_v4())
        .bind(blake3::hash(session_token.as_bytes()).as_bytes().to_vec())
        .bind(owner_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET heartbeat_at = now() WHERE id = $1")
        .bind(worker.id)
        .execute(&pool)
        .await
        .unwrap();
    let service = app(pool.clone(), Arc::new(InMemory::new()));
    let command_uri = format!("/api/v1/servers/{server_id}/console/commands");
    let worker_denied = send_json(
        service.clone(),
        Method::POST,
        &command_uri,
        Some(&worker.token),
        json!({"request_id":Uuid::new_v4(),"command":"say worker must not run this"}),
    )
    .await;
    assert_eq!(worker_denied.0, StatusCode::FORBIDDEN);
    let gateway_denied = send_json(
        service.clone(),
        Method::POST,
        &command_uri,
        Some("shared-internal-test-token"),
        json!({"request_id":Uuid::new_v4(),"command":"say gateway must not run this"}),
    )
    .await;
    assert_eq!(gateway_denied.0, StatusCode::FORBIDDEN);
    let invalid = service
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(&command_uri)
                .header(header::COOKIE, format!("kurbis_session={session_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"request_id":Uuid::new_v4(),"command":"say\nno"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let request_id = Uuid::new_v4();
    let response = service
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(&command_uri)
                .header(header::COOKIE, format!("kurbis_session={session_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"request_id":request_id,"command":"say hello"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let response_status = response.status();
    let response_body = to_bytes(response.into_body(), 32 * 1024).await.unwrap();
    assert_eq!(
        response_status,
        StatusCode::ACCEPTED,
        "console enqueue failed: {}",
        String::from_utf8_lossy(&response_body)
    );
    let response: Value = serde_json::from_slice(&response_body).unwrap();
    let command_id = Uuid::parse_str(response["id"].as_str().unwrap()).unwrap();
    assert_eq!(response["status"], "queued");
    assert_eq!(response["generation"], 1);
    assert_eq!(response["request_id"], request_id.to_string());
    let replay = service
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(&command_uri)
                .header(header::COOKIE, format!("kurbis_session={session_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"request_id":request_id,"command":"say hello"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::ACCEPTED);
    let replay: Value =
        serde_json::from_slice(&to_bytes(replay.into_body(), 32 * 1024).await.unwrap()).unwrap();
    assert_eq!(replay["id"], command_id.to_string());
    let reused_id = service
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(&command_uri)
                .header(header::COOKIE, format!("kurbis_session={session_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"request_id":request_id,"command":"say different"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reused_id.status(), StatusCode::CONFLICT);

    let foreign_claim = send_json(
        service.clone(),
        Method::POST,
        "/internal/worker/control/console/commands/claim",
        Some(&foreign_worker.token),
        json!({}),
    )
    .await;
    assert_eq!(foreign_claim.0, StatusCode::OK);
    assert_eq!(foreign_claim.1, Value::Null);
    let claimed = send_json(
        service.clone(),
        Method::POST,
        "/internal/worker/control/console/commands/claim",
        Some(&worker.token),
        json!({}),
    )
    .await;
    assert_eq!(claimed.0, StatusCode::OK);
    assert_eq!(claimed.1["id"], command_id.to_string());
    assert_eq!(claimed.1["attempt"], 1);
    let duplicate_claim = send_json(
        service.clone(),
        Method::POST,
        "/internal/worker/control/console/commands/claim",
        Some(&worker.token),
        json!({}),
    )
    .await;
    assert_eq!(duplicate_claim.0, StatusCode::OK);
    assert_eq!(duplicate_claim.1, Value::Null);

    let status = service
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("{command_uri}/{command_id}"))
                .header(header::COOKIE, format!("kurbis_session={session_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
    assert_eq!(response_json(status).await["status"], "running");

    let complete = send_json(
        service.clone(),
        Method::POST,
        &format!("/internal/worker/control/console/commands/{command_id}/complete"),
        Some(&worker.token),
        json!({
            "generation": 1,
            "attempt": 1,
            "succeeded": true,
            "output": ["[Server: hello"]
        }),
    )
    .await;
    assert_eq!(complete.0, StatusCode::NO_CONTENT);

    let completed = service
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("{command_uri}/{command_id}"))
                .header(header::COOKIE, format!("kurbis_session={session_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let completed = response_json(completed).await;
    assert_eq!(completed["status"], "succeeded");
    assert_eq!(completed["output"][0], "[Server: hello");
}

#[tokio::test]
async fn console_commands_are_cancelled_and_stale_completion_is_rejected_after_generation_change() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "console-fence").await;
    let server_id = create_server(&pool, "console-fence").await;
    set_server_state(&pool, server_id, 1, "running", "online").await;
    add_placement(&pool, server_id, worker.id, 1, true).await;
    let owner_id: Uuid = sqlx::query_scalar("SELECT owner_id FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let session_token = format!("console-fence-session-{server_id}");
    sqlx::query("INSERT INTO sessions(id, token_hash, user_id) VALUES($1, $2, $3)")
        .bind(Uuid::new_v4())
        .bind(blake3::hash(session_token.as_bytes()).as_bytes().to_vec())
        .bind(owner_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET heartbeat_at = now() WHERE id = $1")
        .bind(worker.id)
        .execute(&pool)
        .await
        .unwrap();
    let service = app(pool.clone(), Arc::new(InMemory::new()));
    let uri = format!("/api/v1/servers/{server_id}/console/commands");
    let submitted = service
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(&uri)
                .header(header::COOKIE, format!("kurbis_session={session_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"request_id":Uuid::new_v4(),"command":"time query daytime"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let submitted_status = submitted.status();
    let submitted_body = to_bytes(submitted.into_body(), 32 * 1024).await.unwrap();
    assert_eq!(
        submitted_status,
        StatusCode::ACCEPTED,
        "console enqueue failed: {}",
        String::from_utf8_lossy(&submitted_body)
    );
    let submitted: Value = serde_json::from_slice(&submitted_body).unwrap();
    let command_id = Uuid::parse_str(submitted["id"].as_str().unwrap()).unwrap();
    let claimed = send_json(
        service.clone(),
        Method::POST,
        "/internal/worker/control/console/commands/claim",
        Some(&worker.token),
        json!({}),
    )
    .await;
    assert_eq!(claimed.0, StatusCode::OK);
    assert_eq!(claimed.1["id"], command_id.to_string());

    // A stop request fences a command that has been claimed but not yet
    // completed. This is the stop-vs-runtime-dispatch boundary.
    set_server_state(&pool, server_id, 1, "running", "stopping").await;
    let stop_race_completion = send_json(
        service.clone(),
        Method::POST,
        &format!("/internal/worker/control/console/commands/{command_id}/complete"),
        Some(&worker.token),
        json!({
            "generation": 1,
            "attempt": 1,
            "succeeded": true,
            "output": []
        }),
    )
    .await;
    assert_eq!(stop_race_completion.0, StatusCode::CONFLICT);
    let status: String =
        sqlx::query_scalar("SELECT status::text FROM console_commands WHERE id = $1")
            .bind(command_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "cancelled");

    // Re-online the unchanged assignment and prove that generation advance
    // also cancels an in-flight claim before accepting completion.
    set_server_state(&pool, server_id, 1, "running", "online").await;
    let second_request_id = Uuid::new_v4();
    let submitted = service
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(&uri)
                .header(header::COOKIE, format!("kurbis_session={session_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"request_id":second_request_id,"command":"say generation fenced"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(submitted.status(), StatusCode::ACCEPTED);
    let submitted: Value = response_json(submitted).await;
    let second_command_id = Uuid::parse_str(submitted["id"].as_str().unwrap()).unwrap();
    let claimed = send_json(
        service.clone(),
        Method::POST,
        "/internal/worker/control/console/commands/claim",
        Some(&worker.token),
        json!({}),
    )
    .await;
    assert_eq!(claimed.0, StatusCode::OK);
    assert_eq!(claimed.1["id"], second_command_id.to_string());

    set_server_state(&pool, server_id, 2, "running", "restoring").await;
    let stale = send_json(
        service,
        Method::POST,
        &format!("/internal/worker/control/console/commands/{second_command_id}/complete"),
        Some(&worker.token),
        json!({
            "generation": 1,
            "attempt": 1,
            "succeeded": true,
            "output": []
        }),
    )
    .await;
    assert_eq!(stale.0, StatusCode::CONFLICT);
    let status: String =
        sqlx::query_scalar("SELECT status::text FROM console_commands WHERE id = $1")
            .bind(second_command_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "cancelled");
}

#[tokio::test]
async fn worker_recovery_marks_claimed_console_commands_unknown_and_never_requeues_them() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "console-recovery").await;
    let server_id = create_server(&pool, "console-recovery").await;
    set_server_state(&pool, server_id, 1, "running", "online").await;
    add_placement(&pool, server_id, worker.id, 1, true).await;
    let owner_id: Uuid = sqlx::query_scalar("SELECT owner_id FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let session_token = format!("console-recovery-session-{server_id}");
    sqlx::query("INSERT INTO sessions(id, token_hash, user_id) VALUES($1, $2, $3)")
        .bind(Uuid::new_v4())
        .bind(blake3::hash(session_token.as_bytes()).as_bytes().to_vec())
        .bind(owner_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET heartbeat_at = now() WHERE id = $1")
        .bind(worker.id)
        .execute(&pool)
        .await
        .unwrap();
    let service = app(pool.clone(), Arc::new(InMemory::new()));
    let uri = format!("/api/v1/servers/{server_id}/console/commands");
    let submitted = service
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(&uri)
                .header(header::COOKIE, format!("kurbis_session={session_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"request_id":Uuid::new_v4(),"command":"say may have run"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(submitted.status(), StatusCode::ACCEPTED);
    let submitted: Value = response_json(submitted).await;
    let command_id = Uuid::parse_str(submitted["id"].as_str().unwrap()).unwrap();
    let claimed = send_json(
        service.clone(),
        Method::POST,
        "/internal/worker/control/console/commands/claim",
        Some(&worker.token),
        json!({}),
    )
    .await;
    assert_eq!(claimed.0, StatusCode::OK);
    assert_eq!(claimed.1["id"], command_id.to_string());

    let recovery = send_json(
        service.clone(),
        Method::POST,
        "/internal/worker/control/recover",
        Some(&worker.token),
        json!({}),
    )
    .await;
    assert_eq!(recovery.0, StatusCode::OK);
    let failed = service
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("{uri}/{command_id}"))
                .header(header::COOKIE, format!("kurbis_session={session_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let failed = response_json(failed).await;
    assert_eq!(failed["status"], "failed");
    assert_eq!(
        failed["error"],
        "execution_outcome_unknown_after_worker_recovery"
    );
    assert!(failed["completed_at"].is_string());

    let claimed_again = send_json(
        service.clone(),
        Method::POST,
        "/internal/worker/control/console/commands/claim",
        Some(&worker.token),
        json!({}),
    )
    .await;
    assert_eq!(claimed_again.0, StatusCode::OK);
    assert_eq!(claimed_again.1, Value::Null);
    let stale_completion = send_json(
        service,
        Method::POST,
        &format!("/internal/worker/control/console/commands/{command_id}/complete"),
        Some(&worker.token),
        json!({
            "generation": 1,
            "attempt": 1,
            "succeeded": true,
            "output": []
        }),
    )
    .await;
    assert_eq!(stale_completion.0, StatusCode::CONFLICT);
}

async fn response_json(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 32 * 1024).await.unwrap()).unwrap()
}
