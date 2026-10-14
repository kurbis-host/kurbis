use super::*;
use async_trait::async_trait;
use axum::{
    body::to_bytes,
    http::{Method, Request, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use kurbis_hoster::ci::{
    CI_RUNTIME_CPU_MILLIS, CI_RUNTIME_DISK_BYTES, CI_RUNTIME_IDLE_SECONDS, CI_RUNTIME_MEMORY_BYTES,
    CiPreviewRuntimeCoordinator, CiPreviewRuntimeHost, CiPreviewRuntimeRepository,
    CiRuntimePlacement, CiRuntimeState, RuntimeCapability,
};
use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path as StorePath};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;

struct RecordingPreviewHost {
    events: Mutex<Vec<String>>,
    worlds: Mutex<HashSet<Uuid>>,
    capabilities: Mutex<Vec<String>>,
    runtimes: CiPreviewRuntimeRepository,
}

impl RecordingPreviewHost {
    fn new(runtimes: CiPreviewRuntimeRepository) -> Self {
        Self {
            events: Mutex::default(),
            worlds: Mutex::default(),
            capabilities: Mutex::default(),
            runtimes,
        }
    }
}

#[async_trait]
impl CiPreviewRuntimeHost for RecordingPreviewHost {
    async fn verify_sandbox(&self) -> anyhow::Result<()> {
        self.events.lock().unwrap().push("verify".into());
        Ok(())
    }

    async fn start(
        &self,
        placement: &CiRuntimePlacement,
        capability: &RuntimeCapability,
    ) -> anyhow::Result<()> {
        assert_eq!(capability.preview_id(), placement.preview_id);
        assert_eq!(capability.generation(), placement.generation);
        if !placement.world_initialized {
            let bytes = b"recording-host-bootstrap-checkpoint";
            self.runtimes
                .mark_world_initialized(
                    placement.preview_id,
                    placement.worker_id,
                    placement.generation,
                    placement.lease_token,
                )
                .await?;
            self.runtimes
                .publish_world_snapshot(
                    placement.preview_id,
                    placement.worker_id,
                    placement.generation,
                    placement.lease_token,
                    &format!(
                        "ci-previews/{}/worlds/recording-host-{}.tar.gz",
                        placement.preview_id,
                        Uuid::new_v4()
                    ),
                    bytes.len() as i64,
                    &blake3::hash(bytes).to_hex(),
                )
                .await?;
        }
        self.worlds.lock().unwrap().insert(placement.preview_id);
        self.capabilities
            .lock()
            .unwrap()
            .push(capability.expose_to_trusted_worker().to_owned());
        self.events
            .lock()
            .unwrap()
            .push(format!("start:{}", placement.generation));
        Ok(())
    }

    async fn stop(&self, placement: &CiRuntimePlacement, retain_world: bool) -> anyhow::Result<()> {
        self.events.lock().unwrap().push(format!(
            "stop:{}:retain={retain_world}",
            placement.generation
        ));
        Ok(())
    }

    async fn delete_world(&self, placement: &CiRuntimePlacement) -> anyhow::Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(format!("delete:{}", placement.generation));
        self.worlds.lock().unwrap().remove(&placement.preview_id);
        Ok(())
    }
}

