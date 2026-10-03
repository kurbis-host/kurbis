//! Shared routing and gateway/control-plane contracts.
//!
//! Route resolution belongs to the database boundary rather than to an HTTP
//! handler. Both the gateway and control plane use this repository so a
//! gateway process can resolve routes without importing control-plane API
//! implementation details.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, postgres::PgListener};
use uuid::Uuid;

use crate::{
    model::{ObservedState, RouteTarget, subdomain_from_hostname},
    pumpkin_control::VerifiedMinecraftIdentity,
};

/// Typed payload sent by the waiting gateway when a player needs admission.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdmissionRequest {
    pub hostname: String,
    pub identity: Option<VerifiedMinecraftIdentity>,
    pub minecraft_player_id: Option<Uuid>,
    /// In-game name used when this verified identity first creates an account.
    pub display_name: Option<String>,
}

/// Typed admission result shared by the gateway client and control-plane API.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AdmissionResponse {
    pub target: RouteTarget,
    pub private_message: Option<String>,
    pub created: bool,
}

/// Current private Bedrock signaling assignment returned by the control plane.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BedrockSignalTarget {
    pub server_id: Uuid,
    pub worker_address: String,
    pub bedrock_port: u16,
    pub signal_address: String,
    pub signal_port: u16,
    pub generation: i64,
}

/// Database-backed route repository used by control-plane and gateway code.
#[derive(Clone)]
pub struct RouteRepository {
    pool: PgPool,
    root_domain: String,
}

impl RouteRepository {
    pub fn new(pool: PgPool, root_domain: impl Into<String>) -> Self {
        Self {
            pool,
            root_domain: normalize_root_domain(&root_domain.into()),
        }
    }

    pub fn accepts_server_address(&self, hostname: &str) -> bool {
        subdomain_from_hostname(hostname, &self.root_domain).is_some()
    }

    pub fn subdomain_for_hostname(&self, hostname: &str) -> Option<String> {
        subdomain_from_hostname(hostname, &self.root_domain)
    }

    pub async fn resolve_hostname(
        &self,
        hostname: &str,
    ) -> Result<Option<RouteTarget>, sqlx::Error> {
        let Some(subdomain) = self.subdomain_for_hostname(hostname) else {
            return Ok(None);
        };
        self.resolve_subdomain(&subdomain).await
    }

    pub async fn resolve_subdomain(
        &self,
        subdomain: &str,
    ) -> Result<Option<RouteTarget>, sqlx::Error> {
        resolve_subdomain(&self.pool, subdomain).await
    }

    pub async fn invalidation_listener(&self) -> Result<PgListener, sqlx::Error> {
        let mut listener = PgListener::connect_with(&self.pool).await?;
        listener.listen("route_changes").await?;
        Ok(listener)
    }
}

/// Resolve one canonical DNS label and its current worker placement.
pub async fn resolve_subdomain(
    pool: &PgPool,
    subdomain: &str,
) -> Result<Option<RouteTarget>, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        server_id: Uuid,
        subdomain: String,
        state: ObservedState,
        account_suspended: bool,
        worker_healthy: Option<bool>,
        worker_valid_until: Option<DateTime<Utc>>,
        worker_address: Option<String>,
        java_port: Option<i32>,
        bedrock_port: Option<i32>,
    }

    let row = sqlx::query_as::<_, Row>(
        "SELECT s.id AS server_id, s.subdomain, s.observed_state AS state,
                u.account_status <> 'active' AS account_suspended,
                CASE WHEN p.server_id IS NULL THEN NULL
                     ELSE w.heartbeat_at > now() - interval '30 seconds'
                END AS worker_healthy,
                w.heartbeat_at + interval '30 seconds' AS worker_valid_until,
                CASE WHEN u.account_status = 'active' AND p.active
                           AND w.heartbeat_at > now() - interval '30 seconds'
                     THEN w.address END AS worker_address,
                CASE WHEN u.account_status = 'active' AND p.active
                           AND w.heartbeat_at > now() - interval '30 seconds'
                     THEN p.java_port END AS java_port,
                CASE WHEN u.account_status = 'active' AND p.active
                           AND w.heartbeat_at > now() - interval '30 seconds'
                     THEN p.bedrock_port END AS bedrock_port
         FROM servers s
         JOIN users u ON u.id = s.owner_id
         LEFT JOIN placements p ON p.server_id = s.id
         LEFT JOIN workers w ON w.id = p.worker_id
         WHERE s.subdomain = $1 AND s.desired_state <> 'deleted'",
    )
    .bind(subdomain)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| RouteTarget {
        server_id: row.server_id,
        subdomain: row.subdomain,
        state: row.state,
        account_suspended: row.account_suspended,
        worker_healthy: row.worker_healthy,
        worker_valid_until: row.worker_valid_until,
        worker_address: row.worker_address,
        java_port: row.java_port.and_then(|value| u16::try_from(value).ok()),
        bedrock_port: row.bedrock_port.and_then(|value| u16::try_from(value).ok()),
    }))
}

fn normalize_root_domain(value: &str) -> String {
    value.trim().trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn repository_scopes_and_normalizes_hostnames() {
        let repository = RouteRepository::new(
            PgPool::connect_lazy("postgres://localhost/test").unwrap(),
            " Play.Example.Test. ",
        );
        assert!(repository.accepts_server_address("MAPLE.play.example.test:19132"));
        assert_eq!(
            repository.subdomain_for_hostname("MAPLE.play.example.test."),
            Some("maple".into())
        );
        assert!(!repository.accepts_server_address("nested.maple.play.example.test"));
        assert!(!repository.accepts_server_address("maple.other.example.test"));
    }

    #[test]
    fn admission_and_signal_contracts_round_trip_as_json() {
        let request = AdmissionRequest {
            hostname: "maple.play.example.test".into(),
            identity: Some(VerifiedMinecraftIdentity::Bedrock {
                xuid: "2533274790395904".into(),
            }),
            minecraft_player_id: Some(Uuid::new_v4()),
            display_name: Some("Admission Player".into()),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<AdmissionRequest>(&json)
                .unwrap()
                .hostname,
            request.hostname
        );

        let signal = BedrockSignalTarget {
            server_id: Uuid::new_v4(),
            worker_address: "192.0.2.20".into(),
            bedrock_port: 46001,
            signal_address: "127.0.0.1".into(),
            signal_port: 46002,
            generation: 3,
        };
        let json = serde_json::to_string(&signal).unwrap();
        assert_eq!(
            serde_json::from_str::<BedrockSignalTarget>(&json)
                .unwrap()
                .generation,
            3
        );

        let admission_schema =
            serde_json::to_value(schemars::schema_for!(AdmissionRequest)).unwrap();
        assert!(admission_schema["properties"]["hostname"].is_object());
        assert!(admission_schema["properties"]["identity"].is_object());
        assert!(admission_schema["properties"]["display_name"].is_object());
        let signal_schema =
            serde_json::to_value(schemars::schema_for!(BedrockSignalTarget)).unwrap();
        assert!(signal_schema["properties"]["signal_address"].is_object());
        assert!(signal_schema["properties"]["generation"].is_object());
    }
}
