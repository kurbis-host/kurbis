use super::*;
use async_trait::async_trait;
use kurbis_hoster::ci::{
    CI_RUNTIME_CPU_MILLIS, CI_RUNTIME_DISK_BYTES, CI_RUNTIME_IDLE_SECONDS, CI_RUNTIME_MEMORY_BYTES,
    CiPreviewRuntimeCoordinator, CiPreviewRuntimeHost, CiPreviewRuntimeRepository,
    CiRuntimePlacement, CiRuntimeState, RuntimeCapability,
};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct RecordingPreviewHost {
    events: Mutex<Vec<String>>,
    worlds: Mutex<HashSet<Uuid>>,
    capabilities: Mutex<Vec<String>>,
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
    let repository_name = format!("kurbis-host/Pumpkin-{}", Uuid::new_v4());
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

    let worker_id = Uuid::new_v4();
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

    let host = Arc::new(RecordingPreviewHost::default());
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
        sqlx::query_scalar("SELECT count(*) FROM ci_port_leases WHERE server_id = $1")
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
    assert_eq!(coordinator.stop_idle().await.unwrap(), 1);
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

    assert_eq!(
        coordinator.ensure_running(&hostname.value).await.unwrap(),
        kurbis_hoster::ci::CiPreviewState::Running
    );
    let restarted = runtimes.current(preview.id).await.unwrap().unwrap();
    assert_eq!(restarted.generation, 2);
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
        "SELECT extract(epoch FROM (expires_at - now())) / 3600 FROM ci_preview_aliases WHERE id=$1",
    )
    .bind(preview.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(extended_ttl_hours >= 71.9);

    sqlx::query(
        "UPDATE ci_preview_aliases SET expires_at = now() - interval '1 second' WHERE id=$1",
    )
    .bind(preview.id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(coordinator.expire(Utc::now()).await.unwrap(), 1);
    assert!(!host.worlds.lock().unwrap().contains(&preview.id));
    assert!(runtimes.current(preview.id).await.unwrap().is_none());
    let remaining_ports: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ci_port_leases WHERE preview_id=$1")
            .bind(preview.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(remaining_ports, 0);
    let (reserved_memory, reserved_disk, reserved_cpu): (i64, i64, i64) = sqlx::query_as(
        "SELECT reserved_memory_bytes,reserved_disk_bytes,reserved_cpu_millis FROM workers WHERE id=$1",
    )
    .bind(worker_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((reserved_memory, reserved_disk, reserved_cpu), (0, 0, 0));
    let events = host.events.lock().unwrap().clone();
    assert!(events.contains(&"stop:1:retain=true".into()));
    assert!(events.contains(&"stop:2:retain=false".into()));
    assert!(events.contains(&"delete:2".into()));

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
