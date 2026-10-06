use std::future::Future;

use anyhow::{Context, Result};
use async_trait::async_trait;

/// Worker-side protocol for a point-in-time Pumpkin world checkpoint. An
/// implementation must make `begin_application_barrier` wait for game ticks
/// and leave-save writes, then freeze the whole container before archive reads.
#[async_trait]
pub(super) trait SnapshotBarrier: Send {
    async fn begin_application_barrier(&mut self) -> Result<()>;
    async fn pause_runtime_container(&mut self) -> Result<()>;
    async fn archive_and_publish(&mut self) -> Result<()>;
    async fn unpause_runtime_container(&mut self) -> Result<()>;
    async fn end_application_barrier(&mut self) -> Result<()>;
}

/// Recover the two-part quiescence protocol after a worker crashes. The
/// generation marker is removed by the release future only after the app-level
/// barrier acknowledges, so either failure leaves recovery retryable.
pub(super) async fn recover_interrupted_boundary(
    unpause_runtime: impl Future<Output = Result<()>>,
    end_application_barrier: impl Future<Output = Result<()>>,
) -> Result<()> {
    unpause_runtime
        .await
        .context("unpause runtime after interrupted snapshot")?;
    end_application_barrier
        .await
        .context("release application barrier after interrupted snapshot")?;
    Ok(())
}

