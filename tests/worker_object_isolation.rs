mod common;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use bytes::Bytes;
use common::{api_state, create_user, test_pool};
use futures::{StreamExt, TryStreamExt, stream};
use kurbis_hoster::{
    contracts::worker::WorkerAssignment,
    control_plane::{
        api::router,
        lifecycle::{healthy_worker_template, heartbeat_worker},
        snapshots::SnapshotStore,
    },
    model::SnapshotReason,
    snapshot::{ManifestFile, SnapshotManifest, physical_object_path},
    worker::snapshot::{SnapshotManager, WorkerObjectClient},
};
use object_store::{ObjectStore, path::Path as ObjectPath};
use sqlx::PgPool;
use tokio::{
    net::TcpListener,
    sync::{Mutex, oneshot},
};
use tower::ServiceExt;
use uuid::Uuid;

static DB_TEST_LOCK: Mutex<()> = Mutex::const_new(());

async fn create_server(pool: &PgPool, owner_id: Uuid, name: &str) -> Uuid {
    let server_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO servers(id, owner_id, subdomain, generation)
         VALUES($1, $2, $3, 1)",
    )
    .bind(server_id)
    .bind(owner_id)
    .bind(format!("{name}-{}", &server_id.simple().to_string()[..10]))
    .execute(pool)
    .await
    .unwrap();
    server_id
}

async fn create_worker(pool: &PgPool, name: &str, ports: (i32, i32)) -> (Uuid, String) {
    let worker_id = Uuid::new_v4();
    heartbeat_worker(
        pool,
        &healthy_worker_template(
            worker_id,
            format!("{name}-{worker_id}"),
            "127.0.0.1".into(),
            8 * 1024 * 1024 * 1024,
            30 * 1024 * 1024 * 1024,
            4000,
            ports.0,
            ports.1,
        ),
    )
    .await
    .unwrap();
    let token = format!("{}{}", worker_id.simple(), Uuid::new_v4().simple())[..43].to_owned();
    let lease_id = worker_lease_id(&token);
    sqlx::query(
        "INSERT INTO worker_credentials(worker_id, api_token_hash, lease_id, lease_expires_at)
         VALUES($1, $2, $3, now() + interval '1 hour')",
    )
    .bind(worker_id)
    .bind(blake3::hash(token.as_bytes()).as_bytes().to_vec())
    .bind(lease_id)
    .execute(pool)
    .await
    .unwrap();
    (worker_id, token)
}

async fn assign_server(
    pool: &PgPool,
    server_id: Uuid,
    worker_id: Uuid,
    generation: i64,
    ports: (i32, i32, i32),
) {
    sqlx::query("UPDATE servers SET generation = $2 WHERE id = $1")
        .bind(server_id)
        .bind(generation)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO placements(server_id, worker_id, generation, lease_token,
                                java_port, bedrock_port, signal_port, active)
         VALUES($1, $2, $3, $4, $5, $6, $7, false)",
    )
    .bind(server_id)
    .bind(worker_id)
    .bind(generation)
    .bind(Uuid::new_v4())
    .bind(ports.0)
    .bind(ports.1)
    .bind(ports.2)
    .execute(pool)
    .await
    .unwrap();
}

async fn create_snapshot_operation(pool: &PgPool, server_id: Uuid, worker_id: Uuid) -> Uuid {
    let operation_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO operations(id, server_id, worker_id, kind, status, generation, attempt, payload)
         VALUES($1, $2, $3, 'snapshot', 'running', 1, 1, jsonb_build_object('reason', 'manual'))",
    )
    .bind(operation_id)
    .bind(server_id)
    .bind(worker_id)
    .execute(pool)
    .await
    .unwrap();
    operation_id
}

async fn create_restart_operation(pool: &PgPool, server_id: Uuid, worker_id: Uuid) -> Uuid {
    let operation_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO operations(id, server_id, worker_id, kind, status, generation, attempt, payload)
         VALUES($1, $2, $3, 'restart', 'running', 1, 1,
                jsonb_build_object('stage', 'stopped', 'reason', 'manual_restart'))",
    )
    .bind(operation_id)
    .bind(server_id)
    .bind(worker_id)
    .execute(pool)
    .await
    .unwrap();
    operation_id
}

