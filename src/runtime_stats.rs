//! Runtime resource snapshots shared by workers and the server-scoped runtime API.
//!
//! Resource values are deliberately optional: an unavailable cgroup/controller
//! must remain unavailable rather than being replaced with a host-wide guess.
//! `measured_at` is assigned by the control plane when a sample is accepted;
//! worker-provided timestamps are never authoritative.

use std::time::Duration;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Maximum age for a runtime sample to be exposed by the control plane.
///
/// Worker leases and placement heartbeats expire after thirty seconds, so a
/// sample older than the same interval cannot safely describe the current
/// runtime assignment.
pub const RUNTIME_STATS_FRESHNESS_SECONDS: i64 = 30;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStats {
    /// The placement generation that produced this sample.
    pub generation: i64,
    pub cpu_percent: Option<f64>,
    pub cpu_limit_millis: Option<u64>,
    pub memory_used_bytes: Option<u64>,
    pub memory_limit_bytes: Option<u64>,
    pub disk_used_bytes: Option<u64>,
    pub disk_limit_bytes: Option<u64>,
    pub tps: Option<f64>,
    /// Assigned by the control plane when the sample is persisted.
    pub measured_at: DateTime<Utc>,
}

impl RuntimeStats {
    /// Validate invariants that cannot be represented by the JSON types alone.
    ///
    /// This is intentionally shared by worker and control-plane code so that a
    /// malformed sample cannot enter persistence merely because it came from an
    /// authenticated worker.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.generation <= 0 {
            return Err("runtime generation must be positive");
        }
        if self
            .cpu_percent
            .is_some_and(|value| !value.is_finite() || !(0.0..=100.0).contains(&value))
        {
            return Err("cpu percentage must be finite and between 0 and 100");
        }
        if self.cpu_limit_millis.is_some_and(|value| value == 0) {
            return Err("cpu limit must be positive when present");
        }
        if self
            .tps
            .is_some_and(|value| !value.is_finite() || !(0.0..=20.0).contains(&value))
        {
            return Err("TPS must be finite and between 0 and 20");
        }
        Ok(())
    }

    /// Validate that the sample belongs to the request's placement generation.
    pub fn validate_generation(&self, generation: i64) -> Result<(), &'static str> {
        self.validate()?;
        if self.generation != generation {
            return Err("runtime stats generation does not match the request generation");
        }
        Ok(())
    }
}

/// Parse a cgroup-v2 `cpu.max` value into an effective CPU quota in millis.
pub fn parse_v2_cpu_max(value: &str) -> Option<u64> {
    let mut fields = value.split_whitespace();
    let quota = fields.next()?;
    let period = fields.next()?.parse::<u64>().ok()?;
    if quota == "max" || period == 0 {
        return None;
    }
    let quota = quota.parse::<u64>().ok()?;
    Some(quota.saturating_mul(1_000).checked_div(period)?.max(1))
}

/// Parse cgroup-v1 `cpu.cfs_quota_us` and `cpu.cfs_period_us` values.
pub fn parse_v1_cpu_quota(quota: &str, period: &str) -> Option<u64> {
    let quota = quota.trim().parse::<i64>().ok()?;
    let period = period.trim().parse::<u64>().ok()?;
    if quota <= 0 || period == 0 {
        return None;
    }
    Some(
        (quota as u64)
            .saturating_mul(1_000)
            .checked_div(period)?
            .max(1),
    )
}

/// Parse cgroup-v2 `memory.current` and `memory.max` values.
pub fn parse_v2_memory(current: &str, limit: &str) -> (Option<u64>, Option<u64>) {
    (parse_bytes(current), parse_limit(limit))
}

/// Parse cgroup-v1 `memory.usage_in_bytes` and `memory.limit_in_bytes` values.
pub fn parse_v1_memory(current: &str, limit: &str) -> (Option<u64>, Option<u64>) {
    (parse_bytes(current), parse_limit(limit))
}

fn parse_bytes(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok()
}

