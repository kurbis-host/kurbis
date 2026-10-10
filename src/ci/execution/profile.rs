//! Measured resource profiles for CI builders.
//!
//! A profile is the single contract shared by admission, the remote helper,
//! and the pipeline.  Keeping the values together prevents a timeout or
//! concurrency setting from silently outliving the memory/workspace budget.

use std::time::Duration;

use crate::ci::SandboxLimits;

/// The fixed tmpfs inode budget provisioned by the Bubblewrap operators.
pub const BUILD_WORKSPACE_INODES: u64 = 1_310_720;
/// The fixed tmpfs size provisioned by the Bubblewrap operators.
pub const BUILD_WORKSPACE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const MIN_MEMORY_BYTES: u64 = 512 * 1024 * 1024;
const MIN_WORKSPACE_BYTES: u64 = 3 * 1024 * 1024 * 1024;
const MIN_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_TIMEOUT: Duration = Duration::from_secs(90 * 60);
const MAX_CONCURRENCY: u16 = 64;
const SYSTEM_RESERVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// One measured build contract.  The same profile is sent to the remote
/// runner, used for cross-process admission, and used for pipeline limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildResourceProfile {
    pub limits: SandboxLimits,
    pub workspace_inodes: u64,
    pub concurrency: u16,
}

impl BuildResourceProfile {
    pub const fn default_rootless() -> Self {
        Self {
            limits: SandboxLimits {
                memory_bytes: 10 * 1024 * 1024 * 1024,
                cpu_millis: 2_000,
                pid_limit: 512,
                disk_bytes: BUILD_WORKSPACE_BYTES,
                process_timeout: Duration::from_secs(90 * 60),
            },
            workspace_inodes: BUILD_WORKSPACE_INODES,
            concurrency: 2,
        }
    }

    /// Validate arithmetic and the immutable workspace contract without
    /// pretending environment variables are host capability evidence.
    pub fn validate(self) -> Result<(), String> {
        let limits = self.limits;
        if limits.memory_bytes < MIN_MEMORY_BYTES {
            return Err(format!(
                "builder memory must be at least {} MiB",
                MIN_MEMORY_BYTES / 1024 / 1024
            ));
        }
        if limits.disk_bytes < MIN_WORKSPACE_BYTES {
            return Err("builder workspace is too small for source, target, and artifacts".into());
        }
        if limits.disk_bytes != BUILD_WORKSPACE_BYTES {
            return Err(format!(
                "builder workspace must be exactly {BUILD_WORKSPACE_BYTES} bytes"
            ));
        }
        if self.workspace_inodes != BUILD_WORKSPACE_INODES {
            return Err(format!(
                "builder workspace inode limit must be exactly {BUILD_WORKSPACE_INODES}"
            ));
        }
        if self.concurrency == 0 || self.concurrency > MAX_CONCURRENCY {
            return Err(format!(
                "builder concurrency must be between 1 and {MAX_CONCURRENCY}"
            ));
        }
        if limits.process_timeout < MIN_TIMEOUT || limits.process_timeout > MAX_TIMEOUT {
            return Err("builder timeout must be between 60s and 90m".into());
        }
        limits
            .memory_bytes
            .checked_mul(u64::from(self.concurrency))
            .and_then(|value| value.checked_add(SYSTEM_RESERVE_BYTES))
            .ok_or_else(|| "builder memory/concurrency budget overflows".to_owned())?;
        if limits.cpu_millis == 0 || limits.pid_limit == 0 {
            return Err("builder CPU and PID limits must be non-zero".into());
        }
        Ok(())
    }

    /// Validate a profile against a live host memory sample.  This is called
    /// during remote registration so an impossible configuration cannot make
    /// CI appear ready and then wait forever for its first build.
    pub fn validate_measured_capacity(
        self,
        available_bytes: u64,
        total_bytes: u64,
    ) -> Result<(), String> {
        self.validate()?;
        if total_bytes == 0 || available_bytes == 0 || available_bytes > total_bytes {
            return Err("remote builder reported invalid memory capacity".into());
        }
        let required = self
            .limits
            .memory_bytes
            .checked_mul(u64::from(self.concurrency))
            .and_then(|value| value.checked_add(SYSTEM_RESERVE_BYTES))
            .ok_or_else(|| "builder memory/concurrency budget overflows".to_owned())?;
        if available_bytes < required {
            return Err(format!(
                "remote builder has {available_bytes} available bytes but the profile requires {required} bytes"
            ));
        }
        Ok(())
    }

    pub const fn memory_bytes(self) -> u64 {
        self.limits.memory_bytes
    }

    pub const fn timeout(self) -> Duration {
        self.limits.process_timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_is_a_valid_fixed_workspace_contract() {
        let profile = BuildResourceProfile::default_rootless();
        profile.validate().expect("default profile must be valid");
        assert_eq!(profile.limits.disk_bytes, BUILD_WORKSPACE_BYTES);
        assert_eq!(profile.workspace_inodes, BUILD_WORKSPACE_INODES);
    }

    #[test]
    fn profile_rejects_impossible_workspace_and_timeout_values() {
        let mut profile = BuildResourceProfile::default_rootless();
        profile.limits.disk_bytes -= 1;
        assert!(profile.validate().is_err());
        profile = BuildResourceProfile::default_rootless();
        profile.limits.process_timeout = Duration::from_secs(1);
        assert!(profile.validate().is_err());
    }

    #[test]
    fn profile_rejects_measured_memory_shortfall() {
        let profile = BuildResourceProfile::default_rootless();
        assert!(
            profile
                .validate_measured_capacity(profile.memory_bytes(), profile.memory_bytes())
                .is_err()
        );
    }
}
