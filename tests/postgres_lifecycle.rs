mod common;

use common::{create_user, test_pool};
use kurbis_hoster::{
    control_plane::api::resolve_subdomain,
    control_plane::lifecycle::{
        Lifecycle, claim_operation, complete_start, complete_stop, healthy_worker_template,
        heartbeat_worker, mark_runtime_lost, recover_running_operations, retry_operation,
    },
    model::{ObservedState, OperationKind},
};
use sqlx::PgPool;
use tokio::sync::Mutex;
use uuid::Uuid;

static DB_TEST_LOCK: Mutex<()> = Mutex::const_new(());

async fn clean_test_rows(pool: &PgPool) {
    sqlx::query(
        "DELETE FROM users WHERE id IN (
             SELECT user_id FROM identities WHERE provider = 'google' AND subject LIKE 'lifecycle-test-%'
         )",
    )
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "DELETE FROM workers
         WHERE name LIKE 'worker-%' OR name LIKE 'loss-worker-%'",
    )
    .execute(pool)
    .await
    .unwrap();
}

async fn create_test_server(pool: &PgPool, prefix: &str) -> (Uuid, Uuid) {
    let identity_subject = Uuid::new_v4();
    let server_id = Uuid::new_v4();
    let user_id = create_user(
        pool,
        "google",
        &format!("lifecycle-test-{identity_subject}"),
        "Lifecycle Test",
        "active",
        None,
    )
    .await;
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
        .bind(server_id)
        .bind(user_id)
        .bind(format!(
            "{prefix}-{}",
            &server_id.simple().to_string()[..12]
        ))
        .execute(pool)
        .await
        .unwrap();
    (user_id, server_id)
}

async fn create_test_worker(pool: &PgPool, prefix: &str, port_min: i32) -> Uuid {
    let worker_id = Uuid::new_v4();
    let worker = healthy_worker_template(
        worker_id,
        format!("{prefix}-{worker_id}"),
        "127.0.0.1".into(),
        8 * 1024 * 1024 * 1024,
        40 * 1024 * 1024 * 1024,
        4000,
        port_min,
        port_min + 100,
    );
    heartbeat_worker(pool, &worker).await.unwrap();
    worker_id
}

async fn publish_final_snapshot(
    pool: &PgPool,
    op: &kurbis_hoster::model::OperationRecord,
    generation: i64,
    size: i64,
) {
    sqlx::query("INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete) VALUES($1,$2,$3,'fixture-manifest',$4,'fixture-checksum','shutdown',true)")
        .bind(Uuid::new_v4()).bind(op.server_id).bind(generation).bind(size).execute(pool).await.unwrap();
    sqlx::query(
        "UPDATE servers SET current_snapshot_generation = $2, storage_bytes = $3 WHERE id = $1",
    )
    .bind(op.server_id)
    .bind(generation)
    .bind(size)
    .execute(pool)
    .await
    .unwrap();
}

