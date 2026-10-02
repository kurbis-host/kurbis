use super::*;
use std::time::Duration;

#[tokio::test]
async fn garbage_collection_preserves_server_scoped_chunks_and_retained_recovery_generations() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_a, a) = create_server(&pool).await;
    let (owner_b, b) = create_server(&pool).await;
    let objects = tempfile::tempdir().unwrap();
    let source_a = tempfile::tempdir().unwrap();
    let source_b = tempfile::tempdir().unwrap();
    let restore = tempfile::tempdir().unwrap();
    let manager = local_snapshot_manager(&pool, objects.path()).await;
    tokio::fs::write(source_a.path().join("shared.dat"), b"shared data")
        .await
        .unwrap();
    tokio::fs::write(source_b.path().join("shared.dat"), b"shared data")
        .await
        .unwrap();
    let snapshot_b = manager
        .create(b, source_b.path(), SnapshotReason::Periodic)
        .await
        .unwrap();
    publish_test_snapshot(&pool, b, snapshot_b.generation, snapshot_b.size_bytes).await;
    for generation in 1..=SNAPSHOT_RETENTION_GENERATIONS + 2 {
        tokio::fs::write(
            source_a.path().join("level.dat"),
            format!("world-{generation}"),
        )
        .await
        .unwrap();
        let snapshot_a = manager
            .create(a, source_a.path(), SnapshotReason::Periodic)
            .await
            .unwrap();
        publish_test_snapshot(&pool, a, snapshot_a.generation, snapshot_a.size_bytes).await;
    }
    let orphan = objects.path().join(format!(
        "servers/{a}/chunks/{}",
        blake3::hash(b"partial upload").to_hex()
    ));
    let orphan_manifest = objects.path().join(format!(
        "servers/{a}/snapshots/999-{}.json",
        blake3::hash(b"unpublished manifest").to_hex()
    ));
    let abandoned_upload = objects
        .path()
        .join(format!("control-plane-object-staging/{a}/orphan"));
    tokio::fs::write(&orphan, b"partial upload").await.unwrap();
    tokio::fs::write(&orphan_manifest, b"unpublished manifest")
        .await
        .unwrap();
    tokio::fs::create_dir_all(abandoned_upload.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&abandoned_upload, b"unpublished object body")
        .await
        .unwrap();
    let gc_store = local_snapshot_store(objects.path()).await;
    let report = SnapshotStore::collect_garbage_with(&pool, gc_store)
        .await
        .unwrap();
    assert_eq!(report.chunks_deleted, 3); // Two expired worlds plus the partial upload.
    assert_eq!(report.manifests_deleted, 1);
    assert_eq!(report.staging_deleted, 1);
    assert!(!orphan.exists());
    assert!(!abandoned_upload.exists());
    let chunks: Vec<_> = std::fs::read_dir(objects.path().join(format!("servers/{a}/chunks")))
        .unwrap()
        .collect();
    assert_eq!(chunks.len(), SNAPSHOT_RETENTION_GENERATIONS as usize + 1);
    manager.restore_latest(a, restore.path()).await.unwrap();
    assert_eq!(
        tokio::fs::read(restore.path().join("level.dat"))
            .await
            .unwrap(),
        b"world-7"
    );
    cleanup(&pool, owner_a).await;
    let gc_store = local_snapshot_store(objects.path()).await;
    let report = SnapshotStore::collect_garbage_with(&pool, gc_store)
        .await
        .unwrap();
    assert_eq!(
        report.chunks_deleted,
        (SNAPSHOT_RETENTION_GENERATIONS + 1) as u64
    );
    manager.restore_latest(b, restore.path()).await.unwrap();
    assert_eq!(
        tokio::fs::read(restore.path().join("shared.dat"))
            .await
            .unwrap(),
        b"shared data"
    );
    cleanup(&pool, owner_b).await;
    let gc_store = local_snapshot_store(objects.path()).await;
    assert_eq!(
        SnapshotStore::collect_garbage_with(&pool, gc_store)
            .await
            .unwrap()
            .chunks_deleted,
        1
    );
}

