//! Shared route selection and startup waiting primitives for gateway protocols.
//!
//! The gateway must never send a client to a placement that became stale while
//! its server was waking or restarting.  This module keeps the readiness and
//! generation-fencing rules identical for Java and Bedrock while leaving each
//! protocol's transport/authentication logic in its own module.

use std::{
    future::Future,
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

use anyhow::Error;
use thiserror::Error as ThisError;
use uuid::Uuid;

use crate::model::{ObservedState, RouteTarget};

/// Protocol-specific backend port selected from a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteProtocol {
    Java,
    Bedrock,
}

/// The immutable placement identity captured before opening a backend stream.
///
/// A generation is required even when the worker address and port are reused:
/// a stop/start can produce a new placement with the same network coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteLease {
    pub server_id: Uuid,
    pub generation: i64,
    pub worker_address: IpAddr,
    pub port: u16,
    pub protocol: RouteProtocol,
}

impl RouteLease {
    /// Build a lease only when the target is safe for direct backend routing.
    pub fn from_target(target: &RouteTarget, protocol: RouteProtocol) -> Option<Self> {
        if !is_direct_ready(target) || target.generation <= 0 {
            return None;
        }
        let worker_address = target.worker_address.as_deref()?.parse().ok()?;
        let port = match protocol {
            RouteProtocol::Java => target.java_port?,
            RouteProtocol::Bedrock => target.bedrock_port?,
        };
        Some(Self {
            server_id: target.server_id,
            generation: target.generation,
            worker_address,
            port,
            protocol,
        })
    }

    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.worker_address, self.port)
    }

    /// Check that a freshly resolved route still denotes this exact placement.
    pub fn matches(&self, target: &RouteTarget) -> bool {
        Self::from_target(target, self.protocol).is_some_and(|current| current == *self)
    }
}

/// Whether a route has everything needed to connect directly to a live worker.
///
/// `Online` alone is insufficient: placement health, account state, generation,
/// and all protocol-specific network fields must also be valid.  Callers should
/// use [`RouteLease::from_target`] for the final protocol-specific check.
pub fn is_direct_ready(target: &RouteTarget) -> bool {
    target.state == ObservedState::Online
        && !target.account_suspended
        && target.worker_healthy == Some(true)
        && target.generation > 0
        && target
            .worker_valid_until
            .is_none_or(|deadline| deadline > chrono::Utc::now())
        && target.worker_address.is_some()
}

/// Resolve a known route until its placement is directly connectable.
///
/// The resolver is injected so Java and Bedrock can share this behavior and
/// tests can deterministically model sleeping, startup, reassignment, and
/// resolution failures without a database.  `None` is terminal: the known
/// server disappeared or was deleted, so callers must fail closed rather than
/// falling back to an older backend address.
pub async fn wait_for_direct_route<F, Fut>(
    protocol: RouteProtocol,
    timeout: Duration,
    poll_interval: Duration,
    mut resolve: F,
) -> Result<RouteLease, RouteWaitError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<RouteTarget>, Error>>,
{
    let deadline = Instant::now() + timeout;
    loop {
        let target = resolve().await.map_err(RouteWaitError::Resolve)?;
        let Some(target) = target else {
            return Err(RouteWaitError::RouteDisappeared);
        };
        if let Some(lease) = RouteLease::from_target(&target, protocol) {
            return Ok(lease);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(RouteWaitError::TimedOut { timeout });
        }
        let remaining = deadline.saturating_duration_since(now);
        tokio::time::sleep(poll_interval.min(remaining)).await;
    }
}

#[derive(Debug, ThisError)]
pub enum RouteWaitError {
    #[error("route resolution failed while waiting for a direct backend: {0}")]
    Resolve(#[source] Error),
    #[error("known server route disappeared while waiting for its backend")]
    RouteDisappeared,
    #[error("direct backend did not become ready within {timeout:?}")]
    TimedOut { timeout: Duration },
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Arc};

    use chrono::{Duration as ChronoDuration, Utc};
    use tokio::sync::Mutex;

    use super::*;

