//! Wakes waiting workers the moment an operation is committed for them.
//!
//! Workers used to find new work by polling every two seconds, so a player's
//! join waited a second on average before anything started. The enqueueing
//! transaction now announces itself with NOTIFY, which PostgreSQL delivers at
//! commit, and workers hold a long poll that returns on that announcement.

use anyhow::Result;
use sqlx::{Postgres, Transaction, postgres::PgListener};
use tokio::sync::watch;

const CHANNEL: &str = "worker_operations";

#[derive(Clone)]
pub struct OperationSignal {
    queued: watch::Sender<u64>,
}

impl Default for OperationSignal {
    fn default() -> Self {
        Self {
            queued: watch::Sender::new(0),
        }
    }
}

impl OperationSignal {
    /// Announce an operation inside the transaction that inserts it. Waiters
    /// are woken only once that transaction commits, so their claim sees it.
    pub async fn announce(tx: &mut Transaction<'_, Postgres>) -> sqlx::Result<()> {
        sqlx::query("SELECT pg_notify($1, '')")
            .bind(CHANNEL)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.queued.subscribe()
    }

    /// A connection that already hears announcements. Subscribing before
    /// forwarding lets a caller know from which point none can be missed.
    pub async fn listener(database_url: &str) -> Result<PgListener> {
        let mut listener = PgListener::connect(database_url).await?;
        listener.listen(CHANNEL).await?;
        Ok(listener)
    }

    /// Forward announcements to waiters until the database connection fails.
    pub async fn forward(&self, mut listener: PgListener) -> Result<()> {
        loop {
            // `None` means the connection was re-established and announcements
            // may have been lost in between, which also has to wake waiters.
            listener.try_recv().await?;
            self.queued
                .send_modify(|count| *count = count.wrapping_add(1));
        }
    }
}