async fn publish_test_snapshot(pool: &PgPool, server_id: Uuid, generation: i64, size_bytes: i64) {
    sqlx::query(
        "UPDATE servers SET current_snapshot_generation = $2, storage_bytes = $3
         WHERE id = $1",
    )
    .bind(server_id)
    .bind(generation)
    .bind(size_bytes)
    .execute(pool)
    .await
    .unwrap();
}

fn operation_query(operation_id: Uuid, generation: i64) -> String {
    format!("assignment=operation&operation_id={operation_id}&generation={generation}&attempt=1")
}

fn worker_lease_id(token: &str) -> Uuid {
    Uuid::from_slice(&blake3::hash(token.as_bytes()).as_bytes()[..16]).unwrap()
}

fn placement_query(generation: i64) -> String {
    format!("assignment=placement&generation={generation}")
}

async fn api_request(
    app: &axum::Router,
    method: Method,
    server_id: Uuid,
    key: &str,
    query: &str,
    token: &str,
    body: Body,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!(
                    "/internal/worker/servers/{server_id}/objects/{key}?{query}"
                ))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header("x-worker-lease", worker_lease_id(token).to_string())
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap()
}

fn deterministic_payload(length: usize) -> Vec<u8> {
    let mut state = 0x1234_5678_9abc_def0_u64;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

fn gated_body(bytes: Bytes) -> (Body, oneshot::Receiver<()>, oneshot::Sender<()>) {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let body = Body::from_stream(stream::once(async move {
        let _ = started_tx.send(());
        release_rx.await.unwrap();
        Ok::<_, std::io::Error>(bytes)
    }));
    (body, started_rx, release_tx)
}

#[tokio::test]
async fn restart_operation_can_publish_and_restore_snapshot_objects_through_worker_api() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let owner = create_user(
        &pool,
        "google",
        &format!("worker-object-restart-{}", Uuid::new_v4()),
        "Worker Object Restart",
        "active",
        None,
    )
    .await;
    let server_id = create_server(&pool, owner, "restart-object").await;
    let (worker_id, token) = create_worker(&pool, "restart-worker", (22400, 22500)).await;
    assign_server(&pool, server_id, worker_id, 1, (22401, 22402, 22403)).await;
    let operation_id = create_restart_operation(&pool, server_id, worker_id).await;

    let object_store: Arc<dyn ObjectStore> = Arc::new(object_store::memory::InMemory::new());
    let app = router(api_state(
        pool.clone(),
        "worker-object-restart-test",
        object_store.clone(),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = WorkerObjectClient::new_with_lease(
        &format!("http://{address}"),
        &token,
        true,
        worker_lease_id(&token),
    )
    .unwrap();
    let snapshots =
        SnapshotManager::new_worker(client).for_operation(server_id, operation_id, 1, 1);
    let source = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(source.path().join("world"))
        .await
        .unwrap();
    tokio::fs::write(
        source.path().join("world/level.dat"),
        b"restart snapshot world",
    )
    .await
    .unwrap();

    let snapshot = snapshots
        .create(server_id, source.path(), SnapshotReason::Shutdown)
        .await
        .unwrap();
    let hint_after_publish: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM worker_snapshot_hints
         WHERE worker_id = $1 AND server_id = $2 AND snapshot_generation = $3)",
    )
    .bind(worker_id)
    .bind(server_id)
    .bind(snapshot.generation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        hint_after_publish,
        "published snapshot is reported as local"
    );
    publish_test_snapshot(&pool, server_id, snapshot.generation, snapshot.size_bytes).await;
    let manifest_path = physical_object_path(server_id, &snapshot.manifest_key).unwrap();
    assert!(object_store.head(&manifest_path).await.is_ok());

    let restored = tempfile::tempdir().unwrap();
    sqlx::query("DELETE FROM worker_snapshot_hints WHERE server_id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        snapshots
            .restore_latest(server_id, restored.path())
            .await
            .unwrap(),
        Some(snapshot.generation)
    );
    assert_eq!(
        tokio::fs::read(restored.path().join("world/level.dat"))
            .await
            .unwrap(),
        b"restart snapshot world"
    );
    let hint_after_restore: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM worker_snapshot_hints
         WHERE worker_id = $1 AND server_id = $2 AND snapshot_generation = $3)",
    )
    .bind(worker_id)
    .bind(server_id)
    .bind(snapshot.generation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(hint_after_restore, "restored snapshot is reported as local");
    sqlx::query("DELETE FROM worker_snapshot_hints WHERE server_id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        snapshots
            .local_matches_current(server_id, restored.path())
            .await
            .unwrap()
    );
    let hint_after_validation: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM worker_snapshot_hints
         WHERE worker_id = $1 AND server_id = $2 AND snapshot_generation = $3)",
    )
    .bind(worker_id)
    .bind(server_id)
    .bind(snapshot.generation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        hint_after_validation,
        "a locally validated matching snapshot is reported"
    );
    sqlx::query("DELETE FROM worker_snapshot_hints WHERE server_id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    let explicit_restore = tempfile::tempdir().unwrap();
    assert_eq!(
        snapshots
            .restore_generation(server_id, snapshot.generation, explicit_restore.path())
            .await
            .unwrap(),
        snapshot.generation
    );
    assert_eq!(
        tokio::fs::read(explicit_restore.path().join("world/level.dat"))
            .await
            .unwrap(),
        b"restart snapshot world"
    );
    let hint_after_explicit_restore: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM worker_snapshot_hints
         WHERE worker_id = $1 AND server_id = $2 AND snapshot_generation = $3)",
    )
    .bind(worker_id)
    .bind(server_id)
    .bind(snapshot.generation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        hint_after_explicit_restore,
        "an explicit generation restore is reported as local"
    );

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
    server_task.abort();
}

