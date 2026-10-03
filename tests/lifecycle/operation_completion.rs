use super::publish_final_snapshot;
use super::{
    DB_TEST_LOCK, Lifecycle, ObservedState, claim_operation, clean_test_rows,
    cleanup_test_entities, complete_reset, complete_start, complete_stop, create_test_server,
    create_test_worker, recover_running_operations, retry_operation, test_pool,
    update_operation_payload,
};
use kurbis_hoster::contracts::{
    lifecycle::ResetMode,
    worker::{OperationProgress, OperationStage},
};
use kurbis_hoster::control_plane::lifecycle::fail_operation;
use kurbis_hoster::model::{DesiredState, OperationStatus, SERVER_MEMORY_BYTES};
use serde_json::json;
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
    publish_final_snapshot(&pool, &stop, 1, 12).await;
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
    super::mark_runtime_lost(
        &pool,
        worker,
        server_a,
        start.generation,
        "container removal confirmed",
    )
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
    publish_final_snapshot(&pool, &stop, 1, 0).await;
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

#[tokio::test]
async fn stop_requires_a_current_complete_final_snapshot_before_releasing_capacity() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let worker = create_test_worker(&pool, "worker-final-snapshot", 31600).await;
    let (owner, server) = create_test_server(&pool, "final-snapshot").await;
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(server, "test").await.unwrap();
    let start = claim_operation(&pool, worker).await.unwrap().unwrap();
    complete_start(&pool, &start, 0).await.unwrap();
    lifecycle.request_stop(server, "test").await.unwrap();
    let stop = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert!(complete_stop(&pool, &stop, 1, 100).await.is_err());
    publish_final_snapshot(&pool, &stop, 1, 100).await;
    for mutation in [
        "UPDATE snapshots SET complete = false WHERE server_id = $1",
        "UPDATE snapshots SET complete = true, reason = 'periodic' WHERE server_id = $1",
        "UPDATE snapshots SET reason = 'shutdown', created_at = now() - interval '1 day' WHERE server_id = $1",
        "UPDATE snapshots SET created_at = now(), size_bytes = 101 WHERE server_id = $1",
    ] {
        sqlx::query(mutation)
            .bind(server)
            .execute(&pool)
            .await
            .unwrap();
        assert!(complete_stop(&pool, &stop, 1, 100).await.is_err());
        let (active, reserved, status): (bool, i64, OperationStatus) = sqlx::query_as(
            "SELECT p.active, w.reserved_memory_bytes, o.status
             FROM placements p JOIN workers w ON w.id = p.worker_id
             JOIN operations o ON o.server_id = p.server_id WHERE o.id = $1",
        )
        .bind(stop.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (active, reserved, status),
            (true, SERVER_MEMORY_BYTES, OperationStatus::Running)
        );
    }
    sqlx::query("UPDATE snapshots SET size_bytes = 100 WHERE server_id = $1")
        .bind(server)
        .execute(&pool)
        .await
        .unwrap();
    // A newer published generation cannot be replaced by an older worker result.
    publish_final_snapshot(&pool, &stop, 2, 200).await;
    assert!(complete_stop(&pool, &stop, 1, 100).await.is_err());
    complete_stop(&pool, &stop, 2, 200).await.unwrap();
    let (state, current, size, reserved): (ObservedState, i64, i64, i64) = sqlx::query_as(
        "SELECT s.observed_state, s.current_snapshot_generation, s.storage_bytes, w.reserved_memory_bytes
         FROM servers s JOIN placements p ON p.server_id = s.id JOIN workers w ON w.id = p.worker_id WHERE s.id = $1",
    ).bind(server).fetch_one(&pool).await.unwrap();
    assert_eq!(
        (state, current, size, reserved),
        (ObservedState::Sleeping, 2, 200, 0)
    );
    cleanup_test_entities(&pool, owner, &[worker]).await;
}