#[tokio::test]
async fn runtime_coordinator_retains_world_on_idle_restarts_generation_and_deletes_on_expiry() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let previews = CiPreviewRepository::new(pool.clone());
    let runtimes = CiPreviewRuntimeRepository::new(pool.clone());
    let repository_name = format!("fixture/Pumpkin-{}", Uuid::new_v4());
    let watched = previews
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let key = BuildKey::new(
        &repository_name,
        "0123456789abcdef0123456789abcdef01234567",
        "patch-runtime-lifecycle",
        "rust-1.98@sha256:fixture",
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
    let digest = format!("sha256:{}", Uuid::new_v4().simple());
    previews
        .attach_artifact(build.id, &digest, "ci/runtime-lifecycle.tar", 128)
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
            &repository_name,
            &key.commit_sha,
            CiRefKind::Commit,
            None,
            &hostname,
            build.id,
        )
        .await
        .unwrap();

    sqlx::query("UPDATE workers SET draining=true WHERE supports_ci_previews")
        .execute(&pool)
        .await
        .unwrap();
    let worker_id = Uuid::new_v4();
    let worker_token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let worker_lease = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workers
            (id,name,address,signal_address,total_memory_bytes,total_disk_bytes,total_cpu_millis,
             heartbeat_at,draining,port_min,port_max,runtime_architecture,supports_ci_previews)
         VALUES($1,$2,'198.51.100.91','10.0.0.91',4294967296,21474836480,4000,
             now()+interval '5 minutes',false,28100,28120,'x86_64-linux',true)",
    )
    .bind(worker_id)
    .bind(format!("ci-runtime-lifecycle-{}", Uuid::new_v4()))
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

    let host = Arc::new(RecordingPreviewHost::new(runtimes.clone()));
    let coordinator =
        CiPreviewRuntimeCoordinator::new(previews.clone(), runtimes.clone(), host.clone());
    assert_eq!(
        coordinator.ensure_running(&hostname.value).await.unwrap(),
        kurbis_hoster::ci::CiPreviewState::Running
    );
    let first = runtimes.current(preview.id).await.unwrap().unwrap();
    assert_eq!(first.generation, 1);
    assert!(first.active);
    assert_eq!(first.state, CiRuntimeState::Running);
    let first_capability = host.capabilities.lock().unwrap()[0].clone();
    assert!(
        runtimes
            .capability_valid(preview.id, first.generation, &first_capability)
            .await
            .unwrap()
    );
    let port_lease_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM port_leases WHERE server_id = $1")
            .bind(preview.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let ci_port_lease_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ci_port_leases WHERE preview_id = $1")
            .bind(preview.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(port_lease_count, 0, "CI ports use their own lease table");
    assert_eq!(ci_port_lease_count, 3);

    let object_store = Arc::new(InMemory::new());
    let service = kurbis_hoster::control_plane::api::router(common::api_state(
        pool.clone(),
        "snapshot-api-internal-secret",
        object_store.clone(),
    ));
    let public_snapshot = Request::builder()
        .method(Method::POST)
        .uri(format!("/internal/ci/previews/{}/commands", preview.id))
        .header("authorization", format!("Bearer {first_capability}"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"command":"snapshot","generation":first.generation,"reason":"manual"})
                .to_string(),
        ))
        .unwrap();
    let public_snapshot_response = service.clone().oneshot(public_snapshot).await.unwrap();
    assert_eq!(public_snapshot_response.status(), StatusCode::OK);
    let command_state: serde_json::Value = serde_json::from_slice(
        &to_bytes(public_snapshot_response.into_body(), 4096)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(command_state["observed_state"], "running");
    assert!(
        runtimes
            .world_snapshot_requested(preview.id, worker_id, first.generation, first.lease_token)
            .await
            .unwrap()
    );

    // Ask the worker to checkpoint an online world. It saves through the normal
    // assignment-fenced control route and publishes only to this preview's object namespace.
    assert!(runtimes.request_world_snapshot(preview.id).await.unwrap());
    let first_snapshot_uri = format!(
        "/internal/worker/control/ci/runtimes/{}/world-snapshot/{}/{}",
        preview.id, first.generation, first.lease_token
    );
    let request_uri = format!(
        "/internal/worker/control/ci/runtimes/{}/world-snapshot-requested/{}/{}",
        preview.id, first.generation, first.lease_token
    );
    let request_state = worker_request(
        service.clone(),
        Method::GET,
        &request_uri,
        &worker_token,
        worker_lease,
        Body::empty(),
    )
    .await;
    assert_eq!(request_state.status(), StatusCode::OK);
    let state_json: serde_json::Value =
        serde_json::from_slice(&to_bytes(request_state.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(state_json["requested"], true);
    let snapshot_bytes = Bytes::from_static(b"verified-world-snapshot-for-worker-restore");
    let upload = Request::builder()
        .method(Method::PUT)
        .uri(&first_snapshot_uri)
        .header("authorization", format!("Bearer {worker_token}"))
        .header("x-worker-lease", worker_lease.to_string())
        .header("content-type", "application/gzip")
        .header("content-length", snapshot_bytes.len())
        .body(Body::from(snapshot_bytes.clone()))
        .unwrap();
    let upload_response = service.clone().oneshot(upload).await.unwrap();
    assert_eq!(upload_response.status(), StatusCode::NO_CONTENT);
    let first_snapshot: kurbis_hoster::ci::runtime::CiWorldSnapshot = sqlx::query_as(
        "SELECT object_key,size_bytes,checksum,source_generation FROM ci_world_snapshots WHERE preview_id=$1",
    ).bind(preview.id).fetch_one(&pool).await.unwrap();
    assert_eq!(first_snapshot.size_bytes, snapshot_bytes.len() as i64);
    assert_eq!(first_snapshot.source_generation, first.generation);
    assert_eq!(
        first_snapshot.checksum,
        blake3::hash(&snapshot_bytes).to_hex().to_string()
    );
    let complete_uri = format!(
        "/internal/worker/control/ci/runtimes/{}/world-snapshot-complete",
        preview.id
    );
    let complete = Request::builder()
        .method(Method::POST)
        .uri(&complete_uri)
        .header("authorization", format!("Bearer {worker_token}"))
        .header("x-worker-lease", worker_lease.to_string())
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "generation": first.generation,
                "lease_token": first.lease_token,
            })
            .to_string(),
        ))
        .unwrap();
    assert_eq!(
        service.clone().oneshot(complete).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    sqlx::query(
        "UPDATE ci_world_snapshots SET created_at=now()-interval '11 minutes' WHERE preview_id=$1",
    )
    .bind(preview.id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(runtimes.request_due_world_snapshots(16).await.unwrap(), 1);
    assert!(
        runtimes
            .world_snapshot_requested(preview.id, worker_id, first.generation, first.lease_token)
            .await
            .unwrap()
    );
    let periodic_snapshot_bytes = Bytes::from_static(b"periodic-worker-world-checkpoint");
    let periodic_upload = Request::builder()
        .method(Method::PUT)
        .uri(&first_snapshot_uri)
        .header("authorization", format!("Bearer {worker_token}"))
        .header("x-worker-lease", worker_lease.to_string())
        .header("content-type", "application/gzip")
        .header("content-length", periodic_snapshot_bytes.len())
        .body(Body::from(periodic_snapshot_bytes.clone()))
        .unwrap();
    assert_eq!(
        service
            .clone()
            .oneshot(periodic_upload)
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    let periodic_snapshot: kurbis_hoster::ci::runtime::CiWorldSnapshot = sqlx::query_as(
        "SELECT object_key,size_bytes,checksum,source_generation FROM ci_world_snapshots WHERE preview_id=$1",
    )
    .bind(preview.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(periodic_snapshot.source_generation, first.generation);
    assert_eq!(
        periodic_snapshot.size_bytes,
        periodic_snapshot_bytes.len() as i64
    );
    assert_eq!(
        periodic_snapshot.checksum,
        blake3::hash(&periodic_snapshot_bytes).to_hex().to_string()
    );
    assert!(
        object_store
            .get(&StorePath::parse(&first_snapshot.object_key).unwrap())
            .await
            .is_ok(),
        "the old checkpoint remains available until durable GC acknowledges deletion"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM ci_world_snapshot_gc WHERE object_key=$1"
        )
        .bind(&first_snapshot.object_key)
        .fetch_one(&pool)
        .await
        .unwrap(),
        1,
        "replacement and GC enqueue commit atomically"
    );
    let abandoned_gc_claim = runtimes
        .claim_world_snapshot_garbage(16)
        .await
        .unwrap()
        .into_iter()
        .find(|claim| claim.object_key == first_snapshot.object_key)
        .expect("superseded snapshot is available for durable GC");
    object_store
        .delete(&StorePath::parse(&abandoned_gc_claim.object_key).unwrap())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE ci_world_snapshot_gc SET claimed_at=now()-interval '11 minutes' WHERE object_key=$1",
    )
    .bind(&abandoned_gc_claim.object_key)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        kurbis_hoster::control_plane::maintenance::cleanup_ci_world_snapshot_garbage(
            &runtimes,
            object_store.clone(),
            16,
        )
        .await
        .unwrap(),
        1
    );
    assert!(
        object_store
            .get(&StorePath::parse(&first_snapshot.object_key).unwrap())
            .await
            .is_err(),
        "durable GC deletes the superseded checkpoint"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM ci_world_snapshot_gc WHERE object_key=$1"
        )
        .bind(&first_snapshot.object_key)
        .fetch_one(&pool)
        .await
        .unwrap(),
        0,
        "GC releases its durable claim after an idempotent delete"
    );
    assert!(
        object_store
            .get(&StorePath::parse(&periodic_snapshot.object_key).unwrap())
            .await
            .is_ok(),
        "GC never deletes the current last-known-good checkpoint"
    );
    assert_eq!(
        kurbis_hoster::control_plane::maintenance::cleanup_ci_world_snapshot_garbage(
            &runtimes,
            object_store.clone(),
            16,
        )
        .await
        .unwrap(),
        0,
        "a completed cleanup is idempotent"
    );
    let periodic_complete = Request::builder()
        .method(Method::POST)
        .uri(&complete_uri)
        .header("authorization", format!("Bearer {worker_token}"))
        .header("x-worker-lease", worker_lease.to_string())
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "generation": first.generation,
                "lease_token": first.lease_token,
            })
            .to_string(),
        ))
        .unwrap();
    assert_eq!(
        service
            .clone()
            .oneshot(periodic_complete)
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );

    sqlx::query(
        "UPDATE ci_preview_aliases SET expires_at = now() + interval '1 hour' WHERE id = $1",
    )
    .bind(preview.id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE ci_runtime_placements SET last_activity_at = now() - interval '10 minutes'
         WHERE preview_id = $1",
    )
    .bind(preview.id)
    .execute(&pool)
    .await
    .unwrap();
    let mut idle_claims = runtimes
        .claim_idle(chrono::Duration::seconds(CI_RUNTIME_IDLE_SECONDS), 16)
        .await
        .unwrap();
    assert_eq!(idle_claims.len(), 1);
    let idle_stopping = idle_claims.pop().unwrap();
    assert_eq!(idle_stopping.state, CiRuntimeState::Stopping);
    assert!(idle_stopping.active);
    host.stop(&idle_stopping, true).await.unwrap();
    let idle_snapshot_bytes =
        Bytes::from_static(b"checkpoint-published-before-idle-capacity-release");
    let idle_snapshot_uri = format!(
        "/internal/worker/control/ci/runtimes/{}/world-snapshot/{}/{}",
        preview.id, idle_stopping.generation, idle_stopping.lease_token
    );
    let idle_upload = Request::builder()
        .method(Method::PUT)
        .uri(&idle_snapshot_uri)
        .header("authorization", format!("Bearer {worker_token}"))
        .header("x-worker-lease", worker_lease.to_string())
        .header("content-length", idle_snapshot_bytes.len())
        .body(Body::from(idle_snapshot_bytes.clone()))
        .unwrap();
    assert_eq!(
        service.clone().oneshot(idle_upload).await.unwrap().status(),
        StatusCode::NO_CONTENT,
        "the authenticated Router accepts the final snapshot while the idle stop is stopping+active"
    );
    let idle_stopped = worker_json_request(
        service.clone(),
        &format!(
            "/internal/worker/control/ci/runtimes/{}/stopped",
            preview.id
        ),
        &worker_token,
        worker_lease,
        serde_json::json!({
            "generation": idle_stopping.generation,
            "lease_token": idle_stopping.lease_token,
        }),
    )
    .await;
    assert_eq!(idle_stopped.status(), StatusCode::NO_CONTENT);
    assert!(host.worlds.lock().unwrap().contains(&preview.id));
    let stopped = runtimes.current(preview.id).await.unwrap().unwrap();
    assert!(!stopped.active);
    assert_eq!(stopped.state, CiRuntimeState::Stopped);
    let alias_state: String =
        sqlx::query_scalar("SELECT state FROM ci_preview_aliases WHERE id = $1")
            .bind(preview.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(alias_state, "ready");
    let leases_after_idle: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ci_port_leases WHERE preview_id = $1")
            .bind(preview.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(leases_after_idle, 0);
    let (reserved_memory, reserved_disk, reserved_cpu): (i64, i64, i64) = sqlx::query_as(
        "SELECT reserved_memory_bytes,reserved_disk_bytes,reserved_cpu_millis FROM workers WHERE id=$1",
    )
    .bind(worker_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(reserved_memory, 0);
    assert_eq!(reserved_cpu, 0);
    assert_eq!(reserved_disk, CI_RUNTIME_DISK_BYTES);

    // Concurrent admission has one generation-fenced claimant; all other
    // callers observe that same starting lease rather than allocating ports.
    let (claim_a, claim_b) = tokio::join!(
        runtimes.claim_start(preview.id, &[4_u8; 32]),
        runtimes.claim_start(preview.id, &[5_u8; 32]),
    );
    let (claim_a, claim_b) = (claim_a.unwrap(), claim_b.unwrap());
    let starting = match (&claim_a, &claim_b) {
        (
            kurbis_hoster::ci::RuntimeLease::Claimed(placement),
            kurbis_hoster::ci::RuntimeLease::Starting(_),
        )
        | (
            kurbis_hoster::ci::RuntimeLease::Starting(_),
            kurbis_hoster::ci::RuntimeLease::Claimed(placement),
        ) => placement.clone(),
        pair => panic!("expected exactly one CI runtime claimant, got {pair:?}"),
    };
    assert_eq!(starting.generation, 2);
    let ci_port_lease_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ci_port_leases WHERE preview_id=$1")
            .bind(preview.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(ci_port_lease_count, 3);

    let snapshot_uri = format!(
        "/internal/worker/control/ci/runtimes/{}/world-snapshot/{}/{}",
        preview.id, starting.generation, starting.lease_token
    );
    let response = worker_request(
        service.clone(),
        Method::GET,
        &snapshot_uri,
        &worker_token,
        worker_lease,
        Body::empty(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap(),
        idle_snapshot_bytes
    );
    let foreign_worker = Uuid::new_v4();
    let foreign_token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let foreign_lease = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workers(id,name,address,signal_address,total_memory_bytes,total_disk_bytes,total_cpu_millis,
             heartbeat_at,draining,port_min,port_max,runtime_architecture,supports_ci_previews)
         VALUES($1,$2,'198.51.100.92','10.0.0.92',4294967296,21474836480,4000,
             now()+interval '5 minutes',false,28200,28220,'x86_64-linux',true)",
    )
    .bind(foreign_worker)
    .bind(format!("ci-runtime-foreign-{}", Uuid::new_v4()))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO worker_credentials(worker_id,api_token_hash,lease_id,lease_expires_at)
         VALUES($1,$2,$3,now()+interval '1 hour')",
    )
    .bind(foreign_worker)
    .bind(blake3::hash(foreign_token.as_bytes()).as_bytes().to_vec())
    .bind(foreign_lease)
    .execute(&pool)
    .await
    .unwrap();
    let foreign_response = worker_request(
        service.clone(),
        Method::GET,
        &snapshot_uri,
        &foreign_token,
        foreign_lease,
        Body::empty(),
    )
    .await;
    if foreign_response.status() != StatusCode::CONFLICT {
        let (parts, body) = foreign_response.into_parts();
        let body = to_bytes(body, 4096).await.unwrap();
        panic!(
            "foreign snapshot status {}: {}",
            parts.status,
            String::from_utf8_lossy(&body)
        );
    }
    let stale_uri = format!(
        "/internal/worker/control/ci/runtimes/{}/world-snapshot/1/{}",
        preview.id, first.lease_token
    );
    let stale_response = worker_request(
        service.clone(),
        Method::GET,
        &stale_uri,
        &worker_token,
        worker_lease,
        Body::empty(),
    )
    .await;
    assert_eq!(stale_response.status(), StatusCode::CONFLICT);
    let started_uri = format!(
        "/internal/worker/control/ci/runtimes/{}/started",
        preview.id
    );
    let stale_started = worker_json_request(
        service.clone(),
        &started_uri,
        &worker_token,
        worker_lease,
        serde_json::json!({"generation": first.generation, "lease_token": first.lease_token}),
    )
    .await;
    assert_eq!(stale_started.status(), StatusCode::CONFLICT);
    let foreign_started = worker_json_request(
        service.clone(),
        &started_uri,
        &foreign_token,
        foreign_lease,
        serde_json::json!({"generation": starting.generation, "lease_token": starting.lease_token}),
    )
    .await;
    assert_eq!(foreign_started.status(), StatusCode::CONFLICT);
    let wrong_worker_capability = worker_json_request(
        service.clone(),
        &format!("/internal/worker/control/ci/runtimes/{}/capability", preview.id),
        &foreign_token,
        foreign_lease,
        serde_json::json!({"generation": starting.generation, "lease_token": starting.lease_token, "capability": URL_SAFE_NO_PAD.encode(rand::random::<[u8;32]>())}),
    )
    .await;
    assert_eq!(wrong_worker_capability.status(), StatusCode::CONFLICT);
    assert!(
        runtimes
            .mark_start_failed(
                starting.preview_id,
                starting.worker_id,
                starting.generation,
                starting.lease_token,
            )
            .await
            .unwrap()
    );

    assert_eq!(
        coordinator.ensure_running(&hostname.value).await.unwrap(),
        kurbis_hoster::ci::CiPreviewState::Running
    );
    let restarted = runtimes.current(preview.id).await.unwrap().unwrap();
    assert_eq!(restarted.generation, 3);
    assert_eq!(restarted.worker_id, worker_id);
    assert!(restarted.active);
    assert!(host.worlds.lock().unwrap().contains(&preview.id));
    let capabilities = host.capabilities.lock().unwrap().clone();
    assert_eq!(capabilities.len(), 2);
    assert_ne!(capabilities[0], capabilities[1]);
    assert!(
        !runtimes
            .capability_valid(preview.id, restarted.generation, &capabilities[0])
            .await
            .unwrap()
    );
    assert!(
        runtimes
            .capability_valid(preview.id, restarted.generation, &capabilities[1])
            .await
            .unwrap()
    );
    let extended_ttl_hours: f64 = sqlx::query_scalar(
        "SELECT (extract(epoch FROM (expires_at - now())) / 3600)::float8 FROM ci_preview_aliases WHERE id=$1",
    )
    .bind(preview.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(extended_ttl_hours >= BUILD_TTL_HOURS as f64 - 0.1);
    let initial_worker = worker_id;
    let replacement_worker = foreign_worker;
    sqlx::query("UPDATE workers SET heartbeat_at=now()-interval '10 minutes' WHERE id=$1")
        .bind(initial_worker)
        .execute(&pool)
        .await
        .unwrap();
    // Exercise the real admission Router against a dead worker heartbeat. A
    // running alias must not be reported as live when its worker is stale; the
    // authenticated admission path reassigns only because the shared snapshot
    // exists and rotates the generation/lease.
    let stale_admission = Request::builder()
        .method(Method::POST)
        .uri("/internal/ci/previews/admit")
        .header("authorization", "Bearer snapshot-api-internal-secret")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"hostname": hostname.value}).to_string(),
        ))
        .unwrap();
    let stale_admission_response = service.clone().oneshot(stale_admission).await.unwrap();
    assert_eq!(stale_admission_response.status(), StatusCode::ACCEPTED);
    let stale_admission_payload: serde_json::Value = serde_json::from_slice(
        &to_bytes(stale_admission_response.into_body(), 4096)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(stale_admission_payload["state"], "booting");
    let transferred = runtimes.current(preview.id).await.unwrap().unwrap();
    assert_eq!(transferred.worker_id, replacement_worker);
    assert_eq!(transferred.generation, 4);
    assert_eq!(transferred.state, CiRuntimeState::Starting);
    assert!(!transferred.active);
    let transferred_snapshot_uri = format!(
        "/internal/worker/control/ci/runtimes/{}/world-snapshot/{}/{}",
        preview.id, transferred.generation, transferred.lease_token
    );
    let transferred_download = worker_request(
        service.clone(),
        Method::GET,
        &transferred_snapshot_uri,
        &foreign_token,
        foreign_lease,
        Body::empty(),
    )
    .await;
    if transferred_download.status() != StatusCode::OK {
        let (parts, body) = transferred_download.into_parts();
        panic!(
            "transferred snapshot status {}: {}",
            parts.status,
            String::from_utf8_lossy(&to_bytes(body, 4096).await.unwrap())
        );
    }
    assert_eq!(
        to_bytes(transferred_download.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap(),
        idle_snapshot_bytes,
        "the new assigned worker can restore only the authenticated persisted world snapshot"
    );
    let old_worker_download = worker_request(
        service.clone(),
        Method::GET,
        &transferred_snapshot_uri,
        &worker_token,
        worker_lease,
        Body::empty(),
    )
    .await;
    assert_eq!(old_worker_download.status(), StatusCode::CONFLICT);
    // A worker can disappear after receiving a Starting assignment but before
    // startup completes. Direct heartbeat reconciliation must rehome it without
    // accepting a stale started callback.
    sqlx::query("UPDATE workers SET heartbeat_at=now()+interval '5 minutes' WHERE id=$1")
        .bind(initial_worker)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET heartbeat_at=now()-interval '10 minutes' WHERE id=$1")
        .bind(replacement_worker)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(runtimes.reconcile_stale_workers(16).await.unwrap(), 1);
    let recovered_starting = runtimes.current(preview.id).await.unwrap().unwrap();
    assert_eq!(recovered_starting.worker_id, initial_worker);
    assert_eq!(recovered_starting.generation, 5);
    assert_eq!(recovered_starting.state, CiRuntimeState::Starting);
    let starting_snapshot_uri = format!(
        "/internal/worker/control/ci/runtimes/{}/world-snapshot/{}/{}",
        preview.id, recovered_starting.generation, recovered_starting.lease_token
    );
    let starting_snapshot = worker_request(
        service.clone(),
        Method::GET,
        &starting_snapshot_uri,
        if initial_worker == worker_id {
            &worker_token
        } else {
            &foreign_token
        },
        if initial_worker == worker_id {
            worker_lease
        } else {
            foreign_lease
        },
        Body::empty(),
    )
    .await;
    assert_eq!(starting_snapshot.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(starting_snapshot.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap(),
        idle_snapshot_bytes
    );
    assert!(
        !runtimes
            .mark_running(
                preview.id,
                foreign_worker,
                transferred.generation,
                transferred.lease_token,
            )
            .await
            .unwrap(),
        "the recovered worker's predecessor is fenced from a delayed started acknowledgement"
    );
    assert!(
        runtimes
            .mark_running(
                preview.id,
                initial_worker,
                recovered_starting.generation,
                recovered_starting.lease_token,
            )
            .await
            .unwrap()
    );

    // Heartbeat expiry while Running also rehomes through the regular stale
    // reconciler, never a synthetic runtime_lost callback.
    sqlx::query("UPDATE workers SET heartbeat_at=now()-interval '10 minutes' WHERE id=$1")
        .bind(initial_worker)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET heartbeat_at=now()+interval '5 minutes' WHERE id=$1")
        .bind(replacement_worker)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(runtimes.reconcile_stale_workers(16).await.unwrap(), 1);
    let recovered_running_loss = runtimes.current(preview.id).await.unwrap().unwrap();
    assert_eq!(recovered_running_loss.worker_id, foreign_worker);
    assert_eq!(recovered_running_loss.generation, 6);
    assert_eq!(recovered_running_loss.state, CiRuntimeState::Starting);
    assert!(
        runtimes
            .mark_running(
                preview.id,
                foreign_worker,
                recovered_running_loss.generation,
                recovered_running_loss.lease_token,
            )
            .await
            .unwrap()
    );

    // If a worker disappears during idle Stopping, use the last complete
    // snapshot and rotate the lease instead of leaving the preview wedged.
    assert!(runtimes.request_stop(preview.id).await.unwrap());
    let stopping = runtimes.current(preview.id).await.unwrap().unwrap();
    assert_eq!(stopping.state, CiRuntimeState::Stopping);
    assert!(stopping.active);
    sqlx::query("UPDATE workers SET heartbeat_at=now()-interval '10 minutes' WHERE id=$1")
        .bind(foreign_worker)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET heartbeat_at=now()+interval '5 minutes' WHERE id=$1")
        .bind(worker_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(runtimes.reconcile_stale_workers(16).await.unwrap(), 1);
    let recovered_stopping = runtimes.current(preview.id).await.unwrap().unwrap();
    assert_eq!(recovered_stopping.worker_id, initial_worker);
    assert_eq!(recovered_stopping.generation, 7);
    assert_eq!(recovered_stopping.state, CiRuntimeState::Starting);
    let stopping_snapshot_uri = format!(
        "/internal/worker/control/ci/runtimes/{}/world-snapshot/{}/{}",
        preview.id, recovered_stopping.generation, recovered_stopping.lease_token
    );
    let stopping_snapshot = worker_request(
        service.clone(),
        Method::GET,
        &stopping_snapshot_uri,
        if initial_worker == worker_id {
            &worker_token
        } else {
            &foreign_token
        },
        if initial_worker == worker_id {
            worker_lease
        } else {
            foreign_lease
        },
        Body::empty(),
    )
    .await;
    assert_eq!(stopping_snapshot.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(stopping_snapshot.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap(),
        idle_snapshot_bytes
    );

    // TTL expiry with the replacement worker offline is finalized by the
    // control plane: object/checkpoint, alias, ports, and reservations are
    // removed without an impossible acknowledgement from the dead worker.
    let now = Utc::now() + chrono::Duration::seconds(1);
    sqlx::query("UPDATE ci_preview_aliases SET expires_at = $2 - interval '1 second' WHERE id=$1")
        .bind(preview.id)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
    previews.expire(now).await.unwrap();
    let expired = runtimes.claim_expired(now, 16).await.unwrap();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].placement.worker_id, initial_worker);
    assert_eq!(expired[0].placement.generation, 7);
    assert_eq!(
        expired[0].placement.lease_token,
        recovered_stopping.lease_token
    );
    sqlx::query("UPDATE workers SET heartbeat_at=now()-interval '10 minutes' WHERE id=$1")
        .bind(worker_id)
        .execute(&pool)
        .await
        .unwrap();
    let stale_ttl_claim = runtimes.claim_stale_expired(16).await.unwrap();
    assert_eq!(stale_ttl_claim.len(), 1);
    assert_eq!(stale_ttl_claim[0].placement.preview_id, preview.id);
    let stale_ttl_delete_ack = worker_json_request(
        service.clone(),
        &format!(
            "/internal/worker/control/ci/runtimes/{}/deleted",
            preview.id
        ),
        &worker_token,
        worker_lease,
        serde_json::json!({
            "generation": recovered_stopping.generation,
            "lease_token": recovered_stopping.lease_token,
        }),
    )
    .await;
    assert_eq!(
        stale_ttl_delete_ack.status(),
        StatusCode::CONFLICT,
        "the stale-GC claim fences the offline worker's delayed TTL deletion acknowledgement"
    );
    kurbis_hoster::control_plane::maintenance::finish_claimed_stale_ci_preview_deletion(
        &runtimes,
        object_store.clone(),
        &stale_ttl_claim[0],
    )
    .await
    .unwrap();
    assert!(
        runtimes.current(preview.id).await.unwrap().is_none(),
        "offline TTL cleanup removes the placement without waiting for worker acknowledgement"
    );
    assert!(
        previews
            .lookup_preview_by_id(preview.id)
            .await
            .unwrap()
            .is_none(),
        "offline TTL cleanup removes the expired alias"
    );
    assert!(
        object_store
            .get(&StorePath::parse(&first_snapshot.object_key).unwrap())
            .await
            .is_err(),
        "offline TTL cleanup deletes the shared world checkpoint"
    );
    let remaining_ports: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ci_port_leases WHERE preview_id=$1")
            .bind(preview.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(remaining_ports, 0);
    for assigned_worker in [worker_id, foreign_worker] {
        let (reserved_memory, reserved_disk, reserved_cpu): (i64, i64, i64) = sqlx::query_as(
            "SELECT reserved_memory_bytes,reserved_disk_bytes,reserved_cpu_millis FROM workers WHERE id=$1",
        )
        .bind(assigned_worker)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((reserved_memory, reserved_disk, reserved_cpu), (0, 0, 0));
    }
    let events = host.events.lock().unwrap().clone();
    assert!(events.contains(&"stop:1:retain=true".into()));
    assert!(events.contains(&"start:3".into()));
    assert!(
        !events.iter().any(|event| event == "stop:7:retain=false"),
        "stale TTL reclamation is performed by the control plane, not an offline worker callback"
    );

    sqlx::query("DELETE FROM worker_credentials WHERE worker_id=$1")
        .bind(foreign_worker)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers WHERE id=$1")
        .bind(foreign_worker)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM worker_credentials WHERE worker_id=$1")
        .bind(worker_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_artifacts WHERE digest=$1")
        .bind(digest)
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

    assert_eq!(CI_RUNTIME_IDLE_SECONDS, 300);
    assert_eq!(CI_RUNTIME_MEMORY_BYTES, 1024 * 1024 * 1024);
    assert_eq!(CI_RUNTIME_CPU_MILLIS, 1000);
}

async fn worker_request(
    service: axum::Router,
    method: Method,
    uri: &str,
    token: &str,
    lease_id: Uuid,
    body: Body,
) -> axum::response::Response {
    service
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .header("x-worker-lease", lease_id.to_string())
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn worker_json_request(
    service: axum::Router,
    uri: &str,
    token: &str,
    lease_id: Uuid,
    body: serde_json::Value,
) -> axum::response::Response {
    service
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .header("x-worker-lease", lease_id.to_string())
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn stale_first_generation_may_cold_start_and_partial_bootstrap_rehomes_but_initialized_world_without_checkpoint_stays_fenced()
 {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let previews = CiPreviewRepository::new(pool.clone());
    let runtimes = CiPreviewRuntimeRepository::new(pool.clone());
    let repository_name = format!("fixture/Pumpkin-cold-start-{}", Uuid::new_v4());
    let watched = previews
        .register_repository(&repository_name, "main")
        .await
        .unwrap();
    let key = BuildKey::new(
        &repository_name,
        "0123456789abcdef0123456789abcdef01234567",
        "patch-cold-start",
        "rust-1.98@sha256:cold-start-fixture",
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
    let digest = format!("sha256:{}", Uuid::new_v4().simple());
    previews
        .attach_artifact(build.id, &digest, "ci/cold-start.tar", 32)
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
            &repository_name,
            &key.commit_sha,
            CiRefKind::Commit,
            None,
            &hostname,
            build.id,
        )
        .await
        .unwrap();

    let worker_one = Uuid::new_v4();
    let worker_two = Uuid::new_v4();
    for (worker, port_min, suffix) in [(worker_one, 28300, 1), (worker_two, 28400, 2)] {
        sqlx::query(
            "INSERT INTO workers(id,name,address,signal_address,total_memory_bytes,total_disk_bytes,total_cpu_millis,
                 heartbeat_at,draining,port_min,port_max,runtime_architecture,supports_ci_previews)
             VALUES($1,$2,$3,$4,4294967296,21474836480,4000,
                 now()+interval '5 minutes',false,$5,$6,'x86_64-linux',true)",
        )
        .bind(worker)
        .bind(format!("ci-cold-start-{suffix}-{}", Uuid::new_v4()))
        .bind(format!("198.51.100.{}", 90 + suffix))
        .bind(format!("10.0.0.{}", 90 + suffix))
        .bind(port_min)
        .bind(port_min + 20)
        .execute(&pool)
        .await
        .unwrap();
    }

    let worker_one_token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let worker_one_lease = Uuid::new_v4();
    let worker_two_token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let worker_two_lease = Uuid::new_v4();
    for (worker, token, lease) in [
        (worker_one, &worker_one_token, worker_one_lease),
        (worker_two, &worker_two_token, worker_two_lease),
    ] {
        sqlx::query(
            "INSERT INTO worker_credentials(worker_id,api_token_hash,lease_id,lease_expires_at)
             VALUES($1,$2,$3,now()+interval '1 hour')",
        )
        .bind(worker)
        .bind(blake3::hash(token.as_bytes()).as_bytes().to_vec())
        .bind(lease)
        .execute(&pool)
        .await
        .unwrap();
    }
    let bootstrap_store = Arc::new(InMemory::new());
    let bootstrap_service = kurbis_hoster::control_plane::api::router(common::api_state(
        pool.clone(),
        "snapshot-api-internal-secret",
        bootstrap_store.clone(),
    ));

    let initial_lease = runtimes
        .claim_start(preview.id, &[10_u8; 32])
        .await
        .unwrap();
    let kurbis_hoster::ci::RuntimeLease::Claimed(initial) = initial_lease else {
        panic!("first worker should claim the initial empty CI runtime");
    };
    assert!([worker_one, worker_two].contains(&initial.worker_id));
    let initial_worker = initial.worker_id;
    let replacement_worker = if initial_worker == worker_one {
        worker_two
    } else {
        worker_one
    };
    assert_eq!(initial.generation, 1);
    assert!(!initial.world_initialized);

    // If the worker dies before Pumpkin has ever run, the generation-1 empty
    // world is explicitly safe to cold-start elsewhere; leases still rotate.
    sqlx::query("UPDATE workers SET heartbeat_at=now()-interval '10 minutes' WHERE id=$1")
        .bind(initial_worker)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(runtimes.reconcile_stale_workers(16).await.unwrap(), 1);
    let replacement = runtimes.current(preview.id).await.unwrap().unwrap();
    assert_eq!(replacement.worker_id, replacement_worker);
    assert_eq!(replacement.generation, 2);
    assert_eq!(replacement.state, CiRuntimeState::Starting);
    assert!(!replacement.world_initialized);

    // A stop request can race the first worker's readiness boundary. The
    // world remains explicitly uninitialized, so a stale Stopping placement
    // may still cold-start elsewhere without treating it as user data.
    sqlx::query(
        "UPDATE ci_runtime_placements SET state='stopping',active=true WHERE preview_id=$1",
    )
    .bind(preview.id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE ci_preview_aliases SET state='stopping' WHERE id=$1")
        .bind(preview.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET heartbeat_at=now()-interval '10 minutes' WHERE id=$1")
        .bind(replacement_worker)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET heartbeat_at=now()+interval '5 minutes' WHERE id=$1")
        .bind(initial_worker)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(runtimes.reconcile_stale_workers(16).await.unwrap(), 1);
    let replacement = runtimes.current(preview.id).await.unwrap().unwrap();
    assert_eq!(replacement.worker_id, initial_worker);
    assert_eq!(replacement.generation, 3);
    assert_eq!(replacement.state, CiRuntimeState::Starting);
    assert!(!replacement.world_initialized);
    let bootstrap_worker = replacement.worker_id;
    let bootstrap_worker_token = if bootstrap_worker == worker_one {
        &worker_one_token
    } else {
        &worker_two_token
    };
    let bootstrap_worker_lease = if bootstrap_worker == worker_one {
        worker_one_lease
    } else {
        worker_two_lease
    };

    assert!(
        !runtimes
            .current(preview.id)
            .await
            .unwrap()
            .unwrap()
            .world_initialized
    );
    let initialized = worker_json_request(
        bootstrap_service.clone(),
        &format!(
            "/internal/worker/control/ci/runtimes/{}/world-initialized",
            preview.id
        ),
        bootstrap_worker_token,
        bootstrap_worker_lease,
        serde_json::json!({
            "generation": replacement.generation,
            "lease_token": replacement.lease_token,
        }),
    )
    .await;
    assert_eq!(initialized.status(), StatusCode::NO_CONTENT);
    let after_barrier = runtimes.current(preview.id).await.unwrap().unwrap();
    assert!(!after_barrier.world_initialized);
    let bootstrap_pending: bool = sqlx::query_scalar(
        "SELECT bootstrap_pending FROM ci_runtime_placements WHERE preview_id=$1",
    )
    .bind(preview.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(bootstrap_pending);

    // A partial, never-visible bootstrap is safe to discard on the next player
    // join. Maintenance does not auto-start it, but an available worker may
    // receive a fresh generation and cold-start without losing player data.
    sqlx::query("UPDATE workers SET heartbeat_at=now()-interval '10 minutes' WHERE id=$1")
        .bind(bootstrap_worker)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET heartbeat_at=now()+interval '5 minutes' WHERE id=$1")
        .bind(replacement_worker)
        .execute(&pool)
        .await
        .unwrap();
    let bootstrap_incomplete_recovery = runtimes
        .claim_start(preview.id, &[12_u8; 32])
        .await
        .unwrap();
    let kurbis_hoster::ci::RuntimeLease::Claimed(recovered) = bootstrap_incomplete_recovery else {
        panic!("a player join should safely rehome a never-visible partial bootstrap");
    };
    assert_eq!(recovered.worker_id, replacement_worker);
    assert_eq!(recovered.generation, replacement.generation + 1);
    assert!(!recovered.world_initialized);
    let replacement = recovered;
    let bootstrap_worker = replacement.worker_id;
    let bootstrap_worker_token = if bootstrap_worker == worker_one {
        &worker_one_token
    } else {
        &worker_two_token
    };
    let bootstrap_worker_lease = if bootstrap_worker == worker_one {
        worker_one_lease
    } else {
        worker_two_lease
    };
    let recovered_initialized = worker_json_request(
        bootstrap_service.clone(),
        &format!(
            "/internal/worker/control/ci/runtimes/{}/world-initialized",
            preview.id
        ),
        bootstrap_worker_token,
        bootstrap_worker_lease,
        serde_json::json!({
            "generation": replacement.generation,
            "lease_token": replacement.lease_token,
        }),
    )
    .await;
    assert_eq!(recovered_initialized.status(), StatusCode::NO_CONTENT);

    // The first ready Pumpkin generation must establish a shared bootstrap
    // point-in-time snapshot before the coordinator marks the runtime Running.
    let bootstrap_bytes = Bytes::from_static(b"bootstrap-world-files-saved-at-quiescent-boundary");
    let bootstrap_upload = Request::builder()
        .method(Method::PUT)
        .uri(format!(
            "/internal/worker/control/ci/runtimes/{}/world-snapshot/{}/{}",
            preview.id, replacement.generation, replacement.lease_token
        ))
        .header("authorization", format!("Bearer {bootstrap_worker_token}"))
        .header("x-worker-lease", bootstrap_worker_lease.to_string())
        .header("content-length", bootstrap_bytes.len())
        .body(Body::from(bootstrap_bytes.clone()))
        .unwrap();
    assert_eq!(
        bootstrap_service
            .clone()
            .oneshot(bootstrap_upload)
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    let bootstrap_snapshot = runtimes.current(preview.id).await.unwrap().unwrap();
    assert!(bootstrap_snapshot.world_initialized);
    let bootstrap_metadata: kurbis_hoster::ci::runtime::CiWorldSnapshot = sqlx::query_as("SELECT object_key,size_bytes,checksum,source_generation FROM ci_world_snapshots WHERE preview_id=$1")
        .bind(preview.id).fetch_one(&pool).await.unwrap();
    assert_eq!(bootstrap_metadata.size_bytes, bootstrap_bytes.len() as i64);
    assert_eq!(
        bootstrap_metadata.checksum,
        blake3::hash(&bootstrap_bytes).to_hex().to_string()
    );
    assert_eq!(bootstrap_metadata.source_generation, replacement.generation);
    assert!(
        runtimes
            .mark_running(
                preview.id,
                bootstrap_worker,
                replacement.generation,
                replacement.lease_token
            )
            .await
            .unwrap()
    );
    let started = runtimes.current(preview.id).await.unwrap().unwrap();
    assert!(started.world_initialized);

    // Remove the durable pointer to simulate a crash before any complete
    // checkpoint could be retained. An initialized world must never migrate as
    // a fresh empty directory, even though another worker has free capacity.
    sqlx::query("DELETE FROM ci_world_snapshots WHERE preview_id=$1")
        .bind(preview.id)
        .execute(&pool)
        .await
        .unwrap();
    bootstrap_store
        .delete(&StorePath::parse(&bootstrap_metadata.object_key).unwrap())
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET heartbeat_at=now()-interval '10 minutes' WHERE id=$1")
        .bind(bootstrap_worker)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET heartbeat_at=now()+interval '5 minutes' WHERE id=$1")
        .bind(initial_worker)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(runtimes.reconcile_stale_workers(16).await.unwrap(), 0);
    let blocked = runtimes
        .claim_start(preview.id, &[11_u8; 32])
        .await
        .unwrap();
    assert_eq!(blocked, kurbis_hoster::ci::RuntimeLease::NoWorker);
    assert_eq!(
        runtimes.current(preview.id).await.unwrap().unwrap(),
        started
    );
    let (ports, worker_two_disk, worker_two_memory): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*),
                (SELECT reserved_disk_bytes FROM workers WHERE id=$2),
                (SELECT reserved_memory_bytes FROM workers WHERE id=$2)
         FROM ci_port_leases WHERE preview_id=$1",
    )
    .bind(preview.id)
    .bind(initial_worker)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(ports, 3);
    assert_eq!(worker_two_disk, 0);
    assert_eq!(worker_two_memory, 0);

    let now = Utc::now() + chrono::Duration::seconds(1);
    sqlx::query("UPDATE ci_preview_aliases SET expires_at=$2-interval '1 second' WHERE id=$1")
        .bind(preview.id)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
    previews.expire(now).await.unwrap();
    let expiry = runtimes.claim_expired(now, 16).await.unwrap();
    assert_eq!(expiry.len(), 1);
    assert_eq!(expiry[0].placement.worker_id, bootstrap_worker);
    sqlx::query("UPDATE workers SET heartbeat_at=now()-interval '10 minutes' WHERE id=$1")
        .bind(bootstrap_worker)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        kurbis_hoster::control_plane::maintenance::cleanup_stale_ci_preview_deletions(
            &runtimes,
            bootstrap_store.clone(),
            16,
        )
        .await
        .unwrap(),
        1,
    );
    sqlx::query("DELETE FROM worker_credentials WHERE worker_id=ANY($1)")
        .bind(vec![worker_one, worker_two])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_artifacts WHERE digest=$1")
        .bind(digest)
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
    sqlx::query("DELETE FROM workers WHERE id=ANY($1)")
        .bind(vec![worker_one, worker_two])
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}
