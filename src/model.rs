use std::net::IpAddr;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::Type;
use uuid::Uuid;

pub const SERVER_MEMORY_BYTES: i64 = 2 * 1024 * 1024 * 1024;
pub const SERVER_STORAGE_BYTES: i64 = 5 * 1024 * 1024 * 1024;
pub const SERVER_CPU_MILLIS: i64 = 1000;
pub const DEFAULT_SERVER_MOTD: &str = "Hosted by Kurbis";
pub const DEFAULT_IDLE_SECONDS: i64 = 5 * 60;
pub const SNAPSHOT_RETENTION_GENERATIONS: i64 = 5;

pub fn is_gateway_source_address(value: &str) -> bool {
    if value.trim() != value {
        return false;
    }
    match value.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => {
            !address.is_unspecified() && !address.is_broadcast() && !address.is_multicast()
        }
        Ok(IpAddr::V6(address)) => !address.is_unspecified() && !address.is_multicast(),
        Err(_) => false,
    }
}

pub fn is_private_signal_address(value: &str) -> bool {
    if value.trim() != value {
        return false;
    }
    match value.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => {
            !address.is_unspecified()
                && !address.is_broadcast()
                && !address.is_multicast()
                && (address.is_private() || address.is_loopback())
        }
        Ok(IpAddr::V6(address)) => {
            !address.is_unspecified()
                && !address.is_multicast()
                && (address.is_unique_local() || address.is_loopback())
        }
        Err(_) => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, JsonSchema)]
#[sqlx(type_name = "server_desired_state", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DesiredState {
    Running,
    Stopped,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, JsonSchema)]
#[sqlx(type_name = "server_observed_state", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ObservedState {
    Sleeping,
    Queued,
    Restoring,
    Starting,
    Online,
    Stopping,
    Snapshotting,
    Resetting,
    Failed,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, JsonSchema)]
#[sqlx(type_name = "operation_kind", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Start,
    Stop,
    Restart,
    Snapshot,
    Restore,
    Reset,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, JsonSchema)]
#[sqlx(type_name = "operation_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, JsonSchema)]
#[sqlx(type_name = "snapshot_reason", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum SnapshotReason {
    Periodic,
    Shutdown,
    Manual,
    Restore,
    Reset,
    Export,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, JsonSchema)]
#[sqlx(type_name = "console_command_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ConsoleCommandStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ServerRecord {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub subdomain: String,
    pub is_default: bool,
    pub desired_state: DesiredState,
    pub observed_state: ObservedState,
    pub settings: serde_json::Value,
    pub storage_bytes: i64,
    pub generation: i64,
    pub current_snapshot_generation: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct WorkerRecord {
    pub id: Uuid,
    pub name: String,
    pub address: String,
    pub signal_address: String,
    pub total_memory_bytes: i64,
    pub total_disk_bytes: i64,
    pub total_cpu_millis: i64,
    pub reserved_memory_bytes: i64,
    pub reserved_disk_bytes: i64,
    pub reserved_cpu_millis: i64,
    pub heartbeat_at: DateTime<Utc>,
    pub draining: bool,
    pub port_min: i32,
    pub port_max: i32,
}

impl WorkerRecord {
    pub fn can_fit(&self) -> bool {
        !self.draining
            && self.total_memory_bytes - self.reserved_memory_bytes >= SERVER_MEMORY_BYTES
            && self.total_disk_bytes - self.reserved_disk_bytes >= SERVER_STORAGE_BYTES
            && self.total_cpu_millis - self.reserved_cpu_millis >= SERVER_CPU_MILLIS
            && self.port_max - self.port_min >= 2
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, JsonSchema)]
pub struct PlacementRecord {
    pub server_id: Uuid,
    pub worker_id: Uuid,
    pub generation: i64,
    pub lease_token: Uuid,
    pub java_port: i32,
    pub bedrock_port: i32,
    pub signal_port: i32,
    pub active: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct OperationRecord {
    pub id: Uuid,
    pub server_id: Uuid,
    pub worker_id: Option<Uuid>,
    pub kind: OperationKind,
    pub status: OperationStatus,
    pub generation: i64,
    pub attempt: i32,
    pub payload: serde_json::Value,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ConsoleCommandRecord {
    pub id: Uuid,
    pub server_id: Uuid,
    pub request_id: Uuid,
    pub worker_id: Option<Uuid>,
    pub generation: i64,
    pub attempt: i32,
    pub command: String,
    pub status: ConsoleCommandStatus,
    pub output: Vec<String>,
    pub error: Option<String>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SnapshotRecord {
    pub id: Uuid,
    pub server_id: Uuid,
    pub generation: i64,
    pub manifest_key: String,
    pub size_bytes: i64,
    pub checksum: String,
    pub reason: SnapshotReason,
    pub complete: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RouteTarget {
    pub server_id: Uuid,
    pub subdomain: String,
    pub state: ObservedState,
    pub account_suspended: bool,
    /// Placement generation used to fence stale backend connections.
    ///
    /// A target without an active placement has generation `0`; direct
    /// routing must never treat that value as a valid lease.
    pub generation: i64,
    pub worker_healthy: Option<bool>,
    pub worker_valid_until: Option<DateTime<Utc>>,
    pub worker_address: Option<String>,
    pub java_port: Option<u16>,
    pub bedrock_port: Option<u16>,
}

pub fn validate_subdomain(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 3 || bytes.len() > 32 || bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

pub fn subdomain_from_hostname(value: &str, root_domain: &str) -> Option<String> {
    let value = value.split('\0').next()?.trim();
    let host = match value.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') && port.parse::<u16>().is_ok() => host,
        _ => value,
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let root = root_domain
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if root.is_empty() {
        return None;
    }
    let subdomain = host.strip_suffix(&format!(".{root}"))?;
    if subdomain.contains('.') || !validate_subdomain(subdomain) {
        return None;
    }
    Some(subdomain.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_source_address_accepts_public_unicast_but_rejects_non_hosts() {
        assert!(is_gateway_source_address("198.51.100.23"));
        assert!(is_gateway_source_address("fd00::2"));
        assert!(!is_gateway_source_address("0.0.0.0"));
        assert!(!is_gateway_source_address("224.0.0.1"));
        assert!(!is_gateway_source_address("255.255.255.255"));
        assert!(!is_gateway_source_address("gateway.example.test"));
    }

    #[test]
    fn subdomain_validation_is_strict_and_dns_safe() {
        assert!(validate_subdomain("kurbis-42"));
        assert!(!validate_subdomain("ab"));
        assert!(!validate_subdomain("UPPER"));
        assert!(!validate_subdomain("-edge"));
        assert!(!validate_subdomain("edge-"));
        assert!(!validate_subdomain("edge.example"));
    }

    #[test]
    fn hostname_validation_is_shared_and_normalizes_client_input() {
        assert_eq!(
            subdomain_from_hostname(" Maple.Play.Example.Com.:19132 ", "play.example.com"),
            Some("maple".into())
        );
        assert_eq!(
            subdomain_from_hostname("a.b.play.example.com", "play.example.com"),
            None
        );
        assert_eq!(
            subdomain_from_hostname("maple.other.example.com", "play.example.com"),
            None
        );
        assert_eq!(
            subdomain_from_hostname("maple.play.example.com:invalid", "play.example.com"),
            None
        );
    }
}
