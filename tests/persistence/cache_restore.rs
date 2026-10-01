use super::{
    DB_TEST_LOCK, SnapshotReason, cleanup, create_server, local_snapshot_manager,
    publish_test_snapshot, test_pool,
};
use kurbis_hoster::{
    config::StorageMode,
    control_plane::lifecycle::{healthy_worker_template, heartbeat_worker},
    worker::{persistence::prepare_server_data, storage::StorageManager},
};
use sqlx::PgPool;
use uuid::Uuid;

async fn worker(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    heartbeat_worker(
        pool,
        &healthy_worker_template(
            id,
            format!("snapshot-worker-{id}"),
            "127.0.0.1".into(),
            2147483648,
            5368709120,
            1000,
            32000,
            32100,
        ),
    )
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn cache_hint_cannot_hide_dirty_or_missing_disk_after_a_completed_snapshot() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner, server) = create_server(&pool).await;
    let first_worker = worker(&pool).await;
    let second_worker = worker(&pool).await;
    let objects = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let first_disk = tempfile::tempdir().unwrap();
    let second_disk = tempfile::tempdir().unwrap();
    let manager = local_snapshot_manager(&pool, objects.path()).await;
    tokio::fs::create_dir_all(source.path().join("world/players"))
        .await
        .unwrap();
    tokio::fs::write(source.path().join("world/level.dat"), b"saved-world")
        .await
        .unwrap();
    tokio::fs::write(
        source.path().join("world/players/player.dat"),
        b"saved-inventory",
    )
    .await
    .unwrap();
    let complete = manager
        .create(server, source.path(), SnapshotReason::Periodic)
        .await
        .unwrap();
    publish_test_snapshot(&pool, server, complete.generation, complete.size_bytes).await;
    // A newer partially uploaded generation is not eligible for recovery.
    sqlx::query("INSERT INTO snapshots(id,server_id,generation,manifest_key,size_bytes,checksum,reason,complete) VALUES($1,$2,$3,'partial',0,'partial','periodic',false)")
        .bind(Uuid::new_v4()).bind(server).bind(complete.generation + 1).execute(&pool).await.unwrap();
    let local = StorageManager::new(first_disk.path().into(), StorageMode::Directory);
    let directory = prepare_server_data(
        &pool,
        first_worker,
        server,
        Some(complete.generation),
        &local,
        &manager,
    )
    .await
    .unwrap();
    assert!(
        manager
            .local_matches_current(server, &directory)
            .await
            .unwrap()
    );
    // Same size, different contents: metadata/size equality must not suffice.
    tokio::fs::write(directory.join("world/level.dat"), b"dirty-world")
        .await
        .unwrap();
    assert!(
        !manager
            .local_matches_current(server, &directory)
            .await
            .unwrap()
    );
    prepare_server_data(
        &pool,
        first_worker,
        server,
        Some(complete.generation),
        &local,
        &manager,
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read(directory.join("world/level.dat"))
            .await
            .unwrap(),
        b"saved-world"
    );
    tokio::fs::write(directory.join("world/unpublished.dat"), b"must disappear")
        .await
        .unwrap();
    prepare_server_data(
        &pool,
        first_worker,
        server,
        Some(complete.generation),
        &local,
        &manager,
    )
    .await
    .unwrap();
    assert!(!directory.join("world/unpublished.dat").exists());
    #[cfg(unix)]
    {
        // A matching symlink plus an extra file preserves the file count but
        // must not masquerade as the manifest's exact persistent path set.
        tokio::fs::remove_file(directory.join("world/level.dat"))
            .await
            .unwrap();
        std::os::unix::fs::symlink(
            source.path().join("world/level.dat"),
            directory.join("world/level.dat"),
        )
        .unwrap();
        tokio::fs::write(directory.join("world/substitute.dat"), b"saved-world")
            .await
            .unwrap();
        assert!(
            !manager
                .local_matches_current(server, &directory)
                .await
                .unwrap()
        );
        prepare_server_data(
            &pool,
            first_worker,
            server,
            Some(complete.generation),
            &local,
            &manager,
        )
        .await
        .unwrap();
        assert!(
            !std::fs::symlink_metadata(directory.join("world/level.dat"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!directory.join("world/substitute.dat").exists());
    }
    // Disposable SSD loss leaves a durable DB hint but no local data.
    local.remove(server).await.unwrap();
    prepare_server_data(
        &pool,
        first_worker,
        server,
        Some(complete.generation),
        &local,
        &manager,
    )
    .await
    .unwrap();
    assert!(
        manager
            .local_matches_current(server, &directory)
            .await
            .unwrap()
    );
    let other = StorageManager::new(second_disk.path().into(), StorageMode::Directory);
    let transferred = prepare_server_data(
        &pool,
        second_worker,
        server,
        Some(complete.generation),
        &other,
        &manager,
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read(transferred.join("world/players/player.dat"))
            .await
            .unwrap(),
        b"saved-inventory"
    );
    let cached: Vec<i64> =
        sqlx::query_scalar("SELECT generation FROM worker_snapshot_cache WHERE server_id = $1")
            .bind(server)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(cached, vec![complete.generation; 2]);
    cleanup(&pool, owner).await;
    for id in [first_worker, second_worker] {
        sqlx::query("DELETE FROM workers WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn failed_restore_does_not_publish_a_local_cache_generation() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner, server) = create_server(&pool).await;
    let worker = worker(&pool).await;
    let objects = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let disk = tempfile::tempdir().unwrap();
    let storage = StorageManager::new(disk.path().into(), StorageMode::Directory);
    let manager = local_snapshot_manager(&pool, objects.path()).await;
    let directory = storage.ensure(server).await.unwrap();
    tokio::fs::write(
        directory.join("unpublished.dat"),
        b"no complete snapshot yet",
    )
    .await
    .unwrap();
    prepare_server_data(&pool, worker, server, None, &storage, &manager)
        .await
        .unwrap();
    assert!(!directory.join("unpublished.dat").exists());
    tokio::fs::write(source.path().join("level.dat"), b"saved-world")
        .await
        .unwrap();
    let complete = manager
        .create(server, source.path(), SnapshotReason::Shutdown)
        .await
        .unwrap();
    publish_test_snapshot(&pool, server, complete.generation, complete.size_bytes).await;
    let chunk = objects
        .path()
        .join(format!("servers/{server}"))
        .join("chunks")
        .join(blake3::hash(b"saved-world").to_hex().as_str());
    tokio::fs::write(&chunk, b"corrupt").await.unwrap();
    assert!(
        prepare_server_data(
            &pool,
            worker,
            server,
            Some(complete.generation),
            &storage,
            &manager
        )
        .await
        .is_err()
    );
    let cached: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM worker_snapshot_cache WHERE worker_id = $1 AND server_id = $2)")
        .bind(worker).bind(server).fetch_one(&pool).await.unwrap();
    assert!(!cached);
    tokio::fs::write(chunk, b"saved-world").await.unwrap();
    prepare_server_data(
        &pool,
        worker,
        server,
        Some(complete.generation),
        &storage,
        &manager,
    )
    .await
    .unwrap();
    assert!(
        manager
            .local_matches_current(server, &directory)
            .await
            .unwrap()
    );
    cleanup(&pool, owner).await;
    sqlx::query("DELETE FROM workers WHERE id = $1")
        .bind(worker)
        .execute(&pool)
        .await
        .unwrap();
}