/// Run every checkpoint stage in order and release the app barrier only after
/// Docker confirms the runtime has resumed. If the archive/upload fails, the
/// world resumes but the failed checkpoint is surfaced so the request retries.
pub(super) async fn publish_at_quiescent_boundary(
    boundary: &mut impl SnapshotBarrier,
) -> Result<()> {
    boundary
        .begin_application_barrier()
        .await
        .context("begin application world snapshot barrier")?;
    if let Err(pause_error) = boundary.pause_runtime_container().await {
        return match boundary.end_application_barrier().await {
            Ok(()) => Err(pause_error).context("pause runtime for world snapshot"),
            Err(release_error) => Err(pause_error).context(format!(
                "pause runtime for world snapshot; application barrier release also failed: {release_error:#}"
            )),
        };
    }

    let publish_result = boundary.archive_and_publish().await;
    if let Err(unpause_error) = boundary.unpause_runtime_container().await {
        // Do not clear the application barrier. The marker and paused container
        // let a later reconciliation recover without allowing writes meanwhile.
        return Err(unpause_error).context("unpause runtime after world snapshot");
    }
    let release_result = boundary
        .end_application_barrier()
        .await
        .context("end application world snapshot barrier");
    publish_result?;
    release_result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };
    use tempfile::TempDir;
    use tokio::task::JoinHandle;

    use super::super::world_snapshot::{pack_world, unpack_world_snapshot};

    struct FileWorldBoundary {
        directory: TempDir,
        archive_directory: TempDir,
        app_barrier: Arc<Mutex<bool>>,
        writer_stopped: Arc<AtomicBool>,
        writer: Option<JoinHandle<()>>,
        generations: Vec<u64>,
        paused: bool,
        published_pair: Option<(u64, u64)>,
    }

    impl FileWorldBoundary {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(directory.path().join("region-a.dat"), "0").unwrap();
            std::fs::write(directory.path().join("region-b.dat"), "0").unwrap();
            Self {
                directory,
                archive_directory: tempfile::tempdir().unwrap(),
                app_barrier: Arc::new(Mutex::new(false)),
                writer_stopped: Arc::new(AtomicBool::new(false)),
                writer: None,
                generations: Vec::new(),
                paused: false,
                published_pair: None,
            }
        }

        fn start_concurrent_writer(&mut self) {
            let gate = self.app_barrier.clone();
            let stopped = self.writer_stopped.clone();
            let first = self.directory.path().join("region-a.dat");
            let second = self.directory.path().join("region-b.dat");
            self.writer = Some(tokio::task::spawn_blocking(move || {
                let mut generation = 1_u64;
                while !stopped.load(Ordering::Acquire) {
                    {
                        let blocked = gate.lock().unwrap();
                        if !*blocked {
                            std::fs::write(&first, generation.to_string()).unwrap();
                            std::thread::sleep(Duration::from_millis(1));
                            std::fs::write(&second, generation.to_string()).unwrap();
                            generation += 1;
                        }
                    }
                    std::thread::yield_now();
                }
            }));
        }

        fn files(&self) -> (u64, u64) {
            let first = std::fs::read_to_string(self.directory.path().join("region-a.dat"))
                .unwrap()
                .parse()
                .unwrap();
            let second = std::fs::read_to_string(self.directory.path().join("region-b.dat"))
                .unwrap()
                .parse()
                .unwrap();
            (first, second)
        }
    }

    impl Drop for FileWorldBoundary {
        fn drop(&mut self) {
            self.writer_stopped.store(true, Ordering::Release);
            drop(self.app_barrier.lock().unwrap());
        }
    }

    #[async_trait]
    impl SnapshotBarrier for FileWorldBoundary {
        async fn begin_application_barrier(&mut self) -> Result<()> {
            *self.app_barrier.lock().unwrap() = true;
            // Synchronize with any writer already in its critical section.
            drop(self.app_barrier.lock().unwrap());
            Ok(())
        }

        async fn pause_runtime_container(&mut self) -> Result<()> {
            anyhow::ensure!(
                *self.app_barrier.lock().unwrap(),
                "runtime pause happened before application barrier"
            );
            self.paused = true;
            Ok(())
        }

        async fn archive_and_publish(&mut self) -> Result<()> {
            anyhow::ensure!(
                self.paused,
                "archive attempted while runtime was not paused"
            );
            anyhow::ensure!(
                *self.app_barrier.lock().unwrap(),
                "archive attempted after application barrier release"
            );
            let before = self.files();
            anyhow::ensure!(
                before.0 == before.1,
                "test writer entered the archive boundary"
            );
            let archive = self.archive_directory.path().join("snapshot.tar.gz");
            pack_world(self.directory.path(), &archive, 1024 * 1024)?;
            let restore = self.archive_directory.path().join("restore");
            std::fs::create_dir(&restore)?;
            unpack_world_snapshot(
                &archive,
                &restore,
                1024 * 1024,
                unsafe { libc::geteuid() },
                unsafe { libc::getegid() },
            )?;
            let first: u64 = std::fs::read_to_string(restore.join("region-a.dat"))?.parse()?;
            let second: u64 = std::fs::read_to_string(restore.join("region-b.dat"))?.parse()?;
            anyhow::ensure!(
                first == second,
                "archive contains a torn cross-file generation"
            );
            self.generations.push(first);
            self.published_pair = Some((first, second));
            std::fs::remove_dir_all(restore)?;
            std::fs::remove_file(archive)?;
            Ok(())
        }

        async fn unpause_runtime_container(&mut self) -> Result<()> {
            self.paused = false;
            Ok(())
        }

        async fn end_application_barrier(&mut self) -> Result<()> {
            *self.app_barrier.lock().unwrap() = false;
            Ok(())
        }
    }

    #[tokio::test]
    async fn crash_recovery_unpauses_before_releasing_and_clears_marker_only_after_ack() {
        let marker = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(marker.path(), b"generation-4").unwrap();
        let app_barrier = Arc::new(AtomicBool::new(true));
        let runtime_paused = Arc::new(AtomicBool::new(true));
        let events = Arc::new(Mutex::new(Vec::new()));

        let paused = runtime_paused.clone();
        let unpause_events = events.clone();
        let unpause = async move {
            assert!(paused.swap(false, Ordering::AcqRel));
            unpause_events.lock().unwrap().push("unpaused");
            Ok(())
        };
        let app = app_barrier.clone();
        let release_paused = runtime_paused.clone();
        let release_events = events.clone();
        let marker_path = marker.path().to_owned();
        let release = async move {
            assert!(!release_paused.load(Ordering::Acquire));
            assert!(app.swap(false, Ordering::AcqRel));
            release_events.lock().unwrap().push("released");
            std::fs::remove_file(marker_path)?;
            Ok(())
        };
        recover_interrupted_boundary(unpause, release)
            .await
            .unwrap();

        assert_eq!(*events.lock().unwrap(), vec!["unpaused", "released"]);
        assert!(!app_barrier.load(Ordering::Acquire));
        assert!(!runtime_paused.load(Ordering::Acquire));
        assert!(!marker.path().exists());
    }

    #[tokio::test]
    async fn failed_unpause_keeps_the_application_barrier_and_recovery_marker() {
        let marker = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(marker.path(), b"generation-4").unwrap();
        let app_barrier = Arc::new(AtomicBool::new(true));
        let release_called = Arc::new(AtomicBool::new(false));
        let release_marker = marker.path().to_owned();
        let release_app = app_barrier.clone();
        let release_flag = release_called.clone();
        let result = recover_interrupted_boundary(
            async { anyhow::bail!("docker unpause failed") },
            async move {
                release_flag.store(true, Ordering::Release);
                release_app.store(false, Ordering::Release);
                std::fs::remove_file(release_marker)?;
                Ok(())
            },
        )
        .await;

        assert!(result.is_err());
        assert!(!release_called.load(Ordering::Acquire));
        assert!(app_barrier.load(Ordering::Acquire));
        assert!(marker.path().exists());
    }

    #[tokio::test]
    async fn concurrent_world_mutations_are_quiesced_before_archive_and_resumed_afterward() {
        let mut boundary = FileWorldBoundary::new();
        boundary.start_concurrent_writer();
        tokio::time::sleep(Duration::from_millis(20)).await;
        for _ in 0..20 {
            publish_at_quiescent_boundary(&mut boundary).await.unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(boundary.generations.len(), 20);
        assert!(
            boundary
                .generations
                .iter()
                .any(|generation| *generation > 0)
        );
        assert!(
            boundary
                .published_pair
                .is_some_and(|(left, right)| left == right)
        );
        boundary.writer_stopped.store(true, Ordering::Release);
        if let Some(writer) = boundary.writer.take() {
            writer.await.unwrap();
        }
    }
}
