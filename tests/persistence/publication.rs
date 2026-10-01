use super::*;
use std::time::Duration;

pub(super) async fn wait_for_publishers(pool: &PgPool, server: Uuid, expected: i64) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory'
                 AND classid = 724514 AND objid = (hashtext($1)::bigint & 4294967295)
                 AND NOT granted",
            )
            .bind(server.to_string())
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting == expected {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn concurrent_publishers_serialize_before_allocating_generations() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner, server) = create_server(&pool).await;
    let objects = tempfile::tempdir().unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let restored = tempfile::tempdir().unwrap();
    tokio::fs::write(first.path().join("level.dat"), b"first world")
        .await
        .unwrap();
    tokio::fs::write(second.path().join("level.dat"), b"second world")
        .await
        .unwrap();
    let manager = local_snapshot_manager(&pool, objects.path()).await;
    let mut guard = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(724514, hashtext($1))")
        .bind(server.to_string())
        .execute(&mut *guard)
        .await
        .unwrap();
    let mut tasks = Vec::new();
    for root in [first.path().to_path_buf(), second.path().to_path_buf()] {
        let manager = manager.clone();
        tasks.push(tokio::spawn(async move {
            manager
                .create(server, &root, SnapshotReason::Periodic)
                .await
                .unwrap()
        }));
    }
    // Wait for both real database lock waiters, rather than guessing timing.
    wait_for_publishers(&pool, server, 2).await;
    let allocated: i64 = sqlx::query_scalar("SELECT count(*) FROM snapshots WHERE server_id = $1")
        .bind(server)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        allocated, 0,
        "blocked publishers must not allocate or upload"
    );
    guard.commit().await.unwrap();
    let a = tasks.remove(0).await.unwrap();
    let b = tasks.remove(0).await.unwrap();
    assert_ne!(a.generation, b.generation);
    assert_eq!(a.generation.max(b.generation), 2);
    let current = if a.generation > b.generation { &a } else { &b };
    publish_test_snapshot(&pool, server, current.generation, current.size_bytes).await;
    assert_eq!(
        manager
            .restore_latest(server, restored.path())
            .await
            .unwrap(),
        Some(2)
    );
    let expected = if a.generation == 2 {
        b"first world".as_slice()
    } else {
        b"second world".as_slice()
    };
    assert_eq!(
        tokio::fs::read(restored.path().join("level.dat"))
            .await
            .unwrap(),
        expected
    );
    cleanup(&pool, owner).await;
}

#[tokio::test]
async fn damaged_reused_chunk_cannot_publish_and_retry_recovers() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner, server) = create_server(&pool).await;
    let objects = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let restored = tempfile::tempdir().unwrap();
    let bytes = b"saved world";
    tokio::fs::write(source.path().join("level.dat"), bytes)
        .await
        .unwrap();
    let manager = local_snapshot_manager(&pool, objects.path()).await;
    let initial = manager
        .create(server, source.path(), SnapshotReason::Periodic)
        .await
        .unwrap();
    publish_test_snapshot(&pool, server, initial.generation, initial.size_bytes).await;
    let chunk = objects
        .path()
        .join(format!("servers/{server}"))
        .join("chunks")
        .join(blake3::hash(bytes).to_hex().as_str());
    tokio::fs::write(&chunk, b"wrong world").await.unwrap();
    let error = manager
        .create(server, source.path(), SnapshotReason::Shutdown)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("integrity validation"));
    let (current, complete, partial): (i64, i64, i64) = sqlx::query_as(
        "SELECT current_snapshot_generation,
         (SELECT count(*) FROM snapshots WHERE server_id = $1 AND complete),
         (SELECT count(*) FROM snapshots WHERE server_id = $1 AND NOT complete)
         FROM servers WHERE id = $1",
    )
    .bind(server)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((current, complete, partial), (initial.generation, 1, 1));
    tokio::fs::write(&chunk, bytes).await.unwrap();
    let retry = manager
        .create(server, source.path(), SnapshotReason::Shutdown)
        .await
        .unwrap();
    publish_test_snapshot(&pool, server, retry.generation, retry.size_bytes).await;
    assert_eq!(retry.generation, 3);
    assert_eq!(
        manager
            .restore_latest(server, restored.path())
            .await
            .unwrap(),
        Some(3)
    );
    assert_eq!(
        tokio::fs::read(restored.path().join("level.dat"))
            .await
            .unwrap(),
        bytes
    );
    cleanup(&pool, owner).await;
}

