use super::*;

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use futures::StreamExt;
use kurbis_hoster::ci::{CiPreviewRuntimeRepository, RuntimeLease};
use object_store::{ObjectStore, memory::InMemory, path::Path as StorePath};
use std::sync::Arc;
use tokio::time::{Duration, timeout};
use tower::ServiceExt;

const STREAM_BYTES: usize = 8 * 1024 * 1024 + 123;

async fn wait_until_download_fence_blocks_writer(pool: &sqlx::PgPool) {
    timeout(Duration::from_secs(2), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                    SELECT 1 FROM pg_stat_activity
                    WHERE pid <> pg_backend_pid()
                      AND wait_event_type = 'Lock'
                      AND state = 'active'
                )",
            )
            .fetch_one(pool)
            .await
            .unwrap();
            if blocked {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("database writer did not block behind the in-flight download fence");
}

async fn assert_revoked_after_current_chunk<S, E, F>(body: &mut S, finish_revocation: F)
where
    S: futures::Stream<Item = Result<Bytes, E>> + Unpin,
    F: std::future::Future<Output = bool>,
{
    let revoked_chunk = timeout(Duration::from_secs(2), body.next())
        .await
        .expect("revoked stream did not resume after current chunk release")
        .expect("revoked stream ended without an authorization error");
    assert!(revoked_chunk.is_err(), "stale download bytes were released");
    assert!(finish_revocation.await, "revocation writer failed");
}