#[tokio::test]
async fn shutdown_stage_recovery_rejects_old_claim_and_completed_payload_writes() {
    use kurbis_hoster::control_plane::lifecycle::update_operation_payload;
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let worker = create_test_worker(&pool, "worker-stage-fence", 31700).await;
    let (owner, server) = create_test_server(&pool, "stage-fence").await;
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(server, "test").await.unwrap();
    let start = claim_operation(&pool, worker).await.unwrap().unwrap();
    complete_start(&pool, &start, 0).await.unwrap();
    lifecycle.request_stop(server, "test").await.unwrap();
    let old = claim_operation(&pool, worker).await.unwrap().unwrap();
    update_operation_payload(&pool, &old, OperationProgress::stage(OperationStage::Saved))
        .await
        .unwrap();
    recover_running_operations(&pool, worker).await.unwrap();
    let current = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert_eq!(current.payload["stage"], "saved");
    assert!(
        update_operation_payload(
            &pool,
            &old,
            OperationProgress::stage(OperationStage::Stopped),
        )
        .await
        .is_err()
    );
    update_operation_payload(
        &pool,
        &current,
        OperationProgress::stage(OperationStage::Stopped),
    )
    .await
    .unwrap();
    publish_final_snapshot(&pool, &current, 1, 0).await;
    complete_stop(&pool, &current, 1, 0).await.unwrap();
    assert!(
        update_operation_payload(
            &pool,
            &current,
            OperationProgress::stage(OperationStage::Saved),
        )
        .await
        .is_err()
    );
    let stage: String =
        sqlx::query_scalar("SELECT payload->>'stage' FROM operations WHERE id = $1")
            .bind(current.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stage, "stopped");
    cleanup_test_entities(&pool, owner, &[worker]).await;
}

#[tokio::test]
async fn delete_recovery_fences_cleanup_admission_and_durable_deletion() {
    use kurbis_hoster::control_plane::lifecycle::{complete_delete, validate_delete};
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let worker = create_test_worker(&pool, "worker-delete-fence", 31800).await;
    let (owner, server) = create_test_server(&pool, "delete-fence").await;
    Lifecycle::new(pool.clone())
        .request_delete(server)
        .await
        .unwrap();
    let old = claim_operation(&pool, worker).await.unwrap().unwrap();
    validate_delete(&pool, &old).await.unwrap();
    recover_running_operations(&pool, worker).await.unwrap();
    let current = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert!(validate_delete(&pool, &old).await.is_err());
    assert!(complete_delete(&pool, &old).await.is_err());
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM servers WHERE id = $1)")
        .bind(server)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(exists);
    let mut wrong_generation = current.clone();
    wrong_generation.generation += 1;
    assert!(validate_delete(&pool, &wrong_generation).await.is_err());
    validate_delete(&pool, &current).await.unwrap();
    complete_delete(&pool, &current).await.unwrap();
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM servers WHERE id = $1)")
        .bind(server)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!exists);
    cleanup_test_entities(&pool, owner, &[worker]).await;
}