async fn cleanup_test_entities(pool: &PgPool, user_id: Uuid, worker_ids: &[Uuid]) {
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(pool)
        .await
        .unwrap();
    for worker_id in worker_ids {
        sqlx::query("DELETE FROM workers WHERE id = $1")
            .bind(worker_id)
            .execute(pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn runtime_loss_deactivates_the_placement() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;

    let server_id = Uuid::new_v4();
    let worker_id = Uuid::new_v4();
    let user_id = create_user(
        &pool,
        "google",
        &format!("lifecycle-test-{server_id}"),
        "Runtime Loss Test",
        "active",
        None,
    )
    .await;
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
        .bind(server_id)
        .bind(user_id)
        .bind(format!("loss-{}", &server_id.simple().to_string()[..12]))
        .execute(&pool)
        .await
        .unwrap();
    let worker = healthy_worker_template(
        worker_id,
        format!("loss-worker-{worker_id}"),
        "127.0.0.1".into(),
        8 * 1024 * 1024 * 1024,
        40 * 1024 * 1024 * 1024,
        4000,
        25000,
        25100,
    );
    heartbeat_worker(&pool, &worker).await.unwrap();
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(server_id, "test").await.unwrap();
    let start = claim_operation(&pool, worker_id)
        .await
        .unwrap()
        .expect("start operation");
    complete_start(&pool, &start, 0).await.unwrap();

    mark_runtime_lost(&pool, worker_id, server_id, "runtime exited")
        .await
        .unwrap();

    let (state, active, desired, reserved_memory): (
        ObservedState,
        bool,
        kurbis_hoster::model::DesiredState,
        i64,
    ) = sqlx::query_as(
        "SELECT s.observed_state, p.active, s.desired_state, w.reserved_memory_bytes
         FROM servers s
         JOIN placements p ON p.server_id = s.id
         JOIN workers w ON w.id = p.worker_id
         WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, ObservedState::Failed);
    assert_eq!(desired, kurbis_hoster::model::DesiredState::Stopped);
    assert!(!active);
    assert_eq!(reserved_memory, 0);

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
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
async fn lifecycle_start_and_stop_release_worker_capacity() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;

    let server_id = Uuid::new_v4();
    let worker_id = Uuid::new_v4();
    let user_id = create_user(
        &pool,
        "google",
        &format!("lifecycle-test-{server_id}"),
        "Lifecycle Test",
        "active",
        None,
    )
    .await;
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
        .bind(server_id)
        .bind(user_id)
        .bind(format!("test-{}", &server_id.simple().to_string()[..12]))
        .execute(&pool)
        .await
        .unwrap();

    let worker = healthy_worker_template(
        worker_id,
        format!("worker-{worker_id}"),
        "127.0.0.1".into(),
        8 * 1024 * 1024 * 1024,
        40 * 1024 * 1024 * 1024,
        4000,
        24000,
        24100,
    );
    heartbeat_worker(&pool, &worker).await.unwrap();

    let lifecycle = Lifecycle::new(pool.clone());
    assert_eq!(
        lifecycle.request_start(server_id, "test").await.unwrap(),
        ObservedState::Restoring
    );
    let start = claim_operation(&pool, worker_id)
        .await
        .unwrap()
        .expect("start operation");
    assert_eq!(start.kind, OperationKind::Start);
    complete_start(&pool, &start, 123).await.unwrap();

    let (state, active, reserved_memory, reserved_disk, reserved_cpu): (
        ObservedState,
        bool,
        i64,
        i64,
        i64,
    ) = sqlx::query_as(
        "SELECT s.observed_state, p.active, w.reserved_memory_bytes,
                w.reserved_disk_bytes, w.reserved_cpu_millis
         FROM servers s
         JOIN placements p ON p.server_id = s.id
         JOIN workers w ON w.id = p.worker_id
         WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, ObservedState::Online);
    assert!(active);
    assert_eq!(reserved_memory, kurbis_hoster::model::SERVER_MEMORY_BYTES);
    assert_eq!(reserved_disk, kurbis_hoster::model::SERVER_STORAGE_BYTES);
    assert_eq!(reserved_cpu, kurbis_hoster::model::SERVER_CPU_MILLIS);

    assert_eq!(
        lifecycle.request_stop(server_id, "test").await.unwrap(),
        ObservedState::Stopping
    );
    let stop = claim_operation(&pool, worker_id)
        .await
        .unwrap()
        .expect("stop operation");
    assert_eq!(stop.kind, OperationKind::Stop);
    publish_final_snapshot(&pool, &stop, 1, 456).await;
    complete_stop(&pool, &stop, 1, 456).await.unwrap();

    let (state, active, reserved_memory, reserved_disk, reserved_cpu): (
        ObservedState,
        bool,
        i64,
        i64,
        i64,
    ) = sqlx::query_as(
        "SELECT s.observed_state, p.active, w.reserved_memory_bytes,
                w.reserved_disk_bytes, w.reserved_cpu_millis
         FROM servers s
         JOIN placements p ON p.server_id = s.id
         JOIN workers w ON w.id = p.worker_id
         WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, ObservedState::Sleeping);
    assert!(!active);
    assert_eq!(reserved_memory, 0);
    assert_eq!(reserved_disk, 0);
    assert_eq!(reserved_cpu, 0);

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
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
async fn simultaneous_start_requests_create_one_start_and_one_reservation() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;

    let (user_id, server_id) = create_test_server(&pool, "dupe").await;
    let worker_id = create_test_worker(&pool, "worker-dupe", 26000).await;

    let lifecycle = Lifecycle::new(pool.clone());
    let mut joins = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let lifecycle = lifecycle.clone();
        joins.spawn(async move {
            lifecycle
                .request_start(server_id, "simultaneous_join")
                .await
        });
    }
    while let Some(result) = joins.join_next().await {
        result.unwrap().unwrap();
    }

    let live_ops: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations
         WHERE server_id = $1 AND kind = 'start' AND status IN ('pending', 'running')",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let (reserved_memory, reserved_disk, reserved_cpu): (i64, i64, i64) = sqlx::query_as(
        "SELECT reserved_memory_bytes, reserved_disk_bytes, reserved_cpu_millis
         FROM workers WHERE id = $1",
    )
    .bind(worker_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(live_ops, 1);
    assert_eq!(reserved_memory, kurbis_hoster::model::SERVER_MEMORY_BYTES);
    assert_eq!(reserved_disk, kurbis_hoster::model::SERVER_STORAGE_BYTES);
    assert_eq!(reserved_cpu, kurbis_hoster::model::SERVER_CPU_MILLIS);

    cleanup_test_entities(&pool, user_id, &[worker_id]).await;
}

#[tokio::test]
async fn start_during_stop_requests_restart_without_reassigning_the_active_placement() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;

    let (user_id, server_id) = create_test_server(&pool, "restart-race").await;
    let worker_id = create_test_worker(&pool, "worker-restart-race", 26200).await;
    let lifecycle = Lifecycle::new(pool.clone());

    lifecycle.request_start(server_id, "test").await.unwrap();
    let start = claim_operation(&pool, worker_id)
        .await
        .unwrap()
        .expect("start operation");
    complete_start(&pool, &start, 0).await.unwrap();
    lifecycle.request_stop(server_id, "test").await.unwrap();

    assert_eq!(
        lifecycle
            .request_start(server_id, "join_during_stop")
            .await
            .unwrap(),
        ObservedState::Stopping
    );
    let desired: kurbis_hoster::model::DesiredState =
        sqlx::query_scalar("SELECT desired_state FROM servers WHERE id = $1")
            .bind(server_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(desired, kurbis_hoster::model::DesiredState::Running);

    let stop = claim_operation(&pool, worker_id)
        .await
        .unwrap()
        .expect("stop operation");
    publish_final_snapshot(&pool, &stop, 1, 0).await;
    complete_stop(&pool, &stop, 1, 0).await.unwrap();
    let state: ObservedState =
        sqlx::query_scalar("SELECT observed_state FROM servers WHERE id = $1")
            .bind(server_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, ObservedState::Queued);

    assert_eq!(lifecycle.drain_queue_once().await.unwrap(), 1);
    let generation: i64 = sqlx::query_scalar("SELECT generation FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(generation, start.generation + 1);

    cleanup_test_entities(&pool, user_id, &[worker_id]).await;
}

#[tokio::test]
async fn active_failed_placement_is_fenced_from_reassignment() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;

    let (user_id, server_id) = create_test_server(&pool, "fenced").await;
    let first_worker = create_test_worker(&pool, "worker-fenced-a", 26400).await;
    let lifecycle = Lifecycle::new(pool.clone());

    lifecycle.request_start(server_id, "test").await.unwrap();
    let start = claim_operation(&pool, first_worker)
        .await
        .unwrap()
        .expect("start operation");
    complete_start(&pool, &start, 0).await.unwrap();
    let second_worker = create_test_worker(&pool, "worker-fenced-b", 26600).await;
    sqlx::query("UPDATE servers SET observed_state = 'failed' WHERE id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();

    let error = lifecycle
        .request_start(server_id, "retry")
        .await
        .expect_err("active placement must fence reassignment");
    assert!(error.to_string().contains("active fenced placement"));
    let placement_worker: Uuid =
        sqlx::query_scalar("SELECT worker_id FROM placements WHERE server_id = $1")
            .bind(server_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let second_reserved: i64 =
        sqlx::query_scalar("SELECT reserved_memory_bytes FROM workers WHERE id = $1")
            .bind(second_worker)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(placement_worker, first_worker);
    assert_eq!(second_reserved, 0);

    mark_runtime_lost(&pool, first_worker, server_id, "confirmed stopped")
        .await
        .unwrap();
    assert_eq!(
        lifecycle
            .request_start(server_id, "retry_after_fence")
            .await
            .unwrap(),
        ObservedState::Restoring
    );

    cleanup_test_entities(&pool, user_id, &[first_worker, second_worker]).await;
}

#[tokio::test]
async fn stale_worker_route_is_explicitly_unavailable_without_releasing_the_fence() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;

    let (user_id, server_id) = create_test_server(&pool, "stale-route").await;
    let worker_id = create_test_worker(&pool, "worker-stale-route", 26800).await;
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(server_id, "test").await.unwrap();
    let start = claim_operation(&pool, worker_id)
        .await
        .unwrap()
        .expect("start operation");
    complete_start(&pool, &start, 0).await.unwrap();
    sqlx::query("UPDATE workers SET heartbeat_at = now() - interval '31 seconds' WHERE id = $1")
        .bind(worker_id)
        .execute(&pool)
        .await
        .unwrap();

    let subdomain: String = sqlx::query_scalar("SELECT subdomain FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let route = resolve_subdomain(&pool, &subdomain)
        .await
        .unwrap()
        .expect("route");
    assert_eq!(route.state, ObservedState::Online);
    assert_eq!(route.worker_healthy, Some(false));
    assert!(route.worker_address.is_none());
    let active: bool = sqlx::query_scalar("SELECT active FROM placements WHERE server_id = $1")
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(active);

    cleanup_test_entities(&pool, user_id, &[worker_id]).await;
}

#[tokio::test]
async fn interrupted_worker_operation_is_reclaimed_only_after_explicit_recovery() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;

    let (user_id, server_id) = create_test_server(&pool, "recover-op").await;
    let worker_id = create_test_worker(&pool, "worker-recover-op", 27000).await;
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(server_id, "test").await.unwrap();
    let first_claim = claim_operation(&pool, worker_id)
        .await
        .unwrap()
        .expect("start operation");
    assert_eq!(first_claim.attempt, 1);
    assert!(claim_operation(&pool, worker_id).await.unwrap().is_none());

    assert_eq!(
        recover_running_operations(&pool, worker_id).await.unwrap(),
        1
    );
    let recovered = claim_operation(&pool, worker_id)
        .await
        .unwrap()
        .expect("recovered start operation");
    assert_eq!(recovered.id, first_claim.id);
    assert_eq!(recovered.attempt, 2);

    cleanup_test_entities(&pool, user_id, &[worker_id]).await;
}

#[tokio::test]
async fn failed_operation_retry_is_delayed_and_preserves_the_fenced_placement() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;

    let (user_id, server_id) = create_test_server(&pool, "retry-op").await;
    let worker_id = create_test_worker(&pool, "worker-retry-op", 27200).await;
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(server_id, "test").await.unwrap();
    let operation = claim_operation(&pool, worker_id)
        .await
        .unwrap()
        .expect("start operation");

    assert!(
        retry_operation(&pool, &operation, "temporary object-store outage")
            .await
            .unwrap()
    );
    assert!(claim_operation(&pool, worker_id).await.unwrap().is_none());
    let (status, delayed, active): (kurbis_hoster::model::OperationStatus, bool, bool) =
        sqlx::query_as(
            "SELECT o.status, o.next_attempt_at > now(), p.active
             FROM operations o
             JOIN placements p ON p.server_id = o.server_id
             WHERE o.id = $1",
        )
        .bind(operation.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, kurbis_hoster::model::OperationStatus::Pending);
    assert!(delayed);
    assert!(!active);

    cleanup_test_entities(&pool, user_id, &[worker_id]).await;
}

#[tokio::test]
async fn delete_cancels_an_unclaimed_start_and_releases_its_reservation() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;

    let (user_id, server_id) = create_test_server(&pool, "delete-pending").await;
    let placement_worker = create_test_worker(&pool, "worker-delete-pending", 27400).await;
    let cleanup_worker = create_test_worker(&pool, "worker-delete-cleanup", 27600).await;
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(server_id, "test").await.unwrap();

    assert_eq!(
        lifecycle.request_delete(server_id).await.unwrap(),
        ObservedState::Deleted
    );
    let (desired, state, active, reserved_memory): (
        kurbis_hoster::model::DesiredState,
        ObservedState,
        bool,
        i64,
    ) = sqlx::query_as(
        "SELECT s.desired_state, s.observed_state, p.active, w.reserved_memory_bytes
         FROM servers s
         JOIN placements p ON p.server_id = s.id
         JOIN workers w ON w.id = p.worker_id
         WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(desired, kurbis_hoster::model::DesiredState::Deleted);
    assert_eq!(state, ObservedState::Deleted);
    assert!(!active);
    assert_eq!(reserved_memory, 0);

    let cancelled_starts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations
         WHERE server_id = $1 AND kind = 'start' AND status = 'cancelled'",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cancelled_starts, 1);
    let delete = claim_operation(&pool, cleanup_worker)
        .await
        .unwrap()
        .expect("delete cleanup operation");
    assert_eq!(delete.kind, OperationKind::Delete);
    assert_eq!(delete.worker_id, Some(cleanup_worker));

    cleanup_test_entities(&pool, user_id, &[placement_worker, cleanup_worker]).await;
}

#[tokio::test]
async fn online_delete_finishes_shutdown_then_exposes_cleanup_to_any_worker() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;

    let (user_id, server_id) = create_test_server(&pool, "delete-online").await;
    let runtime_worker = create_test_worker(&pool, "worker-delete-online", 27800).await;
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(server_id, "test").await.unwrap();
    let start = claim_operation(&pool, runtime_worker)
        .await
        .unwrap()
        .expect("start operation");
    complete_start(&pool, &start, 0).await.unwrap();
    let cleanup_worker = create_test_worker(&pool, "worker-delete-other", 28000).await;

    assert_eq!(
        lifecycle.request_delete(server_id).await.unwrap(),
        ObservedState::Stopping
    );
    let stop = claim_operation(&pool, runtime_worker)
        .await
        .unwrap()
        .expect("stop operation");
    assert_eq!(stop.kind, OperationKind::Stop);
    publish_final_snapshot(&pool, &stop, 1, 42).await;
    complete_stop(&pool, &stop, 1, 42).await.unwrap();

    let (desired, state, active): (kurbis_hoster::model::DesiredState, ObservedState, bool) =
        sqlx::query_as(
            "SELECT s.desired_state, s.observed_state, p.active
             FROM servers s JOIN placements p ON p.server_id = s.id WHERE s.id = $1",
        )
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(desired, kurbis_hoster::model::DesiredState::Deleted);
    assert_eq!(state, ObservedState::Deleted);
    assert!(!active);

    let delete = claim_operation(&pool, cleanup_worker)
        .await
        .unwrap()
        .expect("delete cleanup operation");
    assert_eq!(delete.kind, OperationKind::Delete);
    assert_eq!(delete.worker_id, Some(cleanup_worker));

    cleanup_test_entities(&pool, user_id, &[runtime_worker, cleanup_worker]).await;
}

