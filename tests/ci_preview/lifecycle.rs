use super::*;
use axum::http::Method;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use flate2::{Compression, write::GzEncoder};
use kurbis_hoster::ci::{
    CI_RUNTIME_DISK_BYTES, CiPreviewRuntimeRepository, CiRuntimeState, RuntimeLease,
};
use object_store::memory::InMemory;
use sqlx::PgPool;
use std::{io::Write, sync::Arc};
use uuid::Uuid;

struct WorkerFixture {
    id: Uuid,
    token: String,
    lease: Uuid,
}

async fn ready_preview(pool: &PgPool, suffix: &str) -> (Uuid, String, Uuid, String) {
    let watched = CiPreviewRepository::new(pool.clone())
        .register_repository(&format!("kurbis-host/Pumpkin-{suffix}"), "main")
        .await
        .unwrap();
    let repository = watched.repository.clone();
    let sha = format!("{:040x}", rand::random::<u64>());
    let key = BuildKey::new(
        &repository,
        &sha,
        "patch-lifecycle-fixture",
        "rust-1.98@sha256:lifecycle-fixture",
        "x86_64-linux",
    )
    .unwrap();
    let previews = CiPreviewRepository::new(pool.clone());
    let (build, _) = previews.ensure_build(&key).await.unwrap();
    for stage in [
        CiBuildStage::Fetching,
        CiBuildStage::Patching,
        CiBuildStage::Compiling,
        CiBuildStage::Packaging,
    ] {
        previews
            .transition_build(build.id, stage, None)
            .await
            .unwrap();
    }
    let digest = format!("sha256:{}", Uuid::new_v4().simple());
    previews
        .attach_artifact(build.id, &digest, &format!("ci/{suffix}.tar"), 256)
        .await
        .unwrap();
    previews
        .transition_build(build.id, CiBuildStage::Ready, None)
        .await
        .unwrap();
    let hostname = CiHostname::for_ref(
        CiRefKind::Commit,
        &key.commit_sha,
        kurbis_hoster::ci::CI_ROOT_DOMAIN,
    )
    .unwrap();
    let preview = previews
        .ensure_preview(
            &repository,
            &key.commit_sha,
            CiRefKind::Commit,
            None,
            &hostname,
            build.id,
        )
        .await
        .unwrap();
    (preview.id, hostname.value, watched.id, digest)
}

async fn create_worker(pool: &PgPool, suffix: &str, port_min: i32) -> WorkerFixture {
    let id = Uuid::new_v4();
    let token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let lease = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workers(id,name,address,signal_address,total_memory_bytes,total_disk_bytes,total_cpu_millis,
             heartbeat_at,draining,port_min,port_max,runtime_architecture,supports_ci_previews)
         VALUES($1,$2,'198.51.100.151','10.0.0.151',4294967296,21474836480,4000,
             now()+interval '5 minutes',false,$3,$4,'x86_64-linux',true)",
    )
    .bind(id)
    .bind(format!("ci-lifecycle-{suffix}-{}", Uuid::new_v4()))
    .bind(port_min)
    .bind(port_min + 20)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO worker_credentials(worker_id,api_token_hash,lease_id,lease_expires_at)
         VALUES($1,$2,$3,now()+interval '1 hour')",
    )
    .bind(id)
    .bind(blake3::hash(token.as_bytes()).as_bytes().to_vec())
    .bind(lease)
    .execute(pool)
    .await
    .unwrap();
    WorkerFixture { id, token, lease }
}

fn world_archive() -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut archive = tar::Builder::new(&mut encoder);
        let bytes = b"saved-bytes-from-quiescent-world";
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        archive
            .append_data(&mut header, "world/level.dat", &bytes[..])
            .unwrap();
        archive.finish().unwrap();
    }
    encoder.finish().unwrap()
}