#[tokio::test]
async fn manual_snapshot_completion_requires_current_claim_and_published_manual_generation() {
    use kurbis_hoster::control_plane::lifecycle::{complete_snapshot, enqueue_operation};
    use kurbis_hoster::model::OperationKind;
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let worker = create_test_worker(&pool, "worker-snapshot-fence", 31900).await;
    let (owner, server) = create_test_server(&pool, "snapshot-fence").await;
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(server, "test").await.unwrap();
    let start = claim_operation(&pool, worker).await.unwrap().unwrap();
    complete_start(&pool, &start, 0).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    enqueue_operation(
        &mut tx,
        server,
        Some(worker),
        OperationKind::Snapshot,
        start.generation,
        serde_json::json!({"reason":"manual", "stage":"requested"}),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE servers SET observed_state = 'snapshotting' WHERE id = $1")
        .bind(server)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let old = claim_operation(&pool, worker).await.unwrap().unwrap();
    recover_running_operations(&pool, worker).await.unwrap();
    let current = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert!(complete_snapshot(&pool, &current, 1).await.is_err());
    publish_final_snapshot(&pool, &current, 1, 12).await;
    assert!(complete_snapshot(&pool, &current, 1).await.is_err());
    sqlx::query("UPDATE snapshots SET reason = 'manual' WHERE server_id = $1")
        .bind(server)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE placements SET active = false WHERE server_id = $1")
        .bind(server)
        .execute(&pool)
        .await
        .unwrap();
    assert!(complete_snapshot(&pool, &old, 1).await.is_err());
    complete_snapshot(&pool, &current, 1).await.unwrap();
    complete_snapshot(&pool, &current, 999).await.unwrap();
    let status: OperationStatus = sqlx::query_scalar("SELECT status FROM operations WHERE id = $1")
        .bind(current.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, OperationStatus::Succeeded);
    cleanup_test_entities(&pool, owner, &[worker]).await;
}

#[tokio::test]
async fn duplicate_manual_snapshot_requests_share_one_live_operation() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let worker = create_test_worker(&pool, "worker-snapshot-duplicate", 32000).await;
    let (owner, server) = create_test_server(&pool, "snapshot-duplicate").await;
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(server, "test").await.unwrap();
    let start = claim_operation(&pool, worker).await.unwrap().unwrap();
    complete_start(&pool, &start, 0).await.unwrap();

    assert_eq!(
        lifecycle.request_snapshot(server).await.unwrap(),
        ObservedState::Snapshotting
    );
    let duplicate = lifecycle.request_snapshot(server).await.unwrap_err();
    assert!(
        duplicate
            .to_string()
            .contains("a server snapshot is already in progress")
    );
    assert!(lifecycle.request_stop(server, "conflict").await.is_err());
    assert!(lifecycle.request_restart(server).await.is_err());
    assert!(lifecycle.request_delete(server).await.is_err());
    assert_eq!(
        lifecycle
            .request_start(server, "gateway_wake")
            .await
            .unwrap(),
        ObservedState::Snapshotting
    );
    let current_state: ObservedState =
        sqlx::query_scalar("SELECT observed_state FROM servers WHERE id = $1")
            .bind(server)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(current_state, ObservedState::Snapshotting);
    let live_snapshots: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations
         WHERE server_id = $1 AND kind = 'snapshot' AND status IN ('pending','running')",
    )
    .bind(server)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(live_snapshots, 1);
    cleanup_test_entities(&pool, owner, &[worker]).await;
}

#[tokio::test]
async fn deferred_snapshot_retries_keep_the_runtime_online_without_losing_the_fence() {
    use kurbis_hoster::control_plane::lifecycle::{begin_snapshot_retry, defer_snapshot_operation};

    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let worker = create_test_worker(&pool, "worker-snapshot-deferred", 32100).await;
    let (owner, server) = create_test_server(&pool, "snapshot-deferred").await;
    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle.request_start(server, "test").await.unwrap();
    let start = claim_operation(&pool, worker).await.unwrap().unwrap();
    complete_start(&pool, &start, 0).await.unwrap();

    lifecycle.request_snapshot(server).await.unwrap();
    let first = claim_operation(&pool, worker).await.unwrap().unwrap();
    sqlx::query("UPDATE placements SET active = false WHERE server_id = $1")
        .bind(server)
        .execute(&pool)
        .await
        .unwrap();
    defer_snapshot_operation(&pool, &first, "object store is unavailable")
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_as::<_, (ObservedState, bool, String)>(
            "SELECT s.observed_state, p.active, o.payload->>'stage'
             FROM servers s JOIN placements p ON p.server_id = s.id
             JOIN operations o ON o.server_id = s.id AND o.kind = 'snapshot'
             WHERE s.id = $1",
        )
        .bind(server)
        .fetch_one(&pool)
        .await
        .unwrap(),
        (ObservedState::Online, true, "deferred".into())
    );
    assert!(lifecycle.request_stop(server, "conflict").await.is_err());
    assert!(
        retry_operation(&pool, &first, "object store is unavailable")
            .await
            .unwrap()
    );

    sqlx::query(
        "UPDATE operations SET attempt = 4, next_attempt_at = now()
         WHERE server_id = $1 AND kind = 'snapshot'",
    )
    .bind(server)
    .execute(&pool)
    .await
    .unwrap();
    let fifth_attempt = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert_eq!(fifth_attempt.attempt, 5);
    begin_snapshot_retry(&pool, &fifth_attempt).await.unwrap();
    let retry_state: (ObservedState, bool, String) = sqlx::query_as(
        "SELECT s.observed_state, p.active, o.payload->>'stage'
         FROM servers s JOIN placements p ON p.server_id = s.id
         JOIN operations o ON o.server_id = s.id AND o.kind = 'snapshot'
         WHERE s.id = $1",
    )
    .bind(server)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        retry_state,
        (ObservedState::Snapshotting, false, "requested".into())
    );

    defer_snapshot_operation(&pool, &fifth_attempt, "object store is still unavailable")
        .await
        .unwrap();
    assert!(
        retry_operation(&pool, &fifth_attempt, "object store is still unavailable")
            .await
            .unwrap()
    );
    let status: OperationStatus = sqlx::query_scalar(
        "SELECT status FROM operations WHERE server_id = $1 AND kind = 'snapshot'",
    )
    .bind(server)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(status, OperationStatus::Pending);
    let available: (ObservedState, bool) = sqlx::query_as(
        "SELECT s.observed_state, p.active FROM servers s
         JOIN placements p ON p.server_id = s.id WHERE s.id = $1",
    )
    .bind(server)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(available, (ObservedState::Online, true));

    cleanup_test_entities(&pool, owner, &[worker]).await;
}