#[tokio::test]
async fn manifest_upload_failure_preserves_current_and_releases_publication_lock() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner, server) = create_server(&pool).await;
    let objects = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let restored = tempfile::tempdir().unwrap();
    tokio::fs::write(source.path().join("level.dat"), b"initial world")
        .await
        .unwrap();
    let single_connection_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(2))
        .connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let manager = SnapshotManager::new(
        single_connection_pool,
        local_snapshot_store(objects.path()).await,
    );
    let initial = manager
        .create(server, source.path(), SnapshotReason::Periodic)
        .await
        .unwrap();
    publish_test_snapshot(&pool, server, initial.generation, initial.size_bytes).await;
    // Keep completed objects intact, but prevent the next manifest write.
    let object_prefix = objects.path().join(format!("servers/{server}"));
    let manifests = object_prefix.join("snapshots");
    let preserved = object_prefix.join("preserved-snapshots");
    tokio::fs::rename(&manifests, &preserved).await.unwrap();
    tokio::fs::write(&manifests, b"unavailable object-store prefix")
        .await
        .unwrap();
    tokio::fs::write(source.path().join("level.dat"), b"latest world")
        .await
        .unwrap();
    assert!(
        manager
            .create(server, source.path(), SnapshotReason::Shutdown)
            .await
            .is_err()
    );
    let (current, partial): (i64, i64) = sqlx::query_as(
        "SELECT current_snapshot_generation,
         (SELECT count(*) FROM snapshots WHERE server_id = $1 AND NOT complete)
         FROM servers WHERE id = $1",
    )
    .bind(server)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((current, partial), (1, 1));
    tokio::fs::remove_file(&manifests).await.unwrap();
    tokio::fs::rename(preserved, manifests).await.unwrap();
    assert_eq!(
        manager
            .restore_latest(server, restored.path())
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        tokio::fs::read(restored.path().join("level.dat"))
            .await
            .unwrap(),
        b"initial world"
    );
    let retry = tokio::time::timeout(
        Duration::from_secs(10),
        manager.create(server, source.path(), SnapshotReason::Shutdown),
    )
    .await
    .unwrap()
    .unwrap();
    publish_test_snapshot(&pool, server, retry.generation, retry.size_bytes).await;
    assert_eq!(retry.generation, 3);
    assert_eq!(
        manager
            .restore_latest(server, restored.path())
            .await
            .unwrap(),
        Some(3)
    );
    assert_eq!(
        tokio::fs::read(restored.path().join("level.dat"))
            .await
            .unwrap(),
        b"latest world"
    );
    cleanup(&pool, owner).await;
}

#[tokio::test]
async fn cancelled_publisher_does_not_leave_a_lock_in_the_pool() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner, server) = create_server(&pool).await;
    let objects = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    tokio::fs::write(source.path().join("level.dat"), b"saved world")
        .await
        .unwrap();
    let limited_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(2))
        .connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let manager = SnapshotManager::new(limited_pool, local_snapshot_store(objects.path()).await);
    let mut guard = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(724514, hashtext($1))")
        .bind(server.to_string())
        .execute(&mut *guard)
        .await
        .unwrap();
    let task_manager = manager.clone();
    let root = source.path().to_path_buf();
    let task = tokio::spawn(async move {
        task_manager
            .create(server, &root, SnapshotReason::Periodic)
            .await
    });
    wait_for_publishers(&pool, server, 1).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    guard.commit().await.unwrap();
    let snapshot = tokio::time::timeout(
        Duration::from_secs(10),
        manager.create(server, source.path(), SnapshotReason::Periodic),
    )
    .await
    .unwrap()
    .unwrap();
    publish_test_snapshot(&pool, server, snapshot.generation, snapshot.size_bytes).await;
    assert_eq!(snapshot.generation, 1);
    cleanup(&pool, owner).await;
}

#[tokio::test]
async fn restore_source_survives_retention_and_gc_until_operation_finishes() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner, server) = create_server(&pool).await;
    let objects = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let restored = tempfile::tempdir().unwrap();
    let manager = local_snapshot_manager(&pool, objects.path()).await;
    tokio::fs::write(source.path().join("level.dat"), b"selected restore source")
        .await
        .unwrap();
    let selected = manager
        .create(server, source.path(), SnapshotReason::Periodic)
        .await
        .unwrap();
    publish_test_snapshot(&pool, server, selected.generation, selected.size_bytes).await;
    let operation = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO operations(id, server_id, kind, generation, payload)
        VALUES($1,$2,'restore',1,jsonb_build_object('target_generation',$3::bigint))",
    )
    .bind(operation)
    .bind(server)
    .bind(selected.generation)
    .execute(&pool)
    .await
    .unwrap();
    let selected_chunk = objects
        .path()
        .join(format!("servers/{server}"))
        .join("chunks")
        .join(blake3::hash(b"selected restore source").to_hex().as_str());
    for status in ["pending", "running"] {
        sqlx::query("UPDATE operations SET status = $2::operation_status WHERE id = $1")
            .bind(operation)
            .bind(status)
            .execute(&pool)
            .await
            .unwrap();
        for generation in 0..SNAPSHOT_RETENTION_GENERATIONS {
            tokio::fs::write(
                source.path().join("level.dat"),
                format!("{status}-{generation}"),
            )
            .await
            .unwrap();
            let snapshot = manager
                .create(server, source.path(), SnapshotReason::Restore)
                .await
                .unwrap();
            publish_test_snapshot(&pool, server, snapshot.generation, snapshot.size_bytes).await;
        }
        let gc_store = local_snapshot_store(objects.path()).await;
        SnapshotManager::collect_garbage_with(&pool, gc_store)
            .await
            .unwrap();
        assert!(
            selected_chunk.exists(),
            "active restore source chunk must remain"
        );
        assert_eq!(
            manager
                .restore_generation(server, selected.generation, restored.path())
                .await
                .unwrap(),
            selected.generation
        );
        assert_eq!(
            tokio::fs::read(restored.path().join("level.dat"))
                .await
                .unwrap(),
            b"selected restore source"
        );
    }
    sqlx::query("UPDATE operations SET status = 'succeeded' WHERE id = $1")
        .bind(operation)
        .execute(&pool)
        .await
        .unwrap();
    let latest = manager
        .create(server, source.path(), SnapshotReason::Periodic)
        .await
        .unwrap();
    publish_test_snapshot(&pool, server, latest.generation, latest.size_bytes).await;
    let gc_store = local_snapshot_store(objects.path()).await;
    SnapshotManager::collect_garbage_with(&pool, gc_store)
        .await
        .unwrap();
    assert!(
        !selected_chunk.exists(),
        "completed restore must release its retention pin"
    );
    assert!(
        manager
            .restore_generation(server, selected.generation, restored.path())
            .await
            .is_err()
    );
    cleanup(&pool, owner).await;
}
