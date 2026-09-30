use std::time::Duration;

use tokio::time::Instant;

/// A failed periodic save/upload must not stop the worker or retry every tick.
pub(super) struct SnapshotSchedule {
    last_success: Instant,
    retry_at: Option<Instant>,
    failures: u32,
}

impl SnapshotSchedule {
    pub fn new(now: Instant) -> Self {
        Self {
            last_success: now,
            retry_at: None,
            failures: 0,
        }
    }

    pub fn due(&self, now: Instant, interval: Duration) -> bool {
        match self.retry_at {
            Some(retry_at) => now >= retry_at,
            None => now.duration_since(self.last_success) >= interval,
        }
    }

    pub fn succeeded(&mut self, now: Instant) {
        self.last_success = now;
        self.retry_at = None;
        self.failures = 0;
    }

    pub fn failed(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        let seconds = 5_u64
            .saturating_mul(1 << self.failures.saturating_sub(1).min(4))
            .min(60);
        self.retry_at = Some(now + Duration::from_secs(seconds));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outages_back_off_independently_then_resume_the_normal_interval() {
        let start = Instant::now();
        let interval = Duration::from_secs(300);
        let mut failed = SnapshotSchedule::new(start);
        let healthy = SnapshotSchedule::new(start);
        assert!(!failed.due(start + interval - Duration::from_secs(1), interval));
        let mut now = start + interval;
        assert!(failed.due(now, interval));
        for delay in [5, 10, 20, 40, 60, 60] {
            failed.failed(now);
            assert!(!failed.due(now + Duration::from_secs(delay - 1), interval));
            now += Duration::from_secs(delay);
            assert!(failed.due(now, interval));
            assert!(healthy.due(now, interval));
        }
        failed.succeeded(now);
        assert!(!failed.due(now + interval - Duration::from_secs(1), interval));
        assert!(failed.due(now + interval, interval));
        failed.failed(now + interval);
        assert!(failed.due(now + interval + Duration::from_secs(5), interval));
    }
}
