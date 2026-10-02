mod common;

use std::sync::Arc;

use bytes::Bytes;
use common::{create_user, test_pool};
use kurbis_hoster::control_plane::maintenance::WorkerMaintenance;
use kurbis_hoster::snapshot::physical_object_path;
use object_store::{ObjectStore, local::LocalFileSystem};
use sqlx::PgPool;
use tokio::sync::Mutex;
use uuid::Uuid;

static DB_TEST_LOCK: Mutex<()> = Mutex::const_new(());

#[tokio::test]
async fn control_plane_assigns_pending_work_reclaims_stale_exports_and_expires_archives() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    sqlx::query("DELETE FROM users")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers")
        .execute(&pool)
        .await
        .unwrap();
    let objects = tempfile::tempdir().unwrap();
    let store: Arc<dyn ObjectStore> =
        Arc::new(LocalFileSystem::new_with_prefix(objects.path()).unwrap());
    let maintenance = WorkerMaintenance::new(pool.clone(), store.clone());

    let owner_ids = [
        make_owner(&pool, "pending-export").await,
        make_owner(&pool, "stale-export").await,
        make_owner(&pool, "delete-op").await,
        make_owner(&pool, "expired-export").await,
    ];
    let server_ids = [
        make_server(&pool, owner_ids[0], "pending-export").await,
        make_server(&pool, owner_ids[1], "stale-export").await,
        make_server(&pool, owner_ids[2], "delete-op").await,
        make_server(&pool, owner_ids[3], "expired-export").await,
    ];
    let stale_worker = make_worker(&pool, "stale", true).await;
    let healthy_worker = make_worker(&pool, "healthy", false).await;

    let pending_export = Uuid::new_v4();
    insert_export(
        &pool,
        pending_export,
        server_ids[0],
        "pending",
        "ready",
        Some(1),
        None,
        0,
        None,
    )
    .await;
    let stale_export = Uuid::new_v4();
    insert_export(
        &pool,
        stale_export,
        server_ids[1],
        "running",
        "archiving",
        Some(1),
        Some(stale_worker),
        3,
        None,
    )
    .await;
    let delete_operation = Uuid::new_v4();
    sqlx::query(
        "UPDATE servers SET desired_state = 'deleted', observed_state = 'deleted'
         WHERE id = $1",
    )
    .bind(server_ids[2])
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO operations(id, server_id, worker_id, kind, status, generation, payload)
         VALUES($1, $2, NULL, 'delete', 'pending', 0, '{\"reason\":\"test\"}')",
    )
    .bind(delete_operation)
    .bind(server_ids[2])
    .execute(&pool)
    .await
    .unwrap();

    let expired_export = Uuid::new_v4();
    let expired_key = format!("exports/{expired_export}/2.zip");
    insert_export(
        &pool,
        expired_export,
        server_ids[3],
        "completed",
        "completed",
        Some(1),
        None,
        2,
        Some(&expired_key),
    )
    .await;
    let physical_expired_key = physical_object_path(server_ids[3], &expired_key).unwrap();
    store
        .put(
            &physical_expired_key,
            Bytes::from_static(b"expired archive").into(),
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE world_exports SET archive_size = 15, archive_checksum = $2,
             expires_at = now() - interval '1 minute'
         WHERE id = $1",
    )
    .bind(expired_export)
    .bind(blake3::hash(b"expired archive").to_hex().to_string())
    .execute(&pool)
    .await
    .unwrap();

    let report = maintenance.run_once().await.unwrap();
    assert_eq!(report.operations_assigned, 1);
    assert_eq!(report.exports_requeued, 1);
    assert_eq!(report.exports_assigned, 2);
    assert_eq!(report.exports_expired, 1);
    assert_eq!(report.garbage_collection.exports_deleted, 1);

    let assigned_operation: Option<Uuid> =
        sqlx::query_scalar("SELECT worker_id FROM operations WHERE id = $1 AND status = 'pending'")
            .bind(delete_operation)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(assigned_operation, Some(healthy_worker));

    type ExportAssignment = (Uuid, String, Option<Uuid>, i32, Option<String>);
    let assigned_exports: Vec<ExportAssignment> = sqlx::query_as(
        "SELECT id, status, worker_id, attempt, stage FROM world_exports
         WHERE id IN ($1, $2) ORDER BY id",
    )
    .bind(pending_export)
    .bind(stale_export)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        assigned_exports
            .iter()
            .all(|(_, status, worker, _, stage)| {
                status == "pending"
                    && *worker == Some(healthy_worker)
                    && stage.as_deref() == Some("ready")
            })
    );
    assert_eq!(
        assigned_exports
            .iter()
            .find(|(id, ..)| *id == stale_export)
            .map(|(_, _, _, attempt, _)| *attempt),
        Some(3),
        "reassignment preserves the old attempt until the replacement worker claims it"
    );
    let expired_status: String =
        sqlx::query_scalar("SELECT status FROM world_exports WHERE id = $1")
            .bind(expired_export)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(expired_status, "expired");
    assert!(matches!(
        store.head(&physical_expired_key).await,
        Err(object_store::Error::NotFound { .. })
    ));

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(&owner_ids[..])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers WHERE id = ANY($1)")
        .bind(vec![stale_worker, healthy_worker])
        .execute(&pool)
        .await
        .unwrap();
}