#[tokio::test]
async fn runtime_loss_preserves_delete_intent_and_schedules_cleanup() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;

    let (user_id, server_id) = create_test_server(&pool, "delete-loss").await;
    let runtime_worker = create_test_worker(&pool, "worker-delete-loss", 28200).await;
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(server_id, "test").await.unwrap();
    let start = claim_operation(&pool, runtime_worker)
        .await
        .unwrap()
        .expect("start operation");
    complete_start(&pool, &start, 0).await.unwrap();
    let cleanup_worker = create_test_worker(&pool, "worker-delete-loss-cleanup", 28400).await;
    lifecycle.request_delete(server_id).await.unwrap();

    mark_runtime_lost(&pool, runtime_worker, server_id, "worker disappeared")
        .await
        .unwrap();
    let (desired, state): (kurbis_hoster::model::DesiredState, ObservedState) =
        sqlx::query_as("SELECT desired_state, observed_state FROM servers WHERE id = $1")
            .bind(server_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(desired, kurbis_hoster::model::DesiredState::Deleted);
    assert_eq!(state, ObservedState::Deleted);
    let delete = claim_operation(&pool, cleanup_worker)
        .await
        .unwrap()
        .expect("delete cleanup after runtime loss");
    assert_eq!(delete.kind, OperationKind::Delete);

    cleanup_test_entities(&pool, user_id, &[runtime_worker, cleanup_worker]).await;
}