async fn worker_request(
    service: axum::Router,
    method: axum::http::Method,
    uri: &str,
    worker: &WorkerFixture,
    content_length: Option<usize>,
    body: axum::body::Body,
) -> axum::response::Response {
    let content_type = if method == Method::PUT {
        "application/gzip"
    } else {
        "application/json"
    };
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {}", worker.token))
        .header("x-worker-lease", worker.lease.to_string())
        .header("content-type", content_type);
    let request = if let Some(content_length) = content_length {
        request.header("content-length", content_length)
    } else {
        request
    }
    .body(body)
    .unwrap();
    service.oneshot(request).await.unwrap()
}

async fn remove_fixture(
    pool: &PgPool,
    preview_id: Uuid,
    watched_id: Uuid,
    artifact_digest: &str,
    workers: &[WorkerFixture],
) {
    for worker in workers {
        sqlx::query("DELETE FROM worker_credentials WHERE worker_id=$1")
            .bind(worker.id)
            .execute(pool)
            .await
            .unwrap();
    }
    sqlx::query("DELETE FROM ci_preview_aliases WHERE id=$1")
        .bind(preview_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_builds WHERE artifact_digest=$1")
        .bind(artifact_digest)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_artifacts WHERE digest=$1")
        .bind(artifact_digest)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_watched_repositories WHERE id=$1")
        .bind(watched_id)
        .execute(pool)
        .await
        .unwrap();
    for worker in workers {
        sqlx::query("DELETE FROM workers WHERE id=$1")
            .bind(worker.id)
            .execute(pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn stopped_preview_waits_for_join_and_old_worker_is_fenced_after_restart() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let (preview_id, hostname, watched_id, digest) = ready_preview(&pool, "stopped-join").await;
    let first_worker = create_worker(&pool, "stopped-first", 29100).await;
    let runtimes = CiPreviewRuntimeRepository::new(pool.clone());
    let store = Arc::new(InMemory::new());
    let service = kurbis_hoster::control_plane::api::router(common::api_state(
        pool.clone(),
        "snapshot-api-internal-secret",
        store.clone(),
    ));

    let RuntimeLease::Claimed(starting) = runtimes
        .claim_start(preview_id, &[21_u8; 32])
        .await
        .unwrap()
    else {
        panic!("ready preview should claim its first runtime generation");
    };
    assert_eq!(starting.worker_id, first_worker.id);
    let starting_worker = &first_worker;
    let old_capability = "old-generation-capability";
    assert!(
        runtimes
            .replace_capability_hash(
                preview_id,
                starting.worker_id,
                starting.generation,
                starting.lease_token,
                blake3::hash(old_capability.as_bytes()).as_bytes(),
            )
            .await
            .unwrap()
    );
    assert!(
        runtimes
            .capability_valid(preview_id, starting.generation, old_capability)
            .await
            .unwrap()
    );
    assert!(
        runtimes
            .mark_running(
                preview_id,
                starting.worker_id,
                starting.generation,
                starting.lease_token,
            )
            .await
            .unwrap()
    );

    // Save an actual world archive through the authenticated worker router,
    // then download, verify its digest, and restore its contents locally.
    let world = world_archive();
    assert!(runtimes.request_world_snapshot(preview_id).await.unwrap());
    let upload_uri = format!(
        "/internal/worker/control/ci/runtimes/{}/world-snapshot/{}/{}",
        preview_id, starting.generation, starting.lease_token
    );
    let uploaded = worker_request(
        service.clone(),
        Method::PUT,
        &upload_uri,
        starting_worker,
        Some(world.len()),
        Body::from(world.clone()),
    )
    .await;
    assert_eq!(uploaded.status(), StatusCode::NO_CONTENT);
    let metadata: kurbis_hoster::ci::runtime::CiWorldSnapshot = sqlx::query_as(
        "SELECT object_key,size_bytes,checksum,source_generation FROM ci_world_snapshots WHERE preview_id=$1",
    )
    .bind(preview_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(metadata.checksum, blake3::hash(&world).to_hex().to_string());
    assert_eq!(metadata.size_bytes, world.len() as i64);
    let second_worker = create_worker(&pool, "stopped-second", 29200).await;
    // Drive a real running -> stopping -> stopped transition, then let its
    // worker heartbeat expire. Maintenance must leave the retained world idle;
    // only the later user/join admission may claim its next generation.
    sqlx::query(
        "UPDATE ci_runtime_placements SET last_activity_at=now()-interval '10 minutes'
         WHERE preview_id=$1",
    )
    .bind(preview_id)
    .execute(&pool)
    .await
    .unwrap();
    let stopping = runtimes
        .claim_idle(chrono::Duration::seconds(300), 16)
        .await
        .unwrap()
        .pop()
        .expect("idle running placement is claimed for stop");
    assert_eq!(stopping.state, CiRuntimeState::Stopping);
    assert!(runtimes.complete_idle_stop(&stopping, true).await.unwrap());
    let stopped = runtimes.current(preview_id).await.unwrap().unwrap();
    assert_eq!(stopped.state, CiRuntimeState::Stopped);
    assert!(!stopped.active);
    assert_eq!(stopped.worker_id, starting.worker_id);
    let assigned_worker = if stopped.worker_id == first_worker.id {
        &first_worker
    } else {
        &second_worker
    };
    let unassigned_worker = if stopped.worker_id == first_worker.id {
        &second_worker
    } else {
        &first_worker
    };
    sqlx::query("UPDATE workers SET heartbeat_at=now()-interval '10 minutes' WHERE id=$1")
        .bind(assigned_worker.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(runtimes.reconcile_stale_workers(16).await.unwrap(), 0);
    assert_eq!(
        runtimes.current(preview_id).await.unwrap().unwrap(),
        stopped
    );
    let alias_state: String =
        sqlx::query_scalar("SELECT state FROM ci_preview_aliases WHERE id=$1")
            .bind(preview_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(alias_state, "ready");

    let join_request = Request::builder()
        .method(Method::POST)
        .uri("/internal/ci/previews/admit")
        .header("authorization", "Bearer snapshot-api-internal-secret")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"hostname": hostname}).to_string(),
        ))
        .unwrap();
    let join_response = service.clone().oneshot(join_request).await.unwrap();
    assert_eq!(join_response.status(), StatusCode::ACCEPTED);
    let joined: serde_json::Value =
        serde_json::from_slice(&to_bytes(join_response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(joined["state"], "booting");
    let restarted = runtimes.current(preview_id).await.unwrap().unwrap();
    assert_eq!(restarted.generation, stopped.generation + 1);
    assert_eq!(restarted.worker_id, unassigned_worker.id);
    assert_eq!(restarted.state, CiRuntimeState::Starting);
    let restarted_snapshot_uri = format!(
        "/internal/worker/control/ci/runtimes/{}/world-snapshot/{}/{}",
        preview_id, restarted.generation, restarted.lease_token
    );
    let downloaded = worker_request(
        service.clone(),
        Method::GET,
        &restarted_snapshot_uri,
        unassigned_worker,
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(downloaded.status(), StatusCode::OK);
    let bytes = to_bytes(downloaded.into_body(), CI_RUNTIME_DISK_BYTES as usize)
        .await
        .unwrap();
    assert_eq!(bytes.as_ref(), world.as_slice());
    assert_eq!(blake3::hash(&bytes).to_hex().as_str(), metadata.checksum);
    let restore = tempfile::tempdir().unwrap();
    let decoder = flate2::read::GzDecoder::new(bytes.as_ref());
    tar::Archive::new(decoder).unpack(restore.path()).unwrap();
    assert_eq!(
        std::fs::read(restore.path().join("world/level.dat")).unwrap(),
        b"saved-bytes-from-quiescent-world"
    );
    let old_generation_uri = format!(
        "/internal/worker/control/ci/runtimes/{}/world-snapshot/{}/{}",
        preview_id, stopped.generation, stopped.lease_token
    );
    let stale_worker_response = worker_request(
        service,
        Method::GET,
        &old_generation_uri,
        assigned_worker,
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(stale_worker_response.status(), StatusCode::CONFLICT);
    assert!(
        !runtimes
            .capability_valid(preview_id, restarted.generation, old_capability)
            .await
            .unwrap(),
        "the generation and its capability both rotate after the explicit restart"
    );

    remove_fixture(
        &pool,
        preview_id,
        watched_id,
        &digest,
        &[first_worker, second_worker],
    )
    .await;
    pool.close().await;
}

#[tokio::test]
async fn expired_runtime_deletion_finishes_when_snapshot_object_is_already_missing() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let (preview_id, _hostname, watched_id, digest) = ready_preview(&pool, "missing-object").await;
    let worker = create_worker(&pool, "missing-object", 29300).await;
    let runtimes = CiPreviewRuntimeRepository::new(pool.clone());
    let RuntimeLease::Claimed(starting) = runtimes
        .claim_start(preview_id, &[31_u8; 32])
        .await
        .unwrap()
    else {
        panic!("ready preview should claim its first runtime generation");
    };
    assert!(
        runtimes
            .mark_running(
                preview_id,
                starting.worker_id,
                starting.generation,
                starting.lease_token,
            )
            .await
            .unwrap()
    );
    assert!(runtimes.request_world_snapshot(preview_id).await.unwrap());

    let object_root = tempfile::tempdir().unwrap();
    let store = Arc::new(
        object_store::local::LocalFileSystem::new_with_prefix(object_root.path()).unwrap(),
    );
    let service = kurbis_hoster::control_plane::api::router(common::api_state(
        pool.clone(),
        "snapshot-api-internal-secret",
        store,
    ));
    let world = world_archive();
    let upload_uri = format!(
        "/internal/worker/control/ci/runtimes/{}/world-snapshot/{}/{}",
        preview_id, starting.generation, starting.lease_token
    );
    assert!(
        runtimes
            .world_snapshot_requested(
                preview_id,
                worker.id,
                starting.generation,
                starting.lease_token,
            )
            .await
            .unwrap()
    );
    let upload = worker_request(
        service.clone(),
        Method::PUT,
        &upload_uri,
        &worker,
        Some(world.len()),
        Body::from(world),
    )
    .await;
    if upload.status() != StatusCode::NO_CONTENT {
        let (parts, body) = upload.into_parts();
        panic!(
            "snapshot upload status {}: {}",
            parts.status,
            String::from_utf8_lossy(&to_bytes(body, 4096).await.unwrap())
        );
    }
    let metadata: kurbis_hoster::ci::runtime::CiWorldSnapshot = sqlx::query_as(
        "SELECT object_key,size_bytes,checksum,source_generation FROM ci_world_snapshots WHERE preview_id=$1",
    )
    .bind(preview_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let physical_path = object_root.path().join(&metadata.object_key);
    assert!(physical_path.is_file());
    std::fs::remove_file(&physical_path).unwrap();

    sqlx::query("UPDATE ci_runtime_placements SET state='deleting' WHERE preview_id=$1")
        .bind(preview_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE ci_preview_aliases SET state='expired',expires_at=now()-interval '1 second' WHERE id=$1",
    )
    .bind(preview_id)
    .execute(&pool)
    .await
    .unwrap();
    let deleted = worker_request(
        service,
        Method::POST,
        &format!("/internal/worker/control/ci/runtimes/{preview_id}/deleted"),
        &worker,
        None,
        Body::from(
            serde_json::json!({
                "generation": starting.generation,
                "lease_token": starting.lease_token,
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(runtimes.current(preview_id).await.unwrap().is_none());
    assert!(
        CiPreviewRepository::new(pool.clone())
            .lookup_preview_by_id(preview_id)
            .await
            .unwrap()
            .is_none()
    );

    remove_fixture(&pool, preview_id, watched_id, &digest, &[worker]).await;
    pool.close().await;
}