#[tokio::test]
async fn ci_downloads_reauthorize_each_bounded_chunk_and_verify_integrity() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let previews = CiPreviewRepository::new(pool.clone());
    let runtimes = CiPreviewRuntimeRepository::new(pool.clone());
    let watched_repository = format!("kurbis-host/Pumpkin-stream-{}", Uuid::new_v4());
    let watched = previews
        .register_repository(&watched_repository, "main")
        .await
        .unwrap();
    let key = BuildKey::new(
        &watched_repository,
        "0123456789abcdef0123456789abcdef01234567",
        "stream-reauthorization",
        "rust-1.98@sha256:stream",
        "x86_64-linux",
    )
    .unwrap();
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

    let artifact_bytes = Bytes::from(vec![b'a'; STREAM_BYTES]);
    let artifact_digest = blake3::hash(&artifact_bytes).to_hex().to_string();
    let artifact_key = format!("ci/stream-{}.tar.gz", Uuid::new_v4());
    previews
        .attach_artifact(
            build.id,
            &artifact_digest,
            &artifact_key,
            artifact_bytes.len() as i64,
        )
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
            &watched_repository,
            &key.commit_sha,
            CiRefKind::Commit,
            None,
            &hostname,
            build.id,
        )
        .await
        .unwrap();

    let worker_id = Uuid::new_v4();
    let worker_token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let worker_lease = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workers
            (id,name,address,signal_address,total_memory_bytes,total_disk_bytes,total_cpu_millis,
             heartbeat_at,draining,port_min,port_max,runtime_architecture,supports_ci_previews)
         VALUES($1,$2,'198.51.100.201','10.0.0.201',4294967296,21474836480,4000,
             now()+interval '5 minutes',false,29100,29120,'x86_64-linux',true)",
    )
    .bind(worker_id)
    .bind(format!("ci-stream-{}", Uuid::new_v4()))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO worker_credentials(worker_id,api_token_hash,lease_id,lease_expires_at)
         VALUES($1,$2,$3,now()+interval '1 hour')",
    )
    .bind(worker_id)
    .bind(blake3::hash(worker_token.as_bytes()).as_bytes().to_vec())
    .bind(worker_lease)
    .execute(&pool)
    .await
    .unwrap();

    let object_store = Arc::new(InMemory::new());
    object_store
        .put(
            &StorePath::parse(&artifact_key).unwrap(),
            artifact_bytes.clone().into(),
        )
        .await
        .unwrap();
    let service = kurbis_hoster::control_plane::api::router(common::api_state(
        pool.clone(),
        "streaming-internal-token",
        object_store.clone(),
    ));
    let initial = match runtimes.claim_start(preview.id, &[7_u8; 32]).await.unwrap() {
        RuntimeLease::Claimed(placement) => placement,
        lease => panic!("expected a fresh CI placement, got {lease:?}"),
    };
    let snapshot_bytes = Bytes::from(vec![b'b'; STREAM_BYTES]);
    let snapshot_key = format!(
        "ci-previews/{}/worlds/stream-{}.tar.gz",
        preview.id,
        Uuid::new_v4()
    );
    object_store
        .put(
            &StorePath::parse(&snapshot_key).unwrap(),
            snapshot_bytes.clone().into(),
        )
        .await
        .unwrap();
    let snapshot_digest = blake3::hash(&snapshot_bytes).to_hex().to_string();
    assert!(
        runtimes
            .mark_world_initialized(
                preview.id,
                worker_id,
                initial.generation,
                initial.lease_token
            )
            .await
            .unwrap()
    );
    runtimes
        .publish_world_snapshot(
            preview.id,
            worker_id,
            initial.generation,
            initial.lease_token,
            &snapshot_key,
            snapshot_bytes.len() as i64,
            &snapshot_digest,
        )
        .await
        .unwrap();

    // Artifact: consume exactly one authorized bounded chunk, fence the
    // generation, and prove the remainder is not released.
    let artifact_uri = format!(
        "/internal/worker/control/ci/runtimes/{}/artifacts/{}/{}",
        preview.id, initial.generation, initial.lease_token
    );
    let response = worker_get(&service, &artifact_uri, &worker_token, worker_lease).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let first = timeout(Duration::from_secs(2), body.next())
        .await
        .expect("artifact first chunk timed out")
        .expect("artifact stream ended before first chunk")
        .expect("artifact first chunk failed");
    assert_eq!(first.len(), 8 * 1024 * 1024);
    let revoke_runtimes = runtimes.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let revoke = tokio::spawn(async move {
        let _ = started_tx.send(());
        revoke_runtimes
            .mark_start_failed(
                preview.id,
                worker_id,
                initial.generation,
                initial.lease_token,
            )
            .await
            .unwrap()
    });
    started_rx.await.unwrap();
    wait_until_download_fence_blocks_writer(&pool).await;
    assert_revoked_after_current_chunk(&mut body, async { revoke.await.unwrap() }).await;

    // Reassignment rotates generation/lease and a fresh artifact stream can
    // complete with its declared digest and size intact.
    let reassigned = match runtimes.claim_start(preview.id, &[8_u8; 32]).await.unwrap() {
        RuntimeLease::Claimed(placement) => placement,
        lease => panic!("expected reassigned CI placement, got {lease:?}"),
    };
    assert_eq!(reassigned.worker_id, worker_id);
    assert!(reassigned.generation > initial.generation);
    let reassigned_artifact_uri = format!(
        "/internal/worker/control/ci/runtimes/{}/artifacts/{}/{}",
        preview.id, reassigned.generation, reassigned.lease_token
    );
    let response = worker_get(
        &service,
        &reassigned_artifact_uri,
        &worker_token,
        worker_lease,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), STREAM_BYTES + 1)
            .await
            .unwrap(),
        artifact_bytes
    );

    // World snapshots use the same generation-fenced stream path rather than
    // the old one-shot Body::from_stream response.
    let response = worker_get(
        &service,
        &format!(
            "/internal/worker/control/ci/runtimes/{}/world-snapshot/{}/{}",
            preview.id, reassigned.generation, reassigned.lease_token
        ),
        &worker_token,
        worker_lease,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let first = timeout(Duration::from_secs(2), body.next())
        .await
        .expect("snapshot first chunk timed out")
        .expect("snapshot stream ended before first chunk")
        .expect("snapshot first chunk failed");
    assert_eq!(first.len(), 8 * 1024 * 1024);
    let revoke_runtimes = runtimes.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let revoke = tokio::spawn(async move {
        let _ = started_tx.send(());
        revoke_runtimes
            .mark_start_failed(
                preview.id,
                worker_id,
                reassigned.generation,
                reassigned.lease_token,
            )
            .await
            .unwrap()
    });
    started_rx.await.unwrap();
    wait_until_download_fence_blocks_writer(&pool).await;
    assert_revoked_after_current_chunk(&mut body, async { revoke.await.unwrap() }).await;

    let final_placement = match runtimes.claim_start(preview.id, &[9_u8; 32]).await.unwrap() {
        RuntimeLease::Claimed(placement) => placement,
        lease => panic!("expected final CI placement, got {lease:?}"),
    };
    let response = worker_get(
        &service,
        &format!(
            "/internal/worker/control/ci/runtimes/{}/world-snapshot/{}/{}",
            preview.id, final_placement.generation, final_placement.lease_token
        ),
        &worker_token,
        worker_lease,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), STREAM_BYTES + 1)
            .await
            .unwrap(),
        snapshot_bytes
    );

    let response = worker_get(
        &service,
        &format!(
            "/internal/worker/control/ci/runtimes/{}/artifacts/{}/{}",
            preview.id, final_placement.generation, final_placement.lease_token
        ),
        &worker_token,
        worker_lease,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let first = timeout(Duration::from_secs(2), body.next())
        .await
        .expect("credential-revocation artifact chunk timed out")
        .expect("credential-revocation stream ended before its first chunk")
        .expect("credential-revocation first chunk failed");
    assert_eq!(first.len(), 8 * 1024 * 1024);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let revoke_pool = pool.clone();
    let revoke = tokio::spawn(async move {
        let _ = started_tx.send(());
        sqlx::query("UPDATE worker_credentials SET revoked_at=now() WHERE worker_id=$1")
            .bind(worker_id)
            .execute(&revoke_pool)
            .await
            .unwrap()
            .rows_affected()
            == 1
    });
    started_rx.await.unwrap();
    wait_until_download_fence_blocks_writer(&pool).await;
    assert_revoked_after_current_chunk(&mut body, async { revoke.await.unwrap() }).await;

    assert!(
        runtimes
            .mark_start_failed(
                preview.id,
                worker_id,
                final_placement.generation,
                final_placement.lease_token,
            )
            .await
            .unwrap()
    );
    sqlx::query("DELETE FROM worker_credentials WHERE worker_id=$1")
        .bind(worker_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_world_snapshot_gc WHERE object_key LIKE $1")
        .bind(format!("ci-previews/{}/worlds/%", preview.id))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_world_snapshots WHERE preview_id=$1")
        .bind(preview.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_runtime_placements WHERE preview_id=$1")
        .bind(preview.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_preview_aliases WHERE id=$1")
        .bind(preview.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_artifacts WHERE digest=$1")
        .bind(artifact_digest)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_builds WHERE id=$1")
        .bind(build.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_watched_repositories WHERE id=$1")
        .bind(watched.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers WHERE id=$1")
        .bind(worker_id)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

async fn worker_get(
    service: &axum::Router,
    uri: &str,
    token: &str,
    lease: Uuid,
) -> axum::response::Response {
    service
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .header("x-worker-lease", lease.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}