#[tokio::test]
async fn stopped_server_ports_are_reused_without_losing_its_cached_placement() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let worker = create_test_worker(&pool, "worker-port-reuse", 29000).await;
    sqlx::query("UPDATE workers SET port_max = port_min + 1 WHERE id = $1")
        .bind(worker)
        .execute(&pool)
        .await
        .unwrap();
    let (owner_a, server_a) = create_test_server(&pool, "port-a").await;
    let (owner_b, server_b) = create_test_server(&pool, "port-b").await;
    let lifecycle = Lifecycle::new(pool.clone());
    assert_eq!(
        lifecycle.request_start(server_a, "test").await.unwrap(),
        ObservedState::Restoring
    );
    let start = claim_operation(&pool, worker).await.unwrap().unwrap();
    complete_start(&pool, &start, 0).await.unwrap();
    // Port exhaustion is capacity exhaustion, even when RAM/disk/CPU are free.
    assert_eq!(
        lifecycle.request_start(server_b, "test").await.unwrap(),
        ObservedState::Queued
    );
    lifecycle.request_stop(server_a, "test").await.unwrap();
    let stop = claim_operation(&pool, worker).await.unwrap().unwrap();
    publish_final_snapshot(&pool, &stop, 1, 42).await;
    complete_stop(&pool, &stop, 1, 42).await.unwrap();
    assert_eq!(lifecycle.drain_queue_once().await.unwrap(), 1);
    let assignments: Vec<(Uuid, bool, i32, i32)> = sqlx::query_as(
        "SELECT server_id, active, java_port, bedrock_port FROM placements WHERE worker_id = $1",
    )
    .bind(worker)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(assignments.len(), 2);
    assert!(
        assignments
            .iter()
            .all(|(_, active, java, bedrock)| !active && *java == 29000 && *bedrock == 29001)
    );
    let next = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert_eq!(next.server_id, server_b);
    complete_start(&pool, &next, 0).await.unwrap();
    assert_eq!(
        lifecycle.request_start(server_a, "rejoin").await.unwrap(),
        ObservedState::Queued
    );
    cleanup_test_entities(&pool, owner_a, &[]).await;
    cleanup_test_entities(&pool, owner_b, &[worker]).await;
}