#[tokio::test]
async fn restoring_an_older_snapshot_requires_a_new_authoritative_generation() {
    use kurbis_hoster::control_plane::lifecycle::complete_restore;

    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let worker = create_test_worker(&pool, "worker-restore-monotonic", 32150).await;
    let (owner, server) = create_test_server(&pool, "restore-monotonic").await;
    for generation in 1_i64..=2 {
        sqlx::query(
            "INSERT INTO snapshots(
                id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete
             ) VALUES($1,$2,$3,$4,$5,$6,'shutdown',true)",
        )
        .bind(Uuid::new_v4())
        .bind(server)
        .bind(generation)
        .bind(format!("source-{generation}"))
        .bind(generation * 10)
        .bind(format!("checksum-{generation}"))
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "UPDATE servers SET current_snapshot_generation = 2, storage_bytes = 20 WHERE id = $1",
    )
    .bind(server)
    .execute(&pool)
    .await
    .unwrap();

    let lifecycle = Lifecycle::new(pool.clone());
    assert_eq!(
        lifecycle.request_restore(server, 1).await.unwrap(),
        ObservedState::Restoring
    );
    let restore = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert_eq!(restore.kind, kurbis_hoster::model::OperationKind::Restore);

    sqlx::query(
        "INSERT INTO snapshots(
            id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete, created_at
         ) VALUES($1,$2,3,'restored-3',11,'restored-checksum','restore',true,now())",
    )
    .bind(Uuid::new_v4())
    .bind(server)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE servers SET current_snapshot_generation = 3, storage_bytes = 11 WHERE id = $1",
    )
    .bind(server)
    .execute(&pool)
    .await
    .unwrap();

    complete_restore(&pool, &restore, 1, 3, 11).await.unwrap();
    let state: (ObservedState, Option<i64>, i64) = sqlx::query_as(
        "SELECT observed_state, current_snapshot_generation, storage_bytes
         FROM servers WHERE id = $1",
    )
    .bind(server)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, (ObservedState::Sleeping, Some(3), 11));
    let reasons: Vec<(i64, String)> = sqlx::query_as(
        "SELECT generation, reason::text FROM snapshots
         WHERE server_id = $1 AND complete ORDER BY generation",
    )
    .bind(server)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        reasons,
        vec![
            (1, "shutdown".to_owned()),
            (2, "shutdown".to_owned()),
            (3, "restore".to_owned()),
        ]
    );
    cleanup_test_entities(&pool, owner, &[worker]).await;
}

