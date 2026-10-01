use std::time::Duration;
use tokio::time::Instant;

use crate::pumpkin_control::PlayerStatus;

/// Tracks observed continuous emptiness, including sessions between polls.
pub(super) struct IdleTimer {
    revision: u64,
    empty_since: Option<Instant>,
}

impl IdleTimer {
    pub fn new(status: &PlayerStatus, now: Instant) -> Self {
        Self {
            revision: status.activity_revision,
            empty_since: (status.players == 0).then_some(now),
        }
    }

    pub fn expired(&mut self, status: &PlayerStatus, now: Instant, timeout: Duration) -> bool {
        self.observe(status, now);
        if status.players == 0 {
            now.duration_since(*self.empty_since.get_or_insert(now)) >= timeout
        } else {
            false
        }
    }

    pub fn observe(&mut self, status: &PlayerStatus, now: Instant) {
        if status.players > 0 || status.activity_revision != self.revision {
            self.revision = status.activity_revision;
            self.empty_since = (status.players == 0).then_some(now);
        }
    }

    /// A snapshot restarts Pumpkin, which resets its process-local activity
    /// counter. Rebase that counter without resetting continuous emptiness.
    pub fn after_snapshot_restart(&mut self, status: &PlayerStatus, now: Instant) {
        self.revision = status.activity_revision;
        if status.players > 0 {
            self.empty_since = None;
        } else {
            self.empty_since.get_or_insert(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brief_join_between_polls_restarts_the_full_five_minute_window() {
        let now = Instant::now();
        let timeout = Duration::from_secs(300);
        let mut status = PlayerStatus {
            players: 0,
            activity_revision: 0,
        };
        let mut timer = IdleTimer::new(&status, now);
        assert!(!timer.expired(&status, now + timeout - Duration::from_secs(1), timeout));
        // Join and leave both occurred since the previous zero-player sample.
        status.activity_revision = 2;
        assert!(!timer.expired(&status, now + timeout, timeout));
        assert!(!timer.expired(&status, now + timeout * 2 - Duration::from_secs(1), timeout));
        assert!(timer.expired(&status, now + timeout * 2, timeout));
    }

    #[test]
    fn connected_players_and_restarted_runtime_activity_reset_emptiness() {
        let now = Instant::now();
        let timeout = Duration::from_secs(300);
        let mut status = PlayerStatus {
            players: 0,
            activity_revision: 20,
        };
        let mut timer = IdleTimer::new(&status, now);
        status.players = 1;
        status.activity_revision = 21;
        assert!(!timer.expired(&status, now + timeout, timeout));
        status.players = 0;
        status.activity_revision = 22;
        assert!(!timer.expired(&status, now + timeout * 2, timeout));
        // A process restart resets the counter; that also starts a fresh window.
        status.activity_revision = 0;
        assert!(!timer.expired(&status, now + timeout * 3, timeout));
        assert!(timer.expired(&status, now + timeout * 4, timeout));
    }

    #[test]
    fn snapshot_restart_preserves_continuous_emptiness_and_rebases_activity() {
        let now = Instant::now();
        let timeout = Duration::from_secs(300);
        let mut status = PlayerStatus {
            players: 0,
            activity_revision: 10,
        };
        let mut timer = IdleTimer::new(&status, now);
        let resumed = PlayerStatus {
            players: 0,
            activity_revision: 0,
        };
        timer.after_snapshot_restart(&resumed, now + Duration::from_secs(120));
        assert!(!timer.expired(&resumed, now + timeout - Duration::from_secs(1), timeout));
        assert!(timer.expired(&resumed, now + timeout, timeout));

        status.players = 1;
        status.activity_revision = 1;
        assert!(!timer.expired(&status, now + timeout, timeout));
        status.players = 0;
        status.activity_revision = 2;
        assert!(!timer.expired(&status, now + timeout * 2, timeout));
        let resumed = PlayerStatus {
            players: 0,
            activity_revision: 0,
        };
        timer.after_snapshot_restart(&resumed, now + timeout * 2 + Duration::from_secs(120));
        assert!(!timer.expired(
            &resumed,
            now + timeout * 3 - Duration::from_secs(1),
            timeout
        ));
        assert!(timer.expired(&resumed, now + timeout * 3, timeout));
    }
}