#[tokio::test]
async fn new_join_cannot_overtake_queued_wakes_when_capacity_returns() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let (owner_a, server_a) = create_test_server(&pool, "queue-first").await;
    let (owner_b, server_b) = create_test_server(&pool, "queue-next").await;
    let (owner_c, server_c) = create_test_server(&pool, "queue-new").await;
    let lifecycle = Lifecycle::new(pool.clone());
    assert_eq!(
        lifecycle.request_start(server_a, "join").await.unwrap(),
        ObservedState::Queued
    );
    assert_eq!(
        lifecycle.request_start(server_b, "join").await.unwrap(),
        ObservedState::Queued
    );
    let original: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT requested_at FROM start_requests WHERE server_id = $1")
            .bind(server_a)
            .fetch_one(&pool)
            .await
            .unwrap();
    let worker = create_test_worker(&pool, "worker-queue-one", 29200).await;
    sqlx::query("UPDATE workers SET total_memory_bytes = $2 WHERE id = $1")
        .bind(worker)
        .bind(kurbis_hoster::model::SERVER_MEMORY_BYTES)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        lifecycle.request_start(server_c, "new join").await.unwrap(),
        ObservedState::Queued
    );
    let (left, right) = tokio::join!(lifecycle.drain_queue_once(), lifecycle.drain_queue_once());
    left.unwrap();
    right.unwrap();
    let start = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert_eq!(start.server_id, server_a);
    complete_start(&pool, &start, 0).await.unwrap();
    assert_eq!(
        lifecycle
            .request_start(server_b, "repeat join")
            .await
            .unwrap(),
        ObservedState::Queued
    );
    let next_time: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT requested_at FROM start_requests WHERE server_id = $1")
            .bind(server_b)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(next_time >= original);
    lifecycle.request_stop(server_a, "idle").await.unwrap();
    let stop = claim_operation(&pool, worker).await.unwrap().unwrap();
    publish_final_snapshot(&pool, &stop, 1, 0).await;
    complete_stop(&pool, &stop, 1, 0).await.unwrap();
    assert_eq!(lifecycle.drain_queue_once().await.unwrap(), 1);
    let next = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert_eq!(next.server_id, server_b);
    assert_eq!(
        lifecycle.request_stop(server_c, "cancel").await.unwrap(),
        ObservedState::Sleeping
    );
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM start_requests")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(queued, 0);
    cleanup_test_entities(&pool, owner_a, &[]).await;
    cleanup_test_entities(&pool, owner_b, &[]).await;
    cleanup_test_entities(&pool, owner_c, &[worker]).await;
}