fn parse_limit(value: &str) -> Option<u64> {
    let value = value.trim();
    let parsed = value.parse::<u64>().ok()?;
    // Linux uses a very large sentinel for an unlimited v1 memory limit.
    (parsed < (1_u64 << 60)).then_some(parsed)
}

/// Normalize a cgroup CPU usage delta to 0..=100 percent of its allocation.
pub fn normalize_cpu_percent(
    previous_usage_nanos: u64,
    current_usage_nanos: u64,
    elapsed: Duration,
    cpu_limit_millis: Option<u64>,
) -> Option<f64> {
    let delta = current_usage_nanos.checked_sub(previous_usage_nanos)?;
    let elapsed_nanos = elapsed.as_nanos();
    let limit_millis = cpu_limit_millis?;
    if elapsed_nanos == 0 || limit_millis == 0 {
        return None;
    }
    let allocation = limit_millis as f64 / 1_000.0;
    let used_cpus = delta as f64 / elapsed_nanos as f64;
    Some((used_cpus / allocation * 100.0).clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RuntimeStats {
        RuntimeStats {
            generation: 1,
            cpu_percent: Some(50.0),
            cpu_limit_millis: Some(500),
            memory_used_bytes: Some(10),
            memory_limit_bytes: Some(20),
            disk_used_bytes: Some(10),
            disk_limit_bytes: Some(20),
            tps: Some(20.0),
            measured_at: Utc::now(),
        }
    }

    #[test]
    fn validates_generation_and_metric_ranges() {
        assert!(sample().validate_generation(1).is_ok());
        assert!(sample().validate_generation(2).is_err());

        let mut invalid = sample();
        invalid.generation = 0;
        assert!(invalid.validate().is_err());
        invalid = sample();
        invalid.cpu_percent = Some(101.0);
        assert!(invalid.validate().is_err());
        invalid = sample();
        invalid.tps = Some(-0.1);
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn parses_cgroup_v2_quota_and_unlimited_values() {
        assert_eq!(parse_v2_cpu_max("50000 100000\n"), Some(500));
        assert_eq!(parse_v2_cpu_max("100000 100000"), Some(1000));
        assert_eq!(parse_v2_cpu_max("max 100000"), None);
        assert_eq!(parse_v2_cpu_max("50000 0"), None);
    }

    #[test]
    fn parses_cgroup_v1_quota_and_memory_limits() {
        assert_eq!(parse_v1_cpu_quota("50000", "100000"), Some(500));
        assert_eq!(parse_v1_cpu_quota("-1", "100000"), None);
        assert_eq!(parse_v1_cpu_quota("50000", "0"), None);
        assert_eq!(parse_v1_memory("123", "456"), (Some(123), Some(456)));
        assert_eq!(
            parse_v1_memory("123", "9223372036854771712"),
            (Some(123), None)
        );
    }

    #[test]
    fn parses_v2_memory_max_sentinel() {
        assert_eq!(parse_v2_memory("2048", "max"), (Some(2048), None));
        assert_eq!(parse_v2_memory("bad", "1024"), (None, Some(1024)));
    }

    #[test]
    fn cpu_is_normalized_to_the_effective_quota_and_clamped() {
        let one_second = Duration::from_secs(1);
        assert_eq!(
            normalize_cpu_percent(0, 500_000_000, one_second, Some(500)),
            Some(100.0)
        );
        assert_eq!(
            normalize_cpu_percent(0, 1_000_000_000, one_second, Some(500)),
            Some(100.0)
        );
        assert_eq!(
            normalize_cpu_percent(0, 250_000_000, one_second, Some(500)),
            Some(50.0)
        );
        assert_eq!(normalize_cpu_percent(10, 9, one_second, Some(500)), None);
        assert_eq!(normalize_cpu_percent(0, 1, Duration::ZERO, Some(500)), None);
        assert_eq!(normalize_cpu_percent(0, 1, one_second, None), None);
    }
}