    fn target(state: ObservedState) -> RouteTarget {
        RouteTarget {
            server_id: Uuid::from_u128(1),
            subdomain: "maple".into(),
            state,
            account_suspended: false,
            generation: 7,
            worker_healthy: Some(true),
            worker_valid_until: Some(Utc::now() + ChronoDuration::minutes(1)),
            worker_address: Some("192.0.2.20".into()),
            java_port: Some(25565),
            bedrock_port: Some(19132),
        }
    }

    #[test]
    fn direct_readiness_requires_online_healthy_current_placement() {
        assert!(is_direct_ready(&target(ObservedState::Online)));

        for state in [
            ObservedState::Sleeping,
            ObservedState::Queued,
            ObservedState::Restoring,
            ObservedState::Starting,
            ObservedState::Stopping,
            ObservedState::Failed,
        ] {
            assert!(!is_direct_ready(&target(state)), "state={state:?}");
        }

        let mut suspended = target(ObservedState::Online);
        suspended.account_suspended = true;
        assert!(!is_direct_ready(&suspended));

        let mut unhealthy = target(ObservedState::Online);
        unhealthy.worker_healthy = Some(false);
        assert!(!is_direct_ready(&unhealthy));

        let mut expired = target(ObservedState::Online);
        expired.worker_valid_until = Some(Utc::now() - ChronoDuration::seconds(1));
        assert!(!is_direct_ready(&expired));

        let mut unassigned = target(ObservedState::Online);
        unassigned.generation = 0;
        unassigned.worker_address = None;
        assert!(!is_direct_ready(&unassigned));
    }

    #[test]
    fn lease_selects_protocol_port_and_fences_generation() {
        let java = RouteLease::from_target(&target(ObservedState::Online), RouteProtocol::Java)
            .expect("online Java route");
        assert_eq!(java.socket_addr().to_string(), "192.0.2.20:25565");

        let bedrock =
            RouteLease::from_target(&target(ObservedState::Online), RouteProtocol::Bedrock)
                .expect("online Bedrock route");
        assert_eq!(bedrock.socket_addr().to_string(), "192.0.2.20:19132");
        assert!(java.matches(&target(ObservedState::Online)));
        assert!(bedrock.matches(&target(ObservedState::Online)));
        assert_ne!(java.port, bedrock.port);

        let mut new_generation = target(ObservedState::Online);
        new_generation.generation += 1;
        assert!(
            !RouteLease::from_target(&target(ObservedState::Online), RouteProtocol::Java)
                .unwrap()
                .matches(&new_generation)
        );
    }

    #[tokio::test]
    async fn wait_polls_sleeping_route_then_returns_ready_generation() {
        let routes = Arc::new(Mutex::new(VecDeque::from([
            Ok(Some(target(ObservedState::Starting))),
            Ok(Some(target(ObservedState::Starting))),
            Ok(Some(target(ObservedState::Online))),
        ])));
        let result = {
            let routes = routes.clone();
            let future = wait_for_direct_route(
                RouteProtocol::Java,
                Duration::from_millis(250),
                Duration::from_millis(1),
                move || {
                    let routes = routes.clone();
                    async move { routes.lock().await.pop_front().expect("fixture route") }
                },
            );
            future.await
        }
        .expect("route should become ready");
        assert_eq!(result.generation, 7);
        assert_eq!(result.port, 25565);
    }

    #[tokio::test]
    async fn wait_times_out_without_connecting_to_stale_startup_route() {
        let result = wait_for_direct_route(
            RouteProtocol::Bedrock,
            Duration::from_millis(20),
            Duration::from_millis(5),
            || async { Ok(Some(target(ObservedState::Starting))) },
        );
        assert!(matches!(result.await, Err(RouteWaitError::TimedOut { .. })));
    }

    #[tokio::test]
    async fn wait_fails_closed_when_route_disappears() {
        let error = wait_for_direct_route(
            RouteProtocol::Java,
            Duration::from_secs(1),
            Duration::from_millis(1),
            || async { Ok(None) },
        )
        .await
        .expect_err("missing route must not use a stale backend");
        assert!(matches!(error, RouteWaitError::RouteDisappeared));
    }
}