async fn make_owner(pool: &PgPool, label: &str) -> Uuid {
    create_user(
        pool,
        "google",
        &format!("maintenance-{label}-{}", Uuid::new_v4()),
        label,
        "active",
        None,
    )
    .await
}

async fn make_server(pool: &PgPool, owner_id: Uuid, label: &str) -> Uuid {
    let server_id = Uuid::new_v4();
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
        .bind(server_id)
        .bind(owner_id)
        .bind(format!(
            "maint-{label}-{}",
            &server_id.simple().to_string()[..8]
        ))
        .execute(pool)
        .await
        .unwrap();
    server_id
}

async fn make_worker(pool: &PgPool, label: &str, stale: bool) -> Uuid {
    let worker_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workers(id, name, address, signal_address, total_memory_bytes,
                            total_disk_bytes, total_cpu_millis, port_min, port_max, heartbeat_at)
         VALUES($1, $2, '203.0.113.10', '10.20.0.10', 8589934592,
                42949672960, 4000, 25000, 25100,
                CASE WHEN $3 THEN now() - interval '2 minutes' ELSE now() END)",
    )
    .bind(worker_id)
    .bind(format!("maintenance-{label}-{worker_id}"))
    .bind(stale)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO worker_credentials(worker_id, api_token_hash)
         VALUES($1, $2)",
    )
    .bind(worker_id)
    .bind(blake3::hash(worker_id.as_bytes()).as_bytes().to_vec())
    .execute(pool)
    .await
    .unwrap();
    worker_id
}

#[allow(clippy::too_many_arguments)] // Fixture rows intentionally expose each lifecycle column.
async fn insert_export(
    pool: &PgPool,
    export_id: Uuid,
    server_id: Uuid,
    status: &str,
    stage: &str,
    source_generation: Option<i64>,
    worker_id: Option<Uuid>,
    attempt: i32,
    archive_key: Option<&str>,
) {
    let completed = status == "completed";
    sqlx::query(
        "INSERT INTO world_exports(
             id, server_id, requested_by, status, stage, source_generation, worker_id,
             attempt, archive_key, archive_size, archive_checksum, expires_at
         ) VALUES($1, $2, (SELECT owner_id FROM servers WHERE id = $2), $3, $4, $5, $6,
                  $7, $8, CASE WHEN $9 THEN 15 END,
                  CASE WHEN $9 THEN repeat('a', 64) END,
                  CASE WHEN $9 THEN now() + interval '1 day' END)",
    )
    .bind(export_id)
    .bind(server_id)
    .bind(status)
    .bind(stage)
    .bind(source_generation)
    .bind(worker_id)
    .bind(attempt)
    .bind(archive_key)
    .bind(completed)
    .execute(pool)
    .await
    .unwrap();
}