#[tokio::test]
async fn corrupt_retained_manifest_aborts_before_any_garbage_is_deleted() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner, server) = create_server(&pool).await;
    let objects = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let manager = local_snapshot_manager(&pool, objects.path()).await;
    tokio::fs::write(source.path().join("level.dat"), b"saved world")
        .await
        .unwrap();
    let snapshot = manager
        .create(server, source.path(), SnapshotReason::Periodic)
        .await
        .unwrap();
    publish_test_snapshot(&pool, server, snapshot.generation, snapshot.size_bytes).await;
    let manifest_path = objects
        .path()
        .join(format!("servers/{server}/{}", snapshot.manifest_key));
    let valid_manifest = tokio::fs::read(&manifest_path).await.unwrap();
    let orphan = objects.path().join(format!(
        "servers/{server}/chunks/{}",
        blake3::hash(b"unreferenced").to_hex()
    ));
    tokio::fs::write(&orphan, b"unreferenced").await.unwrap();
    tokio::fs::write(&manifest_path, b"corrupted")
        .await
        .unwrap();
    let gc_store = local_snapshot_store(objects.path()).await;
    assert!(
        SnapshotStore::collect_garbage_with(&pool, gc_store)
            .await
            .is_err()
    );
    assert!(
        orphan.exists(),
        "marking failure must prevent every deletion"
    );
    tokio::fs::write(manifest_path, valid_manifest)
        .await
        .unwrap();
    let gc_store = local_snapshot_store(objects.path()).await;
    assert_eq!(
        SnapshotStore::collect_garbage_with(&pool, gc_store)
            .await
            .unwrap()
            .chunks_deleted,
        1
    );
    cleanup(&pool, owner).await;
}

#[tokio::test]
async fn collector_waits_for_a_publisher_before_deciding_chunk_reachability() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner, server) = create_server(&pool).await;
    let objects = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let bytes = b"world being published";
    tokio::fs::write(source.path().join("level.dat"), bytes)
        .await
        .unwrap();
    tokio::fs::create_dir_all(objects.path().join(format!("servers/{server}/chunks")))
        .await
        .unwrap();
    let chunk = objects
        .path()
        .join(format!("servers/{server}"))
        .join("chunks")
        .join(blake3::hash(bytes).to_hex().as_str());
    tokio::fs::write(&chunk, bytes).await.unwrap();
    let orphan = objects.path().join(format!(
        "servers/{server}/chunks/{}",
        blake3::hash(b"abandoned chunk").to_hex()
    ));
    tokio::fs::write(&orphan, b"abandoned chunk").await.unwrap();
    let manager = local_snapshot_manager(&pool, objects.path()).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(724514, hashtext($1))")
        .bind(server.to_string())
        .execute(&mut *blocker)
        .await
        .unwrap();
    let publisher_manager = manager.clone();
    let root = source.path().to_path_buf();
    let publisher = tokio::spawn(async move {
        publisher_manager
            .create(server, &root, SnapshotReason::Periodic)
            .await
            .unwrap()
    });
    super::publication::wait_for_publishers(&pool, server, 1).await;
    let gc_store = local_snapshot_store(objects.path()).await;
    let gc_pool = pool.clone();
    let collector = tokio::spawn(async move {
        SnapshotStore::collect_garbage_with(&gc_pool, gc_store)
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiters: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND classid = 724515
                 AND objid = 0 AND mode = 'ExclusiveLock' AND NOT granted",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if waiters == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(chunk.exists());
    assert!(!collector.is_finished());
    blocker.commit().await.unwrap();
    let snapshot = publisher.await.unwrap();
    publish_test_snapshot(&pool, server, snapshot.generation, snapshot.size_bytes).await;
    assert_eq!(collector.await.unwrap().chunks_deleted, 1);
    assert!(chunk.exists());
    assert!(!orphan.exists());
    let restore = tempfile::tempdir().unwrap();
    assert_eq!(
        manager
            .restore_latest(server, restore.path())
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        tokio::fs::read(restore.path().join("level.dat"))
            .await
            .unwrap(),
        bytes
    );
    cleanup(&pool, owner).await;
}
