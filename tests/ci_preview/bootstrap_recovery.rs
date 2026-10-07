use super::*;
use kurbis_hoster::ci::{
    BuildKey, CiBuildStage, CiHostname, CiPreviewRuntimeRepository, CiRefKind, CiRuntimeState,
    RuntimeLease,
};
use sqlx::PgPool;

async fn ready_preview(pool: &PgPool, suffix: &str) -> (Uuid, Uuid, String) {
    let previews = CiPreviewRepository::new(pool.clone());
    let repository = format!("fixture/Pumpkin-{suffix}-{}", Uuid::new_v4());
    let watched = previews
        .register_repository(&repository, "main")
        .await
        .unwrap();
    let commit_sha = format!("{:040x}", rand::random::<u64>());
    let key = BuildKey::new(
        &repository,
        &commit_sha,
        "patch-bootstrap-recovery",
        "rust-1.98@sha256:bootstrap-recovery-fixture",
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
        .attach_artifact(build.id, &digest, &format!("ci/{suffix}.tar"), 64)
        .await
        .unwrap();
    previews
        .transition_build(build.id, CiBuildStage::Ready, None)
        .await
        .unwrap();
    let hostname = CiHostname::for_ref(
        CiRefKind::Commit,
        &commit_sha,
        kurbis_hoster::ci::CI_ROOT_DOMAIN,
    )
    .unwrap();
    let preview = previews
        .ensure_preview(
            &repository,
            &commit_sha,
            CiRefKind::Commit,
            None,
            &hostname,
            build.id,
        )
        .await
        .unwrap();
    (preview.id, watched.id, digest)
}

async fn create_worker(pool: &PgPool, suffix: &str, port_min: i32) -> Uuid {
    let worker_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workers(id,name,address,signal_address,total_memory_bytes,total_disk_bytes,total_cpu_millis,
             heartbeat_at,draining,port_min,port_max,runtime_architecture,supports_ci_previews)
         VALUES($1,$2,'198.51.100.181','10.0.0.181',4294967296,21474836480,4000,
             now()+interval '5 minutes',false,$3,$4,'x86_64-linux',true)",
    )
    .bind(worker_id)
    .bind(format!("ci-bootstrap-{suffix}-{}", Uuid::new_v4()))
    .bind(port_min)
    .bind(port_min + 20)
    .execute(pool)
    .await
    .unwrap();
    worker_id
}

async fn checkpoint_bootstrap(
    runtimes: &CiPreviewRuntimeRepository,
    placement: &kurbis_hoster::ci::CiRuntimePlacement,
) {
    let bytes = b"quiescent-bootstrap-world-checkpoint";
    assert!(
        runtimes
            .mark_world_initialized(
                placement.preview_id,
                placement.worker_id,
                placement.generation,
                placement.lease_token,
            )
            .await
            .unwrap()
    );
    runtimes
        .publish_world_snapshot(
            placement.preview_id,
            placement.worker_id,
            placement.generation,
            placement.lease_token,
            &format!(
                "ci-previews/{}/worlds/bootstrap-{}.tar.gz",
                placement.preview_id,
                Uuid::new_v4()
            ),
            bytes.len() as i64,
            &blake3::hash(bytes).to_hex(),
        )
        .await
        .unwrap();
}

