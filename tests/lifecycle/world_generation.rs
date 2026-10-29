use super::{
    DB_TEST_LOCK, Lifecycle, ObservedState, claim_operation, clean_test_rows,
    cleanup_test_entities, complete_reset, create_test_server, create_test_worker, test_pool,
    update_operation_payload,
};
use kurbis_hoster::contracts::{
    lifecycle::ResetMode,
    worker::{OperationProgress, OperationStage},
    world::{NewWorld, WorldGeneration, WorldType},
};
use kurbis_hoster::control_plane::{lifecycle::complete_restore, snapshots::SnapshotStore};
use kurbis_hoster::model::SnapshotReason;
use serde_json::json;
use sqlx::{PgPool, types::Json};
use std::sync::Arc;
use uuid::Uuid;

async fn server_world(pool: &PgPool, server_id: Uuid) -> WorldGeneration {
    let world: Json<WorldGeneration> =
        sqlx::query_scalar("SELECT world FROM servers WHERE id = $1")
            .bind(server_id)
            .fetch_one(pool)
            .await
            .unwrap();
    world.0
}

async fn snapshot_world(pool: &PgPool, server_id: Uuid, generation: i64) -> WorldGeneration {
    let world: Json<WorldGeneration> =
        sqlx::query_scalar("SELECT world FROM snapshots WHERE server_id = $1 AND generation = $2")
            .bind(server_id)
            .bind(generation)
            .fetch_one(pool)
            .await
            .unwrap();
    world.0
}

async fn snapshot(
    store: &SnapshotStore,
    pool: &PgPool,
    server_id: Uuid,
    reason: SnapshotReason,
) -> (i64, i64) {
    let files = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(files.path().join("world"))
        .await
        .unwrap();
    tokio::fs::write(
        files.path().join("world/level.dat"),
        Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    let snapshot = store.create(server_id, files.path(), reason).await.unwrap();
    sqlx::query(
        "UPDATE servers SET current_snapshot_generation = $2, storage_bytes = $3 WHERE id = $1",
    )
    .bind(server_id)
    .bind(snapshot.generation)
    .bind(snapshot.size_bytes)
    .execute(pool)
    .await
    .unwrap();
    (snapshot.generation, snapshot.size_bytes)
}

/// The server's world changes with its files and nowhere else: a reset's
/// pre-reset backup still names the old world, the new one takes over once
/// the old files are gone, a settings reset leaves it alone, and restoring a
/// backup brings back the world it holds.
#[tokio::test]
async fn the_world_follows_resets_and_restores() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    clean_test_rows(&pool).await;
    let worker = create_test_worker(&pool, "worker-world-generation", 33100).await;
    let (owner, server) = create_test_server(&pool, "world-generation").await;
    let store = SnapshotStore::new(
        pool.clone(),
        Arc::new(object_store::memory::InMemory::new()),
    );
    let lifecycle = Lifecycle::new(pool.clone());

    // A server made before seeds were recorded.
    let legacy = server_world(&pool, server).await;
    assert_eq!(legacy, WorldGeneration::default());
    assert_eq!(legacy.seed, None);
    snapshot(&store, &pool, server, SnapshotReason::Shutdown).await;

    let desert = NewWorld {
        world_type: WorldType::SingleBiomeSurface,
        biome: Some("minecraft:desert".to_owned()),
        structures: false,
        seed: Some("kurbis".to_owned()),
        ..NewWorld::default()
    }
    .resolve(0)
    .unwrap();
    assert_eq!(desert.seed, Some(-1_120_102_460));
    assert_eq!(
        lifecycle
            .request_reset(server, ResetMode::Full, json!({}), desert.clone())
            .await
            .unwrap(),
        ObservedState::Resetting
    );
    let reset = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert_eq!(
        serde_json::from_value::<WorldGeneration>(reset.payload["world"].clone()).unwrap(),
        desert
    );
    // Requesting a reset changes nothing yet: the old world is still on disk.
    assert_eq!(server_world(&pool, server).await, legacy);

    update_operation_payload(
        &pool,
        &reset,
        OperationProgress::stage(OperationStage::Stopped),
    )
    .await
    .unwrap();
    let (backup, _) = snapshot(&store, &pool, server, SnapshotReason::Reset).await;
    assert_eq!(snapshot_world(&pool, server, backup).await, legacy);
    update_operation_payload(&pool, &reset, OperationProgress::backed_up(backup))
        .await
        .unwrap();
    assert_eq!(server_world(&pool, server).await, legacy);
    update_operation_payload(
        &pool,
        &reset,
        OperationProgress::stage(OperationStage::Cleared),
    )
    .await
    .unwrap();
    assert_eq!(server_world(&pool, server).await, desert);
    // A retried stage report from a recovered worker is idempotent.
    update_operation_payload(
        &pool,
        &reset,
        OperationProgress::stage(OperationStage::Cleared),
    )
    .await
    .unwrap();
    update_operation_payload(
        &pool,
        &reset,
        OperationProgress::stage(OperationStage::Initialized),
    )
    .await
    .unwrap();
    let (fresh, fresh_size) = snapshot(&store, &pool, server, SnapshotReason::Reset).await;
    assert_eq!(snapshot_world(&pool, server, fresh).await, desert);
    complete_reset(&pool, &reset, fresh, fresh_size, false)
        .await
        .unwrap();
    let (state, settings): (ObservedState, serde_json::Value) =
        sqlx::query_as("SELECT observed_state, settings FROM servers WHERE id = $1")
            .bind(server)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((state, settings), (ObservedState::Sleeping, json!({})));
    assert_eq!(server_world(&pool, server).await, desert);

    // Saving settings never touches the world.
    sqlx::query("UPDATE servers SET settings = '{\"motd\": \"changed\"}' WHERE id = $1")
        .bind(server)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(server_world(&pool, server).await, desert);

    // Restoring the pre-reset backup brings the old world back.
    assert_eq!(
        lifecycle.request_restore(server, backup).await.unwrap(),
        ObservedState::Restoring
    );
    let restore = claim_operation(&pool, worker).await.unwrap().unwrap();
    assert_eq!(server_world(&pool, server).await, desert);
    update_operation_payload(
        &pool,
        &restore,
        OperationProgress::stage(OperationStage::Restored),
    )
    .await
    .unwrap();
    assert_eq!(server_world(&pool, server).await, legacy);
    let (restored, restored_size) = snapshot(&store, &pool, server, SnapshotReason::Restore).await;
    assert_eq!(snapshot_world(&pool, server, restored).await, legacy);
    complete_restore(&pool, &restore, backup, restored, restored_size)
        .await
        .unwrap();
    assert_eq!(server_world(&pool, server).await, legacy);

    cleanup_test_entities(&pool, owner, &[worker]).await;
}
