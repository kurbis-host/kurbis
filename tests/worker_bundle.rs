mod common;

#[path = "persistence/source_boundaries.rs"]
mod source_boundaries;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Command, Output},
};

use common::{create_user, test_pool};
use sqlx::PgPool;
use uuid::Uuid;

#[test]
fn worker_runtime_has_no_database_or_bucket_credential_boundary() {
    source_boundaries::assert_rust_tree_excludes(
        "src/worker",
        &[
            "sqlx::",
            "PgPool",
            "crate::db",
            "crate::control_plane",
            "DATABASE_URL",
            "WORKER_DATABASE_URL",
            "OBJECT_STORE_TYPE",
            "OBJECT_STORE_LOCAL_ROOT",
            "S3_ACCESS_KEY",
            "S3_SECRET_KEY",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AmazonS3Builder",
        ],
    );
}

fn provision(output: &std::path::Path, worker_id: Uuid, rotate: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_worker-bundle"));
    command
        .args(["--worker-id", &worker_id.to_string(), "--output"])
        .arg(output)
        .env(
            "WORKER_PROVISIONING_DATABASE_URL",
            std::env::var("TEST_DATABASE_URL").expect("test database is configured"),
        )
        .env("WORKER_NAME", format!("provisioned-{worker_id}"))
        .env("WORKER_ADDRESS", "198.51.100.20")
        .env("WORKER_SIGNAL_ADDRESS", "10.0.0.20")
        .env("WORKER_CONTROL_PLANE_URL", "https://control.example.test")
        .env("PUBLIC_BASE_URL", "https://panel.example.test")
        .env("WORKER_RUNTIME", "native")
        .env("WORKER_STORAGE_MODE", "directory")
        .env("WORKER_ROOT", "/srv/kurbis-worker")
        .env("WORKER_MEMORY_GIB", "8")
        .env("WORKER_DISK_GIB", "40")
        .env("WORKER_CPU_MILLIS", "4000")
        .env("WORKER_PORT_MIN", "20000")
        .env("WORKER_PORT_MAX", "20040")
        .env("PUMPKIN_BINARY", "/usr/bin/pumpkin")
        .env("WORKER_API_TOKEN", "global-worker-token-fallback-sentinel")
        .env(
            "PUMPKIN_HOST_CONTROL_TOKEN",
            "global-host-control-fallback-sentinel",
        )
        .env("DATABASE_URL", "operator-database-fallback-sentinel")
        .env("INTERNAL_TOKEN", "operator-internal-fallback-sentinel");
    if rotate {
        command.arg("--rotate");
    }
    command.output().expect("run worker bundle provisioner")
}

fn bundle_value(bundle: &str, name: &str) -> String {
    bundle
        .lines()
        .find_map(|line| {
            let value = line.strip_prefix(&format!("export {name}='"))?;
            Some(
                value
                    .strip_suffix('\'')
                    .expect("quoted bundle value")
                    .to_owned(),
            )
        })
        .unwrap_or_else(|| panic!("missing {name} in worker bundle"))
}

async fn insert_active_placement(pool: &PgPool, worker_id: Uuid) -> Uuid {
    let user_id = create_user(
        pool,
        "google",
        &format!("worker-bundle-{worker_id}"),
        "Worker bundle test",
        "active",
        None,
    )
    .await;
    let server_id = Uuid::new_v4();
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
        .bind(server_id)
        .bind(user_id)
        .bind(format!(
            "provision-{}",
            &server_id.simple().to_string()[..16]
        ))
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO placements(
             server_id, worker_id, generation, lease_token, java_port, bedrock_port,
             signal_port, active
         ) VALUES($1, $2, 1, $3, 20000, 20001, 20002, true)",
    )
    .bind(server_id)
    .bind(worker_id)
    .bind(Uuid::new_v4())
    .execute(pool)
    .await
    .unwrap();
    server_id
}

