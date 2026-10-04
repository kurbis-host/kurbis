//! Independent runtime placement and lifecycle for untrusted CI previews.
//!
//! CI records deliberately never enter account-owned `servers` or `placements`.
//! The placement records in this module are the sole authority for preview
//! capabilities, ports, generations, world retention, and worker cleanup.

mod coordinator;
mod repository;

pub use coordinator::{
    CiPreviewRuntimeCoordinator, CiPreviewRuntimeHost, CiRuntimeError, CiRuntimeState,
    RuntimeCapability, RuntimeLease,
};
pub use repository::{
    CI_RUNTIME_CPU_MILLIS, CI_RUNTIME_DISK_BYTES, CI_RUNTIME_IDLE_SECONDS, CI_RUNTIME_MEMORY_BYTES,
    CiPreviewRuntimeRepository, CiRuntimePlacement, ExpiredCiRuntime,
};