#[tokio::test]
async fn restore_admission_waits_for_snapshot_publication_before_pinning_source() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let (owner, server) = create_test_server(&pool, "restore-pin").await;
    let worker = create_test_worker(&pool, "worker-restore-pin", 33000).await;
    sqlx::query("INSERT INTO snapshots(id,server_id,generation,manifest_key,size_bytes,checksum,reason,complete)
        VALUES($1,$2,1,'retained-source',1,'verified-checksum','shutdown',true)")
        .bind(Uuid::new_v4()).bind(server).execute(&pool).await.unwrap();
    let mut publication = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(724514, hashtext($1))")
        .bind(server.to_string())
        .execute(&mut *publication)
        .await
        .unwrap();
    let lifecycle = Lifecycle::new(pool.clone());
    let restore = tokio::spawn(async move { lifecycle.request_restore(server, 1).await });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_locks
                WHERE locktype='advisory' AND classid=724514
                AND objid=(hashtext($1)::bigint & 4294967295) AND NOT granted)",
            )
            .bind(server.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // A publisher can expire the source before admission. Acceptance must then
    // reject the missing generation instead of enqueuing an unusable restore.
    sqlx::query("DELETE FROM snapshots WHERE server_id=$1")
        .bind(server)
        .execute(&mut *publication)
        .await
        .unwrap();
    publication.commit().await.unwrap();
    assert!(
        restore
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("snapshot not found")
    );
    let operations: i64 = sqlx::query_scalar("SELECT count(*) FROM operations WHERE server_id=$1")
        .bind(server)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(operations, 0);
    cleanup_test_entities(&pool, owner, &[worker]).await;
}