#[tokio::test]
async fn worker_bundle_is_api_only_and_provisioning_rotates_only_without_active_assignments() {
    let pool = test_pool().await;
    let initial_role_count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_roles")
        .fetch_one(&pool)
        .await
        .unwrap();
    let worker_id = Uuid::new_v4();
    let directory = tempfile::tempdir().unwrap();
    let bundle_path = directory.path().join("worker.env");

    let created = provision(&bundle_path, worker_id, false);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let first_bundle = fs::read_to_string(&bundle_path).unwrap();
    assert_eq!(
        fs::metadata(&bundle_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let first_api_token = bundle_value(&first_bundle, "WORKER_API_TOKEN");
    let first_host_token = bundle_value(&first_bundle, "PUMPKIN_HOST_CONTROL_TOKEN");
    assert_eq!(first_api_token.len(), 43);
    assert_eq!(first_host_token.len(), 43);
    assert_ne!(first_api_token, first_host_token);
    assert_ne!(first_host_token, "global-host-control-fallback-sentinel");
    for forbidden in [
        "DATABASE_URL",
        "WORKER_DATABASE_URL",
        "WORKER_PROVISIONING_DATABASE_URL",
        "INTERNAL_TOKEN",
        "PUMPKIN_GATEWAY_CONTROL_TOKEN",
        "OBJECT_STORE_TYPE",
        "OBJECT_STORE_LOCAL_ROOT",
        "S3_ACCESS_KEY",
        "S3_SECRET_KEY",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "CLOUD_CREDENTIALS_JSON",
        "CONTROL_PLANE_INTERNAL_URL",
        "GOOGLE_OAUTH_CLIENT_SECRET",
        "DISCORD_OAUTH_CLIENT_SECRET",
    ] {
        assert!(
            !first_bundle
                .lines()
                .any(|line| line.starts_with(&format!("export {forbidden}="))),
            "worker bundle must not contain {forbidden}"
        );
    }
    for forbidden_value in [
        "global-worker-token-fallback-sentinel",
        "global-host-control-fallback-sentinel",
        "operator-database-fallback-sentinel",
        "operator-internal-fallback-sentinel",
    ] {
        assert!(!first_bundle.contains(forbidden_value));
    }

    let (stored_hash, lease_id, lease_expires_at): (
        Vec<u8>,
        Option<Uuid>,
        Option<chrono::DateTime<chrono::Utc>>,
    ) = sqlx::query_as(
        "SELECT api_token_hash, lease_id, lease_expires_at
             FROM worker_credentials WHERE worker_id = $1",
    )
    .bind(worker_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        stored_hash,
        blake3::hash(first_api_token.as_bytes()).as_bytes().to_vec()
    );
    assert_eq!((lease_id, lease_expires_at), (None, None));

    let duplicate = provision(&bundle_path, worker_id, false);
    assert!(!duplicate.status.success());
    assert_eq!(fs::read_to_string(&bundle_path).unwrap(), first_bundle);

    let server_id = insert_active_placement(&pool, worker_id).await;
    let blocked_rotation = provision(&bundle_path, worker_id, true);
    assert!(!blocked_rotation.status.success());
    assert!(String::from_utf8_lossy(&blocked_rotation.stderr).contains("stop this worker"));
    assert_eq!(fs::read_to_string(&bundle_path).unwrap(), first_bundle);

    sqlx::query("DELETE FROM servers WHERE id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    let rotated = provision(&bundle_path, worker_id, true);
    assert!(
        rotated.status.success(),
        "{}",
        String::from_utf8_lossy(&rotated.stderr)
    );
    let rotated_bundle = fs::read_to_string(&bundle_path).unwrap();
    let rotated_token = bundle_value(&rotated_bundle, "WORKER_API_TOKEN");
    assert_ne!(rotated_token, first_api_token);
    let rotated_hash: Vec<u8> =
        sqlx::query_scalar("SELECT api_token_hash FROM worker_credentials WHERE worker_id = $1")
            .bind(worker_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        rotated_hash,
        blake3::hash(rotated_token.as_bytes()).as_bytes().to_vec()
    );
    let final_role_count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_roles")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(final_role_count, initial_role_count);
}