async fn clean_fixture(
    pool: &PgPool,
    preview_id: Uuid,
    watched_id: Uuid,
    digest: &str,
    workers: &[Uuid],
) {
    sqlx::query("DELETE FROM ci_preview_aliases WHERE id=$1")
        .bind(preview_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_builds WHERE artifact_digest=$1")
        .bind(digest)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_artifacts WHERE digest=$1")
        .bind(digest)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ci_watched_repositories WHERE id=$1")
        .bind(watched_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers WHERE id=ANY($1)")
        .bind(workers)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_initial_start_retries_on_its_worker_and_only_becomes_visible_after_checkpoint() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let (preview_id, watched_id, digest) = ready_preview(&pool, "bootstrap-retry").await;
    sqlx::query("UPDATE workers SET draining=true WHERE supports_ci_previews")
        .execute(&pool)
        .await
        .unwrap();
    let worker_id = create_worker(&pool, "retry", 28500).await;
    let runtimes = CiPreviewRuntimeRepository::new(pool.clone());

    let RuntimeLease::Claimed(first) = runtimes
        .claim_start(preview_id, &[31_u8; 32])
        .await
        .unwrap()
    else {
        panic!("first join should claim a bootstrap generation");
    };
    assert!(
        runtimes
            .mark_world_initialized(preview_id, worker_id, first.generation, first.lease_token,)
            .await
            .unwrap(),
        "the worker must be able to fence its partial local bootstrap before launch"
    );
    let (initialized, bootstrap_pending): (bool, bool) = sqlx::query_as(
        "SELECT world_initialized, bootstrap_pending FROM ci_runtime_placements WHERE preview_id=$1",
    )
    .bind(preview_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        !initialized,
        "a partially created world is not a checkpoint"
    );
    assert!(bootstrap_pending);
    assert!(
        !runtimes
            .mark_running(preview_id, worker_id, first.generation, first.lease_token,)
            .await
            .unwrap(),
        "a runtime without its first checkpoint must not become routable"
    );

    assert!(
        runtimes
            .mark_start_failed(preview_id, worker_id, first.generation, first.lease_token,)
            .await
            .unwrap()
    );
    let RuntimeLease::Claimed(retry) = runtimes
        .claim_start(preview_id, &[32_u8; 32])
        .await
        .unwrap()
    else {
        panic!("a subsequent player join should retry the failed bootstrap");
    };
    assert_eq!(
        retry.worker_id, worker_id,
        "retain partial local files on a live worker"
    );
    assert_eq!(retry.generation, first.generation + 1);
    assert_ne!(retry.lease_token, first.lease_token);
    assert!(!retry.world_initialized);
    let bootstrap_pending: bool = sqlx::query_scalar(
        "SELECT bootstrap_pending FROM ci_runtime_placements WHERE preview_id=$1",
    )
    .bind(preview_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        !bootstrap_pending,
        "each generation must open its own bootstrap fence"
    );
    assert!(
        !runtimes
            .mark_world_initialized(preview_id, worker_id, first.generation, first.lease_token,)
            .await
            .unwrap(),
        "a failed generation cannot reopen its bootstrap fence"
    );

    assert!(
        !runtimes
            .mark_running(preview_id, worker_id, retry.generation, retry.lease_token,)
            .await
            .unwrap()
    );
    checkpoint_bootstrap(&runtimes, &retry).await;
    assert!(
        runtimes
            .mark_running(preview_id, worker_id, retry.generation, retry.lease_token,)
            .await
            .unwrap(),
        "the retried runtime becomes visible only after its durable baseline is published"
    );
    let running = runtimes.current(preview_id).await.unwrap().unwrap();
    assert!(running.world_initialized);
    assert_eq!(running.state, CiRuntimeState::Running);

    clean_fixture(&pool, preview_id, watched_id, &digest, &[worker_id]).await;
    pool.close().await;
}

#[tokio::test]
async fn dead_partial_bootstrap_waits_for_join_then_discards_only_never_visible_generation() {
    let _test_guard = CI_TEST_LOCK.lock().await;
    let pool = pool().await;
    let (preview_id, watched_id, digest) = ready_preview(&pool, "bootstrap-rehome").await;
    sqlx::query("UPDATE workers SET draining=true WHERE supports_ci_previews")
        .execute(&pool)
        .await
        .unwrap();
    let worker_a = create_worker(&pool, "rehome-a", 28600).await;
    let worker_b = create_worker(&pool, "rehome-b", 28700).await;
    sqlx::query("UPDATE workers SET draining=false WHERE id=ANY($1)")
        .bind(vec![worker_a, worker_b])
        .execute(&pool)
        .await
        .unwrap();
    let runtimes = CiPreviewRuntimeRepository::new(pool.clone());
    let RuntimeLease::Claimed(first) = runtimes
        .claim_start(preview_id, &[41_u8; 32])
        .await
        .unwrap()
    else {
        panic!("first join should claim a bootstrap generation");
    };
    let other_worker = if first.worker_id == worker_a {
        worker_b
    } else {
        worker_a
    };
    assert!(
        runtimes
            .mark_world_initialized(
                preview_id,
                first.worker_id,
                first.generation,
                first.lease_token,
            )
            .await
            .unwrap()
    );
    sqlx::query("UPDATE workers SET heartbeat_at=now()-interval '10 minutes' WHERE id=$1")
        .bind(first.worker_id)
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(
        runtimes.reconcile_stale_workers(16).await.unwrap(),
        0,
        "maintenance must not auto-start a never-visible partial bootstrap"
    );
    let RuntimeLease::Claimed(retry) = runtimes
        .claim_start(preview_id, &[42_u8; 32])
        .await
        .unwrap()
    else {
        panic!("the next player join should safely rehome an uncheckpointed bootstrap");
    };
    assert_eq!(retry.worker_id, other_worker);
    assert_eq!(retry.generation, first.generation + 1);
    assert_ne!(retry.lease_token, first.lease_token);
    assert!(!retry.world_initialized);
    let (initialized, bootstrap_pending, snapshots): (bool, bool, i64) = sqlx::query_as(
        "SELECT runtime.world_initialized, runtime.bootstrap_pending,
                (SELECT count(*) FROM ci_world_snapshots WHERE preview_id=$1)
         FROM ci_runtime_placements runtime WHERE runtime.preview_id=$1",
    )
    .bind(preview_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!initialized);
    assert!(!bootstrap_pending);
    assert_eq!(snapshots, 0);
    assert!(
        runtimes
            .world_snapshot_metadata(
                preview_id,
                first.worker_id,
                first.generation,
                first.lease_token,
            )
            .await
            .is_err(),
        "the discarded partial generation cannot publish or restore its files"
    );

    checkpoint_bootstrap(&runtimes, &retry).await;
    assert!(
        runtimes
            .mark_running(
                preview_id,
                retry.worker_id,
                retry.generation,
                retry.lease_token,
            )
            .await
            .unwrap()
    );
    let visible = runtimes.current(preview_id).await.unwrap().unwrap();
    assert!(visible.world_initialized);
    assert_eq!(visible.state, CiRuntimeState::Running);

    // Once a world has become visible, losing its only checkpoint is not a
    // license to replace it with an empty world.
    sqlx::query("DELETE FROM ci_world_snapshots WHERE preview_id=$1")
        .bind(preview_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET heartbeat_at=now()-interval '10 minutes' WHERE id=$1")
        .bind(retry.worker_id)
        .execute(&pool)
        .await
        .unwrap();
    let result = runtimes
        .claim_start(preview_id, &[43_u8; 32])
        .await
        .unwrap();
    assert_eq!(result, RuntimeLease::NoWorker);
    assert_eq!(
        runtimes.current(preview_id).await.unwrap().unwrap(),
        visible
    );

    clean_fixture(
        &pool,
        preview_id,
        watched_id,
        &digest,
        &[worker_a, worker_b],
    )
    .await;
    pool.close().await;
}