#[tokio::test]
async fn queue_drain_snapshot_cannot_restart_a_cancelled_or_deleted_server() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let lifecycle = Lifecycle::new(pool.clone());
    for deleting in [false, true] {
        let (owner, server) = create_test_server(&pool, "queue-cancel").await;
        assert_eq!(
            lifecycle.request_start(server, "join").await.unwrap(),
            ObservedState::Queued
        );
        // Hold admission until the drainer has read its queue snapshot. Wait
        // for the actual database lock waiter rather than relying on a sleep.
        let mut gate = pool.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(724513, 1)")
            .execute(&mut *gate)
            .await
            .unwrap();
        let draining = lifecycle.clone();
        let task = tokio::spawn(async move { draining.drain_queue_once().await });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let waiting: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype = 'advisory'
                     AND classid = 724513 AND objid = 1 AND NOT granted
                     AND database = (SELECT oid FROM pg_database WHERE datname = current_database()))")
                    .fetch_one(&pool).await.unwrap();
                if waiting { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.expect("drainer must reach admission lock");
        if deleting {
            lifecycle.request_delete(server).await.unwrap();
            // Simulate completed durable deletion before admission resumes.
            sqlx::query("DELETE FROM servers WHERE id = $1")
                .bind(server)
                .execute(&pool)
                .await
                .unwrap();
        } else {
            lifecycle.request_stop(server, "cancel").await.unwrap();
        }
        let worker = create_test_worker(&pool, "worker-cancel", 29400).await;
        gate.commit().await.unwrap();
        assert_eq!(task.await.unwrap().unwrap(), 0);
        if !deleting {
            let (state, generation): (ObservedState, i64) =
                sqlx::query_as("SELECT observed_state, generation FROM servers WHERE id = $1")
                    .bind(server)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!((state, generation), (ObservedState::Sleeping, 0));
        }
        let reserved: i64 =
            sqlx::query_scalar("SELECT reserved_memory_bytes FROM workers WHERE id = $1")
                .bind(worker)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(reserved, 0);
        cleanup_test_entities(&pool, owner, &[worker]).await;
    }
}

#[path = "lifecycle/operation_completion.rs"]
mod operation_completion;
