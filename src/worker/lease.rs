use std::time::Duration;

use anyhow::{Context, Result, bail};
use sqlx::{Connection, PgPool, Postgres, pool::PoolConnection};
use tokio::time::{interval, timeout};
use uuid::Uuid;

/// An exclusive worker identity tied to one live Postgres session.
pub struct WorkerLease {
    connection: PoolConnection<Postgres>,
}

impl WorkerLease {
    pub async fn acquire(pool: &PgPool, worker_id: Uuid) -> Result<Self> {
        let mut connection = pool.acquire().await?;
        // Session locks must never return to the reusable connection pool,
        // including when worker initialization or recovery fails.
        connection.close_on_drop();
        let acquired: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1, 0))")
                .bind(format!("kurbis-worker:{worker_id}"))
                .fetch_one(&mut *connection)
                .await?;
        if !acquired {
            bail!("another worker process already owns WORKER_ID {worker_id}");
        }
        Ok(Self { connection })
    }

    pub async fn backend_pid(&mut self) -> Result<i32> {
        Ok(sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *self.connection)
            .await?)
    }

    /// Loss of this connection means loss of the exclusive worker identity.
    /// The runner supervises this future alongside heartbeat and runtime work.
    pub async fn monitor(mut self) -> Result<()> {
        let mut timer = interval(Duration::from_secs(5));
        loop {
            timer.tick().await;
            timeout(Duration::from_secs(5), self.connection.ping())
                .await
                .context("worker lease connection check timed out")?
                .context("worker identity lease was lost")?;
        }
    }
}