#[tokio::test]
async fn resets_advance_generation_recover_persisted_stages_and_preserve_mode_invariants() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let worker = create_test_worker(&pool, "worker-reset-fencing", 31800).await;

    let (online_owner, online_server) = create_test_server(&pool, "reset-world-online").await;
    let online_subdomain: String =
        sqlx::query_scalar("SELECT subdomain FROM servers WHERE id = $1")
            .bind(online_server)
            .fetch_one(&pool)
            .await
            .unwrap();
    let custom_settings = json!({
        "motd":"Keep my world settings",
        "difficulty":"Hard",
        "gamemode":"Adventure",
        "view_distance":10,
        "simulation_distance":6,
        "hardcore":true,
        "whitelist":true
    });
    sqlx::query("UPDATE servers SET settings = $2 WHERE id = $1")
        .bind(online_server)
        .bind(&custom_settings)
        .execute(&pool)
        .await
        .unwrap();
    let whitelist_player = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO whitelist_entries(server_id, player_id, name) VALUES($1, $2, 'KeepAccess')",
    )
    .bind(online_server)
    .bind(whitelist_player)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete)
         VALUES($1, $2, 1, 'old-world-snapshot', 12, 'old-checksum', 'shutdown', true)",
    )
    .bind(Uuid::new_v4())
    .bind(online_server)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE servers SET current_snapshot_generation = 1, storage_bytes = 12 WHERE id = $1",
    )
    .bind(online_server)
    .execute(&pool)
    .await
    .unwrap();

    let lifecycle = Lifecycle::new(pool.clone());
    lifecycle
        .request_start(online_server, "reset test")
        .await
        .unwrap();
    let start = claim_operation(&pool, worker).await.unwrap().unwrap();
    complete_start(&pool, &start, 12).await.unwrap();
    let initial_generation = start.generation;
    assert_eq!(
        lifecycle
            .request_reset(online_server, ResetMode::World, json!({"unused":true}))
            .await
            .unwrap(),
        ObservedState::Resetting
    );
    let reset = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert_eq!(reset.generation, initial_generation + 1);
    assert_eq!(reset.payload["mode"], "world");
    assert_eq!(reset.payload["was_running"], true);

    let (state, desired, generation, placement_generation, active, default_server): (
        ObservedState,
        DesiredState,
        i64,
        i64,
        bool,
        bool,
    ) = sqlx::query_as(
        "SELECT s.observed_state, s.desired_state, s.generation, p.generation, p.active, s.is_default
         FROM servers s JOIN placements p ON p.server_id = s.id WHERE s.id = $1",
    )
    .bind(online_server)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (
            state,
            desired,
            generation,
            placement_generation,
            active,
            default_server
        ),
        (
            ObservedState::Resetting,
            DesiredState::Running,
            reset.generation,
            reset.generation,
            true,
            true
        )
    );
    assert!(
        lifecycle
            .request_start(online_server, "racing wake")
            .await
            .is_err()
    );
    assert!(
        lifecycle
            .request_stop(online_server, "racing stop")
            .await
            .is_err()
    );
    assert!(
        lifecycle
            .request_reset(online_server, ResetMode::World, json!({}))
            .await
            .is_err()
    );

    update_operation_payload(
        &pool,
        &reset,
        OperationProgress::backed_up(reset.generation - 1),
    )
    .await
    .unwrap();
    recover_running_operations(&pool, worker).await.unwrap();
    let recovered = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert_eq!(recovered.id, reset.id);
    assert_eq!(recovered.attempt, reset.attempt + 1);
    assert_eq!(recovered.payload["stage"], "backed_up");
    assert!(complete_reset(&pool, &reset, 2, 32, true).await.is_err());

    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete)
         VALUES($1, $2, 2, 'post-world-reset', 32, 'new-checksum', 'reset', true)",
    )
    .bind(Uuid::new_v4())
    .bind(online_server)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE servers SET current_snapshot_generation = 2, storage_bytes = 32 WHERE id = $1",
    )
    .bind(online_server)
    .execute(&pool)
    .await
    .unwrap();
    complete_reset(&pool, &recovered, 2, 32, true)
        .await
        .unwrap();
    let (state, generation, snapshot, active, subdomain, settings): (
        ObservedState,
        i64,
        i64,
        bool,
        String,
        serde_json::Value,
    ) = sqlx::query_as(
        "SELECT s.observed_state, s.generation, s.current_snapshot_generation, p.active, s.subdomain, s.settings
         FROM servers s JOIN placements p ON p.server_id = s.id WHERE s.id = $1",
    )
    .bind(online_server)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (state, generation, snapshot, active, subdomain),
        (
            ObservedState::Online,
            reset.generation,
            2,
            true,
            online_subdomain
        )
    );
    assert_eq!(settings, custom_settings);
    let (whitelist_count, snapshot_count): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM whitelist_entries WHERE server_id = $1),
                (SELECT count(*) FROM snapshots WHERE server_id = $1)",
    )
    .bind(online_server)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((whitelist_count, snapshot_count), (1, 2));

    let (sleeping_owner, sleeping_server) = create_test_server(&pool, "reset-full-sleeping").await;
    let sleeping_subdomain: String =
        sqlx::query_scalar("SELECT subdomain FROM servers WHERE id = $1")
            .bind(sleeping_server)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("UPDATE servers SET settings = $2 WHERE id = $1")
        .bind(sleeping_server)
        .bind(&custom_settings)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO whitelist_entries(server_id, player_id, name) VALUES($1, $2, 'RemoveAccess')",
    )
    .bind(sleeping_server)
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete)
         VALUES($1, $2, 1, 'sleeping-old-snapshot', 8, 'old', 'shutdown', true)",
    )
    .bind(Uuid::new_v4())
    .bind(sleeping_server)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE servers SET current_snapshot_generation = 1, storage_bytes = 8 WHERE id = $1",
    )
    .bind(sleeping_server)
    .execute(&pool)
    .await
    .unwrap();
    let default_settings = json!({
        "motd":"A Pumpkin Server",
        "difficulty":"Normal",
        "gamemode":"Survival",
        "view_distance":12,
        "simulation_distance":8,
        "hardcore":false,
        "whitelist":false
    });
    lifecycle
        .request_reset(sleeping_server, ResetMode::Full, default_settings.clone())
        .await
        .unwrap();
    let sleeping_reset = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert_eq!(sleeping_reset.payload["was_running"], false);
    assert_eq!(sleeping_reset.generation, 1);
    let (state, desired, settings, whitelist_count, default_server): (
        ObservedState,
        DesiredState,
        serde_json::Value,
        i64,
        bool,
    ) = sqlx::query_as(
        "SELECT s.observed_state, s.desired_state, s.settings,
                (SELECT count(*) FROM whitelist_entries WHERE server_id = s.id), s.is_default
         FROM servers s WHERE s.id = $1",
    )
    .bind(sleeping_server)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, ObservedState::Resetting);
    assert_eq!(desired, DesiredState::Stopped);
    assert_eq!(settings, default_settings);
    assert_eq!(whitelist_count, 0);
    assert!(default_server);

    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete)
         VALUES($1, $2, 2, 'post-full-reset', 4, 'new', 'reset', true)",
    )
    .bind(Uuid::new_v4())
    .bind(sleeping_server)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE servers SET current_snapshot_generation = 2, storage_bytes = 4 WHERE id = $1",
    )
    .bind(sleeping_server)
    .execute(&pool)
    .await
    .unwrap();
    complete_reset(&pool, &sleeping_reset, 2, 4, false)
        .await
        .unwrap();
    let (state, desired, current, active, subdomain): (
        ObservedState,
        DesiredState,
        i64,
        Option<bool>,
        String,
    ) = sqlx::query_as(
        "SELECT s.observed_state, s.desired_state, s.current_snapshot_generation, p.active, s.subdomain
         FROM servers s LEFT JOIN placements p ON p.server_id = s.id WHERE s.id = $1",
    )
    .bind(sleeping_server)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (state, desired, current, active, subdomain),
        (
            ObservedState::Sleeping,
            DesiredState::Stopped,
            2,
            None,
            sleeping_subdomain
        )
    );
    let retained_backups: i64 =
        sqlx::query_scalar("SELECT count(*) FROM snapshots WHERE server_id = $1")
            .bind(sleeping_server)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(retained_backups, 2);

    cleanup_test_entities(&pool, online_owner, &[worker]).await;
    cleanup_test_entities(&pool, sleeping_owner, &[]).await;
}

