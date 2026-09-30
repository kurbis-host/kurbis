use super::{
    DB_TEST_LOCK, Lifecycle, ObservedState, claim_operation, clean_test_rows,
    cleanup_test_entities, complete_start, complete_stop, create_test_server, create_test_worker,
    recover_running_operations, retry_operation, test_pool,
};
use melon_hoster::control_plane::lifecycle::fail_operation;
use melon_hoster::model::{OperationStatus, SERVER_MEMORY_BYTES};
use uuid::Uuid;

#[tokio::test]
async fn replayed_stop_preserves_other_servers_capacity_and_snapshot_pointer() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let worker = create_test_worker(&pool, "worker-replayed-stop", 31000).await;
    let (owner_a, server_a) = create_test_server(&pool, "stop-replay-a").await;
    let (owner_b, server_b) = create_test_server(&pool, "stop-replay-b").await;
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(server_a, "test").await.unwrap();
    let start = claim_operation(&pool, worker).await.unwrap().unwrap();
    complete_start(&pool, &start, 11).await.unwrap();
    lifecycle.request_stop(server_a, "test").await.unwrap();
    let stop = claim_operation(&pool, worker).await.unwrap().unwrap();
    complete_stop(&pool, &stop, 1, 12).await.unwrap();
    lifecycle.request_start(server_b, "test").await.unwrap();
    // Both concurrent and delayed delivery of the same stop result are no-ops.
    let (first, second) = tokio::join!(
        complete_stop(&pool, &stop, 2, 999),
        complete_stop(&pool, &stop, 3, 888),
    );
    first.unwrap();
    second.unwrap();
    let reserved: i64 =
        sqlx::query_scalar("SELECT reserved_memory_bytes FROM workers WHERE id = $1")
            .bind(worker)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reserved, SERVER_MEMORY_BYTES);
    let pointer: (ObservedState, i64, i64) = sqlx::query_as(
        "SELECT observed_state, current_snapshot_generation, storage_bytes FROM servers WHERE id = $1")
        .bind(server_a).fetch_one(&pool).await.unwrap();
    assert_eq!(pointer, (ObservedState::Sleeping, 1, 12));
    // A delayed successful start must not make a stopped container routable.
    complete_start(&pool, &start, 999).await.unwrap();
    let (state, active): (ObservedState, bool) = sqlx::query_as(
        "SELECT s.observed_state, p.active FROM servers s JOIN placements p ON p.server_id = s.id WHERE s.id = $1")
        .bind(server_a).fetch_one(&pool).await.unwrap();
    assert_eq!((state, active), (ObservedState::Sleeping, false));
    cleanup_test_entities(&pool, owner_a, &[]).await;
    cleanup_test_entities(&pool, owner_b, &[worker]).await;
}

#[tokio::test]
async fn recovered_claim_rejects_delayed_completion_retry_and_failure() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let worker = create_test_worker(&pool, "worker-stale-claim", 31200).await;
    let (owner, server) = create_test_server(&pool, "stale-claim").await;
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(server, "test").await.unwrap();
    let old = claim_operation(&pool, worker).await.unwrap().unwrap();
    recover_running_operations(&pool, worker).await.unwrap();
    let current = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert_eq!(old.id, current.id);
    assert_eq!(current.attempt, old.attempt + 1);
    assert!(complete_start(&pool, &old, 0).await.is_err());
    assert!(retry_operation(&pool, &old, "late failure").await.is_err());
    assert!(
        fail_operation(&pool, &old, "late terminal failure")
            .await
            .is_err()
    );
    let (status, attempt): (OperationStatus, i32) =
        sqlx::query_as("SELECT status, attempt FROM operations WHERE id = $1")
            .bind(current.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (status, attempt),
        (OperationStatus::Running, current.attempt)
    );
    let mut wrong_worker = current.clone();
    wrong_worker.worker_id = Some(Uuid::new_v4());
    assert!(complete_start(&pool, &wrong_worker, 0).await.is_err());
    complete_start(&pool, &current, 45).await.unwrap();
    complete_start(&pool, &current, 999).await.unwrap();
    assert!(
        fail_operation(&pool, &current, "late failure after success")
            .await
            .is_err()
    );
    let state: (ObservedState, i64) =
        sqlx::query_as("SELECT observed_state, storage_bytes FROM servers WHERE id = $1")
            .bind(server)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, (ObservedState::Online, 45));
    cleanup_test_entities(&pool, owner, &[worker]).await;
}

#[tokio::test]
async fn failed_start_is_fenced_until_removal_then_replays_preserve_other_reservations() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let worker = create_test_worker(&pool, "worker-failed-replay", 31400).await;
    let (owner_a, server_a) = create_test_server(&pool, "fail-replay-a").await;
    let (owner_b, server_b) = create_test_server(&pool, "fail-replay-b").await;
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(server_a, "test").await.unwrap();
    let start = claim_operation(&pool, worker).await.unwrap().unwrap();
    fail_operation(&pool, &start, "restore failed before container creation")
        .await
        .unwrap();
    let (reserved_before, active_before): (i64, bool) = sqlx::query_as(
        "SELECT w.reserved_memory_bytes, p.active FROM workers w JOIN placements p ON p.worker_id = w.id WHERE p.server_id = $1")
        .bind(server_a).fetch_one(&pool).await.unwrap();
    assert_eq!(
        (reserved_before, active_before),
        (SERVER_MEMORY_BYTES, true)
    );
    assert!(
        lifecycle
            .request_start(server_a, "premature retry")
            .await
            .is_err()
    );
    super::mark_runtime_lost(&pool, worker, server_a, "container removal confirmed")
        .await
        .unwrap();
    lifecycle.request_start(server_b, "test").await.unwrap();
    fail_operation(&pool, &start, "duplicate failure")
        .await
        .unwrap();
    let reserved: i64 =
        sqlx::query_scalar("SELECT reserved_memory_bytes FROM workers WHERE id = $1")
            .bind(worker)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reserved, SERVER_MEMORY_BYTES);
    assert!(complete_start(&pool, &start, 0).await.is_err());
    cleanup_test_entities(&pool, owner_a, &[]).await;
    cleanup_test_entities(&pool, owner_b, &[worker]).await;
}

#[tokio::test]
async fn suspension_during_claimed_start_enqueues_one_clean_shutdown() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let worker = create_test_worker(&pool, "worker-suspend-start", 31600).await;
    let (owner, server) = create_test_server(&pool, "suspend-start").await;
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(server, "test").await.unwrap();
    let start = claim_operation(&pool, worker).await.unwrap().unwrap();
    sqlx::query("UPDATE users SET account_status = 'suspended' WHERE id = $1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    complete_start(&pool, &start, 0).await.unwrap();
    complete_start(&pool, &start, 0).await.unwrap();
    let state: ObservedState =
        sqlx::query_scalar("SELECT observed_state FROM servers WHERE id = $1")
            .bind(server)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, ObservedState::Stopping);
    let stops: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations WHERE server_id = $1 AND kind = 'stop'",
    )
    .bind(server)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stops, 1);
    let stop = claim_operation(&pool, worker).await.unwrap().unwrap();
    complete_stop(&pool, &stop, 1, 0).await.unwrap();
    let state: ObservedState =
        sqlx::query_scalar("SELECT observed_state FROM servers WHERE id = $1")
            .bind(server)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, ObservedState::Sleeping);
    cleanup_test_entities(&pool, owner, &[worker]).await;
}
