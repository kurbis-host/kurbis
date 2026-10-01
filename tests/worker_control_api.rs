mod common;

use std::sync::Arc;

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
}

async fn create_worker(pool: &PgPool, label: &str) -> TestWorker {
    let id = Uuid::new_v4();
    let token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
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
        "INSERT INTO worker_auth.credentials(worker_id, api_token_hash, database_role)
         VALUES($1, $2, $3)",
    )
    .bind(id)
    .bind(blake3::hash(token.as_bytes()).as_bytes().to_vec())
    .bind(format!("test_{}", id.simple()))
    .execute(pool)
    .await
    .unwrap();
    TestWorker { id, token }
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
            },
            server_id,
            "idle_timeout",
        )
        .await
        .unwrap();
    assert_eq!(observed, kurbis_hoster::model::ObservedState::Stopping);
}

#[tokio::test]
async fn worker_stop_route_matches_owner_command_fence_and_rejects_foreign_actor() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "route-stop").await;
    let foreign = create_worker(&pool, "route-stop-foreign").await;
    let server_id = create_server(&pool, "route-stop").await;
    set_server_state(&pool, server_id, 1, "running", "online").await;
    add_placement(&pool, server_id, worker.id, 1, true).await;
    let service = app(pool.clone(), Arc::new(InMemory::new()));
    let uri = format!("/internal/worker/control/servers/{server_id}/stop");
    let body = json!({"generation": 1, "reason": "idle_timeout"});

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
    assert_eq!(status, StatusCode::OK);
    let observed: String =
        sqlx::query_scalar("SELECT observed_state::text FROM servers WHERE id = $1")
            .bind(server_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(observed, "stopping");
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
        &format!("/internal/worker/control/servers/{server_b}/stop"),
        Some(&worker_a.token),
        json!({ "generation": 7, "reason":"idle_timeout" }),
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
