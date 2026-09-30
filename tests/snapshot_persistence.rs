mod common;

use common::{create_user, test_pool};
use kurbis_hoster::{
    config::ObjectStoreConfig,
    model::{SNAPSHOT_RETENTION_GENERATIONS, SnapshotReason},
    worker::snapshot::SnapshotManager,
};
use sqlx::PgPool;
use tokio::sync::Mutex;
use uuid::Uuid;

static DB_TEST_LOCK: Mutex<()> = Mutex::const_new(());

async fn create_server(pool: &PgPool) -> (Uuid, Uuid) {
    let server_id = Uuid::new_v4();
    let user_id = create_user(
        pool,
        "google",
        &format!("snapshot-test-{server_id}"),
        "Snapshot Test",
        "active",
        None,
    )
    .await;
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
        .bind(server_id)
        .bind(user_id)
        .bind(format!("snap-{}", &server_id.simple().to_string()[..12]))
        .execute(pool)
        .await
        .unwrap();
    (user_id, server_id)
}

async fn cleanup(pool: &PgPool, user_id: Uuid) {
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn published_snapshot_restores_and_keeps_a_bounded_recovery_window() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (user_id, server_id) = create_server(&pool).await;
    let object_root = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let restore = tempfile::tempdir().unwrap();
    let manager = SnapshotManager::new(
        pool.clone(),
        &ObjectStoreConfig::Local {
            root: object_root.path().to_path_buf(),
        },
    )
    .await
    .unwrap();

    tokio::fs::create_dir_all(source.path().join("world"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(source.path().join("run"))
        .await
        .unwrap();
    tokio::fs::write(
        source.path().join("run/private-runtime-data"),
        b"not world data",
    )
    .await
    .unwrap();
    tokio::fs::write(source.path().join("pumpkin.toml"), b"managed configuration")
        .await
        .unwrap();
    let total = SNAPSHOT_RETENTION_GENERATIONS + 2;
    for generation in 1..=total {
        tokio::fs::write(
            source.path().join("world/level.dat"),
            format!("generation-{generation}"),
        )
        .await
        .unwrap();
        let snapshot = manager
            .create(server_id, source.path(), SnapshotReason::Periodic)
            .await
            .unwrap();
        assert_eq!(snapshot.generation, generation);
        assert!(snapshot.complete);
    }

    let (current, retained, oldest): (Option<i64>, i64, i64) = sqlx::query_as(
        "SELECT s.current_snapshot_generation,
                (SELECT count(*) FROM snapshots sn WHERE sn.server_id = s.id AND sn.complete),
                (SELECT min(generation) FROM snapshots sn WHERE sn.server_id = s.id AND sn.complete)
         FROM servers s WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(current, Some(total));
    assert_eq!(retained, SNAPSHOT_RETENTION_GENERATIONS);
    assert_eq!(oldest, total - SNAPSHOT_RETENTION_GENERATIONS + 1);

    let restored = manager
        .restore_latest(server_id, restore.path())
        .await
        .unwrap();
    assert_eq!(restored, Some(total));
    assert!(!restore.path().join("run").exists());
    assert!(!restore.path().join("pumpkin.toml").exists());
    assert_eq!(
        tokio::fs::read_to_string(restore.path().join("world/level.dat"))
            .await
            .unwrap(),
        format!("generation-{total}")
    );

    cleanup(&pool, user_id).await;
}

#[tokio::test]
async fn restore_rejects_a_corrupted_published_manifest() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (user_id, server_id) = create_server(&pool).await;
    let object_root = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let restore = tempfile::tempdir().unwrap();
    let manager = SnapshotManager::new(
        pool.clone(),
        &ObjectStoreConfig::Local {
            root: object_root.path().to_path_buf(),
        },
    )
    .await
    .unwrap();
    tokio::fs::write(source.path().join("level.dat"), b"world")
        .await
        .unwrap();
    let snapshot = manager
        .create(server_id, source.path(), SnapshotReason::Periodic)
        .await
        .unwrap();
    tokio::fs::write(object_root.path().join(&snapshot.manifest_key), b"corrupt")
        .await
        .unwrap();

    let error = manager
        .restore_latest(server_id, restore.path())
        .await
        .expect_err("corrupt manifest must not restore");
    assert!(error.to_string().contains("manifest checksum mismatch"));

    cleanup(&pool, user_id).await;
}

#[tokio::test]
async fn incomplete_newer_generation_is_never_selected_for_restore() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (user_id, server_id) = create_server(&pool).await;
    let object_root = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let restore = tempfile::tempdir().unwrap();
    let manager = SnapshotManager::new(
        pool.clone(),
        &ObjectStoreConfig::Local {
            root: object_root.path().to_path_buf(),
        },
    )
    .await
    .unwrap();
    tokio::fs::write(source.path().join("level.dat"), b"published")
        .await
        .unwrap();
    let published = manager
        .create(server_id, source.path(), SnapshotReason::Periodic)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete)
         VALUES($1, $2, $3, 'partial', 0, 'partial', 'periodic', false)",
    )
    .bind(Uuid::new_v4())
    .bind(server_id)
    .bind(published.generation + 1)
    .execute(&pool)
    .await
    .unwrap();

    assert_eq!(
        manager
            .restore_latest(server_id, restore.path())
            .await
            .unwrap(),
        Some(published.generation)
    );
    assert_eq!(
        tokio::fs::read_to_string(restore.path().join("level.dat"))
            .await
            .unwrap(),
        "published"
    );

    cleanup(&pool, user_id).await;
}

#[path = "persistence/cache_restore.rs"]
mod cache_restore;

#[path = "persistence/publication.rs"]
mod publication;

#[path = "persistence/garbage_collection.rs"]
mod garbage_collection;