#[tokio::test]
async fn online_restore_stops_cleanly_then_enqueues_the_restore_operation() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let worker = create_test_worker(&pool, "worker-online-restore", 33200).await;
    let (owner, server) = create_test_server(&pool, "online-restore").await;
    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete)
         VALUES($1, $2, 1, 'restore-source', 10, 'restore-source-checksum', 'shutdown', true)",
    )
    .bind(Uuid::new_v4())
    .bind(server)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE servers SET current_snapshot_generation = 1 WHERE id = $1")
        .bind(server)
        .execute(&pool)
        .await
        .unwrap();

    let lifecycle = Lifecycle::new(pool.clone());
    assert_eq!(
        lifecycle.request_start(server, "test").await.unwrap(),
        ObservedState::Restoring
    );
    let start = claim_operation(&pool, worker).await.unwrap().unwrap();
    complete_start(&pool, &start, 10).await.unwrap();

    assert_eq!(
        lifecycle.request_restore(server, 1).await.unwrap(),
        ObservedState::Stopping
    );
    let stop = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert_eq!(stop.kind, kurbis_hoster::model::OperationKind::Stop);
    publish_final_snapshot(&pool, &stop, 2, 11).await;
    complete_stop(&pool, &stop, 2, 11).await.unwrap();

    let (state, desired): (ObservedState, DesiredState) =
        sqlx::query_as("SELECT observed_state, desired_state FROM servers WHERE id = $1")
            .bind(server)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (state, desired),
        (ObservedState::Restoring, DesiredState::Stopped)
    );
    let restore = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert_eq!(restore.kind, kurbis_hoster::model::OperationKind::Restore);
    assert_eq!(restore.generation, 2);
    cleanup_test_entities(&pool, owner, &[worker]).await;
}
