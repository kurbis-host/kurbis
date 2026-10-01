mod common;

use std::path::Path;

use async_zip::tokio::read::seek::ZipFileReader;
use common::{create_user, test_pool};
use kurbis_hoster::{
    config::ObjectStoreConfig, model::SnapshotReason, worker::snapshot::SnapshotManager,
};
use sqlx::PgPool;
use tempfile::TempDir;
use tokio::{fs, io::BufReader};
use uuid::Uuid;

static DB_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn write(root: &Path, path: &str, data: &[u8]) {
    let target = root.join(path);
    fs::create_dir_all(target.parent().unwrap()).await.unwrap();
    fs::write(target, data).await.unwrap();
}

async fn create_server(pool: &PgPool, owner_id: Uuid) -> Uuid {
    let server_id = Uuid::new_v4();
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
        .bind(server_id)
        .bind(owner_id)
        .bind(format!("world-{}", &server_id.simple().to_string()[..12]))
        .execute(pool)
        .await
        .unwrap();
    server_id
}

#[tokio::test]
#[allow(deprecated)]
async fn exports_stream_zip64_anvil_files_normalize_pumpkin_progress_and_pin_sources() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let owner_id = create_user(
        &pool,
        "google",
        &format!("world-export-{}", Uuid::new_v4()),
        "World export test",
        "active",
        None,
    )
    .await;
    let server_id = create_server(&pool, owner_id).await;
    let export_id = Uuid::new_v4();
    let identity = Uuid::new_v4();
    let workspace = TempDir::new().unwrap();
    let server_root = workspace.path().join("server");
    let object_root = workspace.path().join("objects");
    let staging_root = workspace.path().join("staging");
    fs::create_dir_all(&server_root).await.unwrap();

    write(&server_root, "world/level.dat", b"level metadata").await;
    write(
        &server_root,
        "world/region/r.0.0.mca",
        b"overworld anvil region",
    )
    .await;
    write(
        &server_root,
        "world/DIM-1/region/r.0.0.mca",
        b"nether anvil region",
    )
    .await;
    write(
        &server_root,
        &format!("world/players/data/{identity}.dat"),
        b"player inventory and progress",
    )
    .await;
    write(
        &server_root,
        &format!("world/players/advancements/{identity}.json"),
        b"{\"minecraft:story/root\":{}}",
    )
    .await;
    write(
        &server_root,
        &format!("world/players/stats/{identity}.json"),
        b"{\"minecraft:custom\":{}}",
    )
    .await;
    write(&server_root, "world/players/plugin-private.bin", b"exclude").await;
    write(&server_root, "world/session.lock", b"runtime lock").await;
    write(&server_root, "ops.json", b"operator credentials").await;
    write(&server_root, "logs/latest.log", b"server log").await;
    write(&server_root, "run/host-control.sock", b"socket").await;

    let object_config = ObjectStoreConfig::Local { root: object_root };
    let snapshots = SnapshotManager::new(pool.clone(), &object_config)
        .await
        .unwrap();
    let source = snapshots
        .create(server_id, &server_root, SnapshotReason::Manual)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO world_exports(id, server_id, requested_by, status, stage, source_generation)
         VALUES($1, $2, $3, 'running', 'archiving', $4)",
    )
    .bind(export_id)
    .bind(server_id)
    .bind(owner_id)
    .bind(source.generation)
    .execute(&pool)
    .await
    .unwrap();

    for _ in 0..5 {
        snapshots
            .create(server_id, &server_root, SnapshotReason::Periodic)
            .await
            .unwrap();
    }
    let pinned: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM snapshots WHERE server_id = $1 AND generation = $2)",
    )
    .bind(server_id)
    .bind(source.generation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        pinned,
        "retention must preserve an export's source generation"
    );

    let archive = snapshots
        .create_world_archive(export_id, server_id, source.generation, 1, &staging_root)
        .await
        .unwrap();
    assert!(archive.size_bytes > 0);
    assert_eq!(
        blake3::hash(&fs::read(&archive.path).await.unwrap())
            .to_hex()
            .to_string(),
        archive.checksum
    );

    let file = fs::File::open(&archive.path).await.unwrap();
    let mut zip = ZipFileReader::with_tokio(BufReader::new(file))
        .await
        .unwrap();
    assert!(zip.file().zip64(), "world archive must use ZIP64 records");
    let names = zip
        .file()
        .entries()
        .iter()
        .map(|entry| entry.filename().as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    for expected in [
        "level.dat",
        "region/r.0.0.mca",
        "DIM-1/region/r.0.0.mca",
        &format!("playerdata/{identity}.dat"),
        &format!("advancements/{identity}.json"),
        &format!("stats/{identity}.json"),
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "missing {expected}"
        );
    }
    for excluded in [
        "session.lock",
        "players/plugin-private.bin",
        "ops.json",
        "latest.log",
        "host-control.sock",
    ] {
        assert!(
            !names.iter().any(|name| name == excluded),
            "included {excluded}"
        );
    }

    let level_index = names.iter().position(|name| name == "level.dat").unwrap();
    let mut level_reader = zip.reader_with_entry(level_index).await.unwrap();
    let mut level_bytes = Vec::new();
    level_reader
        .read_to_end_checked(&mut level_bytes)
        .await
        .unwrap();
    assert_eq!(level_bytes, b"level metadata");
    drop(level_reader);

    let player_index = names
        .iter()
        .position(|name| name == &format!("playerdata/{identity}.dat"))
        .unwrap();
    let mut player_reader = zip.reader_with_entry(player_index).await.unwrap();
    let mut player_bytes = Vec::new();
    player_reader
        .read_to_end_checked(&mut player_bytes)
        .await
        .unwrap();
    assert_eq!(player_bytes, b"player inventory and progress");
    drop(player_reader);

    snapshots.upload_world_archive(&archive).await.unwrap();
    let archive_store = object_config.build().await.unwrap();
    let uploaded = archive_store
        .get(&object_store::path::Path::from(archive.archive_key.clone()))
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(uploaded.len() as i64, archive.size_bytes);
    assert_eq!(
        blake3::hash(&uploaded).to_hex().to_string(),
        archive.checksum
    );

    sqlx::query(
        "UPDATE world_exports SET status = 'completed', stage = 'completed',
             archive_key = $2, archive_size = $3, archive_checksum = $4,
             expires_at = now() + interval '24 hours'
         WHERE id = $1",
    )
    .bind(export_id)
    .bind(&archive.archive_key)
    .bind(archive.size_bytes)
    .bind(&archive.checksum)
    .execute(&pool)
    .await
    .unwrap();
    snapshots
        .create(server_id, &server_root, SnapshotReason::Periodic)
        .await
        .unwrap();
    let pin_released: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM snapshots WHERE server_id = $1 AND generation = $2)",
    )
    .bind(server_id)
    .bind(source.generation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        !pin_released,
        "retention can prune a source after export completion"
    );

    snapshots.remove_world_archive_staging(&archive).await;
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner_id)
        .execute(&pool)
        .await
        .unwrap();
}
