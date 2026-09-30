mod common;

use common::test_pool;
use kurbis_hoster::worker::lease::WorkerLease;
use std::time::Duration;
use uuid::Uuid;

#[tokio::test]
async fn worker_identity_is_exclusive_and_released_after_drop_and_cancellation() {
    let pool = test_pool().await;
    let worker = Uuid::new_v4();
    let lease = WorkerLease::acquire(&pool, worker).await.unwrap();
    assert!(WorkerLease::acquire(&pool, worker).await.is_err());
    drop(lease);
    let lease = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(lease) = WorkerLease::acquire(&pool, worker).await {
                break lease;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let monitor = tokio::spawn(lease.monitor());
    assert!(WorkerLease::acquire(&pool, worker).await.is_err());
    monitor.abort();
    assert!(monitor.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(lease) = WorkerLease::acquire(&pool, worker).await {
                drop(lease);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn terminated_lease_session_stops_monitor_before_replacement_worker_takes_over() {
    let pool = test_pool().await;
    let worker = Uuid::new_v4();
    let mut lease = WorkerLease::acquire(&pool, worker).await.unwrap();
    let pid = lease.backend_pid().await.unwrap();
    let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(pid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(terminated);
    let error = tokio::time::timeout(Duration::from_secs(10), lease.monitor())
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("worker identity lease was lost"));
    let replacement = WorkerLease::acquire(&pool, worker).await.unwrap();
    assert!(WorkerLease::acquire(&pool, worker).await.is_err());
    drop(replacement);
}
