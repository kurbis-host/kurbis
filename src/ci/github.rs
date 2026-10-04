//! GitHub source-control adapter.
//!
//! The implementation lives in [`super::source_control`] so orchestration only depends on
//! the `SourceControlClient` trait. This module is the stable provider-facing name.
pub use super::source_control::{GitHubSourceControl, deduplicate_refs, retry_delay};
