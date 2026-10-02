use super::{
    DB_TEST_LOCK, SnapshotReason, cleanup, create_server, local_snapshot_manager,
    publish_test_snapshot, test_pool,
};
use kurbis_hoster::{
    config::StorageMode,
    worker::{persistence::prepare_server_data, storage::StorageManager},
};
use uuid::Uuid;

#[tokio::test]
async fn local_snapshot_match_requires_exact_paths_and_content_across_disks() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner, server) = create_server(&pool).await;
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
    sqlx::query("INSERT INTO snapshots(id,server_id,generation,manifest_key,size_bytes,checksum,reason,complete) VALUES($1,$2,$3,'partial',0,'partial','periodic',false)")
        .bind(Uuid::new_v4()).bind(server).bind(complete.generation + 1).execute(&pool).await.unwrap();

    let first = StorageManager::new(first_disk.path().into(), StorageMode::Directory);
    let directory = prepare_server_data(server, Some(complete.generation), &first, &manager)
        .await
        .unwrap();
    assert!(
        manager
            .local_matches_current(server, &directory)
            .await
            .unwrap()
    );
    tokio::fs::write(directory.join("world/level.dat"), b"dirty-world")
        .await
        .unwrap();
    assert!(
        !manager
            .local_matches_current(server, &directory)
            .await
            .unwrap()
    );
    prepare_server_data(server, Some(complete.generation), &first, &manager)
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
    prepare_server_data(server, Some(complete.generation), &first, &manager)
        .await
        .unwrap();
    assert!(!directory.join("world/unpublished.dat").exists());
    #[cfg(unix)]
    {
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
        prepare_server_data(server, Some(complete.generation), &first, &manager)
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

    first.remove(server).await.unwrap();
    prepare_server_data(server, Some(complete.generation), &first, &manager)
        .await
        .unwrap();
    assert!(
        manager
            .local_matches_current(server, &directory)
            .await
            .unwrap()
    );
    let second = StorageManager::new(second_disk.path().into(), StorageMode::Directory);
    let transferred = prepare_server_data(server, Some(complete.generation), &second, &manager)
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(transferred.join("world/players/player.dat"))
            .await
            .unwrap(),
        b"saved-inventory"
    );
    cleanup(&pool, owner).await;
}

#[tokio::test]
async fn failed_restore_leaves_no_trusted_local_copy_and_can_retry() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner, server) = create_server(&pool).await;
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
    prepare_server_data(server, None, &storage, &manager)
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
        prepare_server_data(server, Some(complete.generation), &storage, &manager)
            .await
            .is_err()
    );
    tokio::fs::write(chunk, b"saved-world").await.unwrap();
    prepare_server_data(server, Some(complete.generation), &storage, &manager)
        .await
        .unwrap();
    assert!(
        manager
            .local_matches_current(server, &directory)
            .await
            .unwrap()
    );
    cleanup(&pool, owner).await;
}
