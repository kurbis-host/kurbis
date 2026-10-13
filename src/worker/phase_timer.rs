use std::{
    fmt,
    time::{Duration, Instant},
};

/// Wall-clock time spent in each named step of one worker operation.
///
/// A server start is a chain of waits on Docker, the control plane and
/// Pumpkin. Logging the split with every start shows which link got slow
/// without reproducing the start under a profiler.
pub struct PhaseTimer {
    started: Instant,
    last: Instant,
    phases: Vec<(&'static str, Duration)>,
}

impl PhaseTimer {
    pub fn start() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            last: now,
            phases: Vec::new(),
        }
    }

    /// Close the step that began at the previous mark (or at the start).
    pub fn mark(&mut self, phase: &'static str) {
        let now = Instant::now();
        self.phases.push((phase, now - self.last));
        self.last = now;
    }

    pub fn total(&self) -> Duration {
        self.started.elapsed()
    }
}

impl fmt::Display for PhaseTimer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, (phase, elapsed)) in self.phases.iter().enumerate() {
            if index > 0 {
                formatter.write_str(" ")?;
            }
            write!(formatter, "{phase}={}ms", elapsed.as_millis())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_are_reported_in_order_and_sum_to_the_total() {
        let mut timer = PhaseTimer::start();
        std::thread::sleep(Duration::from_millis(5));
        timer.mark("first");
        timer.mark("second");

        let rendered = timer.to_string();
        let (first, second) = rendered.split_once(' ').expect("two phases");
        assert!(first.starts_with("first=") && first.ends_with("ms"));
        assert!(second.starts_with("second=") && second.ends_with("ms"));
        let measured: Duration = timer.phases.iter().map(|(_, elapsed)| *elapsed).sum();
        assert!(measured >= Duration::from_millis(5));
        assert!(measured <= timer.total());
    }
}