#[tokio::test]
async fn worker_object_access_is_server_and_assignment_scoped_with_streamed_snapshot_and_export() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let owner_a = create_user(
        &pool,
        "google",
        &format!("worker-object-a-{}", Uuid::new_v4()),
        "Worker Object A",
        "active",
        None,
    )
    .await;
    let owner_b = create_user(
        &pool,
        "google",
        &format!("worker-object-b-{}", Uuid::new_v4()),
        "Worker Object B",
        "active",
        None,
    )
    .await;
    let server_a = create_server(&pool, owner_a, "object-a").await;
    let server_b = create_server(&pool, owner_b, "object-b").await;
    let (worker_a, token_a) = create_worker(&pool, "worker-a", (22000, 22100)).await;
    let (worker_b, token_b) = create_worker(&pool, "worker-b", (22200, 22300)).await;
    assign_server(&pool, server_a, worker_a, 1, (22001, 22002, 22003)).await;
    assign_server(&pool, server_b, worker_b, 1, (22201, 22202, 22203)).await;
    let operation_a = create_snapshot_operation(&pool, server_a, worker_a).await;
    let operation_b = create_snapshot_operation(&pool, server_b, worker_b).await;

    let object_store: Arc<dyn ObjectStore> = Arc::new(object_store::memory::InMemory::new());
    let app_state = api_state(
        pool.clone(),
        "worker-object-internal-test",
        object_store.clone(),
    );
    let app = router(app_state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    let serving_app = app.clone();
    let server_task = tokio::spawn(async move {
        axum::serve(listener, serving_app).await.unwrap();
    });

    let b_payload = b"private object for worker B";
    let b_hash = blake3::hash(b_payload).to_hex().to_string();
    let b_key = format!("chunks/{b_hash}");
    let b_physical_path = physical_object_path(server_b, &b_key).unwrap();
    object_store
        .put(
            &b_physical_path,
            bytes::Bytes::from_static(b_payload).into(),
        )
        .await
        .unwrap();
    let b_query = operation_query(operation_b, 1);
    for method in [Method::GET, Method::HEAD, Method::DELETE] {
        let response = api_request(
            &app,
            method,
            server_b,
            &b_key,
            &b_query,
            &token_a,
            Body::empty(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    let response = api_request(
        &app,
        Method::PUT,
        server_b,
        &b_key,
        &b_query,
        &token_a,
        Body::from("attempted overwrite by worker A"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        object_store
            .get(&b_physical_path)
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        b_payload
    );

    let unplaced_server = create_server(&pool, owner_a, "unplaced-delete").await;
    let delete_operation = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO operations(id, server_id, worker_id, kind, status, generation, attempt)
         VALUES($1, $2, $3, 'delete', 'running', 1, 1)",
    )
    .bind(delete_operation)
    .bind(unplaced_server)
    .bind(worker_a)
    .execute(&pool)
    .await
    .unwrap();
    let unplaced_manifest = format!("snapshots/1-{}.json", "c".repeat(64));
    let unplaced_path = physical_object_path(unplaced_server, &unplaced_manifest).unwrap();
    object_store
        .put(
            &unplaced_path,
            bytes::Bytes::from_static(b"offline delete manifest").into(),
        )
        .await
        .unwrap();
    let offline_query = operation_query(delete_operation, 1);
    let offline_read = api_request(
        &app,
        Method::GET,
        unplaced_server,
        &unplaced_manifest,
        &offline_query,
        &token_a,
        Body::empty(),
    )
    .await;
    assert_eq!(offline_read.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(offline_read.into_body(), 1024)
            .await
            .unwrap()
            .as_ref(),
        b"offline delete manifest"
    );
    let offline_delete = api_request(
        &app,
        Method::DELETE,
        unplaced_server,
        &unplaced_manifest,
        &offline_query,
        &token_a,
        Body::empty(),
    )
    .await;
    assert_eq!(offline_delete.status(), StatusCode::NO_CONTENT);
    assert!(object_store.head(&unplaced_path).await.is_err());

    let unplaced_export_server = create_server(&pool, owner_a, "unplaced-export").await;
    let unplaced_export_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO world_exports(id, server_id, requested_by, status, stage,
                                  source_generation, worker_id, attempt)
         VALUES($1, $2, $3, 'running', 'archiving', 1, $4, 1)",
    )
    .bind(unplaced_export_id)
    .bind(unplaced_export_server)
    .bind(owner_a)
    .bind(worker_a)
    .execute(&pool)
    .await
    .unwrap();
    let unplaced_export_key = format!("exports/{unplaced_export_id}/1.zip");
    let unplaced_export_query =
        format!("assignment=export&export_id={unplaced_export_id}&generation=1&attempt=1");
    let unplaced_export_put = api_request(
        &app,
        Method::PUT,
        unplaced_export_server,
        &unplaced_export_key,
        &unplaced_export_query,
        &token_a,
        Body::from("unplaced export payload"),
    )
    .await;
    assert_eq!(unplaced_export_put.status(), StatusCode::NO_CONTENT);
    let unplaced_export_get = api_request(
        &app,
        Method::GET,
        unplaced_export_server,
        &unplaced_export_key,
        &unplaced_export_query,
        &token_a,
        Body::empty(),
    )
    .await;
    assert_eq!(unplaced_export_get.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(unplaced_export_get.into_body(), 1024)
            .await
            .unwrap()
            .as_ref(),
        b"unplaced export payload"
    );
    sqlx::query("UPDATE world_exports SET status = 'cancelled', stage = 'cancelled' WHERE id = $1")
        .bind(unplaced_export_id)
        .execute(&pool)
        .await
        .unwrap();
    object_store
        .delete(&physical_object_path(unplaced_export_server, &unplaced_export_key).unwrap())
        .await
        .unwrap();

    for key in [
        "registry/credentials",
        "chunks/../registry/credentials",
        "chunks/%2e%2e%2fregistry%2fcredentials",
        "servers/123e4567-e89b-12d3-a456-426614174000/chunks/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        let response = api_request(
            &app,
            Method::GET,
            server_a,
            key,
            &operation_query(operation_a, 1),
            &token_a,
            Body::empty(),
        )
        .await;
        assert!(
            !response.status().is_success(),
            "worker object endpoint accepted {key}"
        );
    }
    let list_response = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/internal/worker/servers/{server_a}/objects?{}",
                operation_query(operation_a, 1)
            ))
            .header(header::AUTHORIZATION, format!("Bearer {token_a}"))
            .header("x-worker-lease", worker_lease_id(&token_a).to_string())
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::NOT_FOUND);

    let local_base_url = format!("http://{address}");
    let client_a = WorkerObjectClient::new_with_lease(
        &local_base_url,
        &token_a,
        true,
        worker_lease_id(&token_a),
    )
    .unwrap();
    let snapshots =
        SnapshotManager::new_worker(client_a.clone()).for_operation(server_a, operation_a, 1, 1);
    let source = tempfile::tempdir().unwrap();
    let world_payload = deterministic_payload(12 * 1024 * 1024);
    tokio::fs::create_dir_all(source.path().join("world"))
        .await
        .unwrap();
    tokio::fs::write(source.path().join("world/level.dat"), &world_payload)
        .await
        .unwrap();
    let snapshot = snapshots
        .create(server_a, source.path(), SnapshotReason::Manual)
        .await
        .unwrap();
    publish_test_snapshot(&pool, server_a, snapshot.generation, snapshot.size_bytes).await;
    assert!(snapshot.manifest_key.starts_with("snapshots/"));
    let stored_manifest = object_store
        .get(&physical_object_path(server_a, &snapshot.manifest_key).unwrap())
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(
        blake3::hash(&stored_manifest).to_hex().as_str(),
        snapshot.checksum
    );

    let symlink_restore = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    tokio::fs::write(outside.path().join("sentinel"), b"keep outside data")
        .await
        .unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path(), symlink_restore.path().join("world")).unwrap();
        assert!(
            snapshots
                .restore_latest(server_a, symlink_restore.path())
                .await
                .is_err()
        );
        assert_eq!(
            tokio::fs::read(outside.path().join("sentinel"))
                .await
                .unwrap(),
            b"keep outside data"
        );
    }
    let restored = tempfile::tempdir().unwrap();
    assert_eq!(
        snapshots
            .restore_latest(server_a, restored.path())
            .await
            .unwrap(),
        Some(snapshot.generation)
    );
    assert_eq!(
        tokio::fs::read(restored.path().join("world/level.dat"))
            .await
            .unwrap(),
        world_payload
    );
    assert!(
        snapshots
            .local_matches_current(server_a, restored.path())
            .await
            .unwrap()
    );

    let export_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO world_exports(id, server_id, requested_by, status, stage,
                                  source_generation, worker_id, attempt)
         VALUES($1, $2, $3, 'running', 'archiving', $4, $5, 3)",
    )
    .bind(export_id)
    .bind(server_a)
    .bind(owner_a)
    .bind(snapshot.generation)
    .bind(worker_a)
    .execute(&pool)
    .await
    .unwrap();
    let export_manager = SnapshotManager::new_worker(client_a.clone()).for_export(
        server_a,
        export_id,
        snapshot.generation,
        3,
    );
    let staging = tempfile::tempdir().unwrap();
    let archive = export_manager
        .create_world_archive(export_id, server_a, snapshot.generation, 3, staging.path())
        .await
        .unwrap();
    assert_eq!(archive.archive_key, format!("exports/{export_id}/3.zip"));
    export_manager.upload_world_archive(&archive).await.unwrap();
    let archive_bytes = tokio::fs::read(&archive.path).await.unwrap();
    assert!(archive_bytes.len() > 8 * 1024 * 1024);
    let physical_archive = physical_object_path(server_a, &archive.archive_key).unwrap();
    assert_eq!(
        object_store
            .get(&physical_archive)
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        archive_bytes
    );
    let export_store = client_a.scoped(
        server_a,
        WorkerAssignment::Export {
            export_id,
            generation: snapshot.generation,
            attempt: 3,
        },
    );
    let archive_key = ObjectPath::from(archive.archive_key.clone());
    assert_eq!(
        export_store.head(&archive_key).await.unwrap().size,
        archive_bytes.len() as u64
    );
    assert_eq!(
        export_store
            .get(&archive_key)
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        archive_bytes
    );
    let stale_archive_key = ObjectPath::from(format!("exports/{export_id}/2.zip"));
    assert!(
        export_store
            .put(
                &stale_archive_key,
                bytes::Bytes::from_static(b"stale attempt").into()
            )
            .await
            .is_err()
    );
    sqlx::query("UPDATE world_exports SET stage = 'ready' WHERE id = $1")
        .bind(export_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        export_store
            .put(
                &archive_key,
                bytes::Bytes::from_static(b"write outside archiving stage").into()
            )
            .await
            .is_err()
    );
    sqlx::query("UPDATE world_exports SET stage = 'archiving' WHERE id = $1")
        .bind(export_id)
        .execute(&pool)
        .await
        .unwrap();

    let in_flight_archive = Bytes::from(deterministic_payload(10 * 1024 * 1024));
    let (upload_body, upload_started, release_upload) = gated_body(in_flight_archive.clone());
    let upload_request = Request::builder()
        .method(Method::PUT)
        .uri(format!(
            "/internal/worker/servers/{server_a}/objects/{}?assignment=export&export_id={export_id}&generation={}&attempt=3",
            archive.archive_key,
            snapshot.generation
        ))
        .header(header::AUTHORIZATION, format!("Bearer {token_a}"))
        .header("x-worker-lease", worker_lease_id(&token_a).to_string())
        .body(upload_body)
        .unwrap();
    let upload_task = tokio::spawn(app.clone().oneshot(upload_request));
    upload_started.await.unwrap();
    let gc_pool = pool.clone();
    let gc_store = object_store.clone();
    let gc_task = tokio::spawn(async move {
        SnapshotStore::collect_garbage_with(&gc_pool, gc_store)
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory'
                   AND classid = 724515 AND objid = 0 AND mode = 'ExclusiveLock'
                   AND NOT granted",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if waiting > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("global GC must wait for an active export upload");
    assert!(!gc_task.is_finished());
    release_upload.send(()).unwrap();
    let upload_response = upload_task.await.unwrap().unwrap();
    assert_eq!(upload_response.status(), StatusCode::NO_CONTENT);
    let gc_report = tokio::time::timeout(Duration::from_secs(5), gc_task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(gc_report.exports_deleted, 0);
    assert_eq!(gc_report.staging_deleted, 0);
    assert_eq!(
        object_store
            .get(&physical_object_path(server_a, &archive.archive_key).unwrap())
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        in_flight_archive
    );

    export_manager
        .delete_world_archive(server_a, &archive.archive_key)
        .await
        .unwrap();
    assert!(export_store.head(&archive_key).await.is_err());
    export_manager.remove_world_archive_staging(&archive).await;

    let client_b = WorkerObjectClient::new_with_lease(
        &local_base_url,
        &token_b,
        true,
        worker_lease_id(&token_b),
    )
    .unwrap();

    let stalled_server = create_server(&pool, owner_a, "stalled-read").await;
    assign_server(&pool, stalled_server, worker_a, 1, (22010, 22011, 22012)).await;
    let stalled_payload = deterministic_payload(24 * 1024 * 1024);
    let stalled_key = format!("chunks/{}", blake3::hash(&stalled_payload).to_hex());
    object_store
        .put(
            &physical_object_path(stalled_server, &stalled_key).unwrap(),
            Bytes::from(stalled_payload).into(),
        )
        .await
        .unwrap();
    let stalled_response = api_request(
        &app,
        Method::GET,
        stalled_server,
        &stalled_key,
        &placement_query(1),
        &token_a,
        Body::empty(),
    )
    .await;
    assert_eq!(stalled_response.status(), StatusCode::OK);
    let mut stalled_stream = stalled_response.into_body().into_data_stream();
    let first_segment = tokio::time::timeout(Duration::from_secs(5), stalled_stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first_segment.len(), 8 * 1024 * 1024);
    tokio::time::timeout(Duration::from_secs(3), async {
        sqlx::query("UPDATE servers SET generation = 2 WHERE id = $1")
            .bind(stalled_server)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE placements SET worker_id = $2, generation = 2, lease_token = $3,
                 java_port = 22210, bedrock_port = 22211, signal_port = 22212
             WHERE server_id = $1",
        )
        .bind(stalled_server)
        .bind(worker_b)
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();
    })
    .await
    .expect("a stalled download must not hold assignment locks");
    drop(stalled_stream);

    let valid_before_reassignment = api_request(
        &app,
        Method::GET,
        server_a,
        &snapshot.manifest_key,
        &operation_query(operation_a, 1),
        &token_a,
        Body::empty(),
    )
    .await;
    assert_eq!(valid_before_reassignment.status(), StatusCode::OK);
    let _ = to_bytes(valid_before_reassignment.into_body(), 20 * 1024 * 1024)
        .await
        .unwrap();

    let in_flight_bytes = Bytes::from(deterministic_payload(10 * 1024 * 1024));
    let in_flight_key = format!("chunks/{}", blake3::hash(&in_flight_bytes).to_hex());
    let (in_flight_body, started_rx, release_tx) = gated_body(in_flight_bytes);
    let in_flight_request = Request::builder()
        .method(Method::PUT)
        .uri(format!(
            "/internal/worker/servers/{server_a}/objects/{in_flight_key}?{}",
            operation_query(operation_a, 1)
        ))
        .header(header::AUTHORIZATION, format!("Bearer {token_a}"))
        .header("x-worker-lease", worker_lease_id(&token_a).to_string())
        .body(in_flight_body)
        .unwrap();
    let in_flight_task = tokio::spawn(app.clone().oneshot(in_flight_request));
    started_rx.await.unwrap();
    let gc_pool = pool.clone();
    let gc_store = object_store.clone();
    let gc_task = tokio::spawn(async move {
        SnapshotStore::collect_garbage_with(&gc_pool, gc_store)
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory'
                   AND classid = 724515 AND objid = 0 AND mode = 'ExclusiveLock'
                   AND NOT granted",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if waiting > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("global GC must wait for the active worker upload");
    assert!(!gc_task.is_finished());
    sqlx::query("UPDATE servers SET generation = 2 WHERE id = $1")
        .bind(server_a)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE placements SET worker_id = $2, generation = 2, lease_token = $3,
             java_port = 22204, bedrock_port = 22205, signal_port = 22206
         WHERE server_id = $1",
    )
    .bind(server_a)
    .bind(worker_b)
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE world_exports SET worker_id = $2 WHERE id = $1")
        .bind(export_id)
        .bind(worker_b)
        .execute(&pool)
        .await
        .unwrap();
    release_tx.send(()).unwrap();
    let in_flight_response = in_flight_task.await.unwrap().unwrap();
    assert_eq!(in_flight_response.status(), StatusCode::NOT_FOUND);
    assert!(
        object_store
            .head(&physical_object_path(server_a, &in_flight_key).unwrap())
            .await
            .is_err()
    );
    let staged_objects: Vec<_> = object_store
        .list(Some(&ObjectPath::from("control-plane-object-staging")))
        .try_collect()
        .await
        .unwrap();
    assert!(staged_objects.is_empty());
    let gc_report = tokio::time::timeout(Duration::from_secs(5), gc_task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(gc_report.staging_deleted, 0);
    let old_worker_request = api_request(
        &app,
        Method::GET,
        server_a,
        &snapshot.manifest_key,
        &operation_query(operation_a, 1),
        &token_a,
        Body::empty(),
    )
    .await;
    assert_eq!(old_worker_request.status(), StatusCode::NOT_FOUND);
    assert!(export_store.head(&archive_key).await.is_err());
    let reassigned = SnapshotManager::new_worker(client_b.clone()).for_placement(server_a, 2);
    let reassigned_restore = tempfile::tempdir().unwrap();
    assert_eq!(
        reassigned
            .restore_latest(server_a, reassigned_restore.path())
            .await
            .unwrap(),
        Some(snapshot.generation)
    );
    assert_eq!(
        tokio::fs::read(reassigned_restore.path().join("world/level.dat"))
            .await
            .unwrap(),
        world_payload
    );

    sqlx::query("UPDATE worker_credentials SET revoked_at = now() WHERE worker_id = $1")
        .bind(worker_b)
        .execute(&pool)
        .await
        .unwrap();
    let revoked = api_request(
        &app,
        Method::GET,
        server_a,
        &snapshot.manifest_key,
        &placement_query(2),
        &token_b,
        Body::empty(),
    )
    .await;
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);

    // A manifest is untrusted object data. Validate every entry before the
    // first restore write so a valid prefix cannot escape when a later entry
    // contains traversal or a forged chunk reference.
    let forged_generation = 2_i64;
    let forged_payload = b"valid entry must not be written";
    let forged_hash = blake3::hash(forged_payload).to_hex().to_string();
    let forged_manifest = SnapshotManifest {
        server_id: server_a,
        generation: forged_generation,
        files: vec![
            ManifestFile {
                path: "world/level.dat".to_owned(),
                size: forged_payload.len() as u64,
                blake3: forged_hash.clone(),
            },
            ManifestFile {
                path: "../outside-sentinel".to_owned(),
                size: 0,
                blake3: forged_hash.clone(),
            },
        ],
    };
    let forged_bytes = serde_json::to_vec(&forged_manifest).unwrap();
    let forged_checksum = blake3::hash(&forged_bytes).to_hex().to_string();
    let forged_key = format!("snapshots/{forged_generation}-{forged_checksum}.json");
    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete)
         VALUES($1, $2, $3, $4, $5, $6, 'periodic', true)",
    )
    .bind(Uuid::new_v4())
    .bind(server_a)
    .bind(forged_generation)
    .bind(&forged_key)
    .bind(forged_payload.len() as i64)
    .bind(&forged_checksum)
    .execute(&pool)
    .await
    .unwrap();
    object_store
        .put(
            &physical_object_path(server_a, &format!("chunks/{forged_hash}")).unwrap(),
            Bytes::from_static(forged_payload).into(),
        )
        .await
        .unwrap();
    object_store
        .put(
            &physical_object_path(server_a, &forged_key).unwrap(),
            Bytes::from(forged_bytes).into(),
        )
        .await
        .unwrap();
    publish_test_snapshot(
        &pool,
        server_a,
        forged_generation,
        forged_payload.len() as i64,
    )
    .await;
    let forged_restore = tempfile::tempdir().unwrap();
    let outside_sentinel = forged_restore
        .path()
        .parent()
        .unwrap()
        .join("outside-sentinel");
    tokio::fs::write(&outside_sentinel, b"unchanged")
        .await
        .unwrap();
    let control_plane_snapshots = SnapshotStore::new(pool.clone(), object_store.clone());
    assert!(
        control_plane_snapshots
            .restore_latest(server_a, forged_restore.path())
            .await
            .is_err()
    );
    assert!(!forged_restore.path().join("world/level.dat").exists());
    assert_eq!(
        tokio::fs::read(&outside_sentinel).await.unwrap(),
        b"unchanged"
    );
    let _ = tokio::fs::remove_file(outside_sentinel).await;

    for owner_id in [owner_a, owner_b] {
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(owner_id)
            .execute(&pool)
            .await
            .unwrap();
    }
    for worker_id in [worker_a, worker_b] {
        sqlx::query("DELETE FROM workers WHERE id = $1")
            .bind(worker_id)
            .execute(&pool)
            .await
            .unwrap();
    }
    server_task.abort();
}
