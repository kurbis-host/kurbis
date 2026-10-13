use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use sqlx::{PgPool, postgres::PgListener};
use tokio::sync::{RwLock, watch};

use crate::routing::RouteRepository;
use crate::{model::RouteTarget, routing::CiRouteTarget};

#[derive(Clone)]
pub struct RouteCache {
    repository: RouteRepository,
    cache: Arc<RwLock<CacheState>>,
    changed: watch::Sender<u64>,
    ttl: Duration,
}

#[derive(Default)]
struct CacheState {
    revision: u64,
    entries: HashMap<String, CachedRoute>,
}

#[derive(Clone)]
struct CachedRoute {
    target: Option<RouteTarget>,
    expires: Instant,
}

impl RouteCache {
    pub fn new(pool: PgPool, root_domain: String) -> Self {
        Self {
            repository: RouteRepository::new(pool, root_domain),
            cache: Arc::new(RwLock::new(CacheState::default())),
            changed: watch::Sender::new(0),
            ttl: Duration::from_secs(10),
        }
    }

    pub async fn resolve(&self, hostname: &str) -> Result<Option<RouteTarget>> {
        let Some(subdomain) = self.repository.subdomain_for_hostname(hostname) else {
            return Ok(None);
        };
        loop {
            let cache = self.cache.read().await;
            if let Some(cached) = cache.entries.get(&subdomain)
                && cached.expires > Instant::now()
            {
                return Ok(cached.target.clone());
            }
            let revision = cache.revision;
            drop(cache);
            let target = self.repository.resolve_subdomain(&subdomain).await?;
            let mut cache = self.cache.write().await;
            if cache.revision != revision {
                continue;
            }
            let remaining_health = target
                .as_ref()
                .filter(|target| target.worker_healthy == Some(true))
                .and_then(|target| target.worker_valid_until)
                .map(|deadline| (deadline - chrono::Utc::now()).to_std().unwrap_or_default());
            let ttl = remaining_health.map_or(self.ttl, |remaining| self.ttl.min(remaining));
            cache.entries.insert(
                subdomain.clone(),
                CachedRoute {
                    target: target.clone(),
                    expires: Instant::now() + ttl,
                },
            );
            return Ok(target);
        }
    }

    /// Resolve directly from the repository when a generation-sensitive
    /// connection is about to be opened. The cache remains useful for status
    /// and startup polling, but a direct backend must not rely on a stale
    /// placement snapshot.
    pub async fn resolve_current(&self, hostname: &str) -> Result<Option<RouteTarget>> {
        let Some(subdomain) = self.repository.subdomain_for_hostname(hostname) else {
            return Ok(None);
        };
        Ok(self.repository.resolve_subdomain(&subdomain).await?)
    }

    pub fn accepts_server_address(&self, hostname: &str) -> bool {
        self.repository.accepts_server_address(hostname)
    }

    pub fn accepts_ci_address(&self, hostname: &str, ci_root_domain: &str) -> bool {
        self.repository
            .ci_subdomain_for_hostname(hostname, ci_root_domain)
            .is_some()
    }

    pub async fn resolve_ci(
        &self,
        hostname: &str,
        ci_root_domain: &str,
    ) -> Result<Option<CiRouteTarget>> {
        Ok(self
            .repository
            .resolve_ci_hostname(hostname, ci_root_domain)
            .await?)
    }

    pub async fn invalidate_all(&self) {
        let mut cache = self.cache.write().await;
        cache.revision = cache.revision.wrapping_add(1);
        cache.entries.clear();
        drop(cache);
        self.changed
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// Fires whenever a route may have changed, so a caller waiting for a
    /// server to come online can look again at once instead of on a timer.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    pub async fn invalidation_listener(&self) -> Result<PgListener> {
        Ok(self.repository.invalidation_listener().await?)
    }

    pub async fn listen_for_invalidations(self, mut listener: PgListener) -> Result<()> {
        loop {
            // recv() hides reconnection gaps; notifications can be lost while
            // disconnected. try_recv() exposes that gap as None, which also
            // invalidates all previously cached routing decisions.
            listener.try_recv().await?;
            self.invalidate_all().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hostname_parser_rejects_nested_and_foreign_names() {
        let routes = RouteRepository::new(
            PgPool::connect_lazy("postgres://localhost/test").unwrap(),
            "play.example.com",
        );
        assert_eq!(
            routes.subdomain_for_hostname("abc.play.example.com"),
            Some("abc".into())
        );
        assert_eq!(
            routes.subdomain_for_hostname("abc.def.play.example.com"),
            None
        );
        assert_eq!(routes.subdomain_for_hostname("abc.other.example.com"), None);
        assert_eq!(
            routes.subdomain_for_hostname("abc.play.example.com:19132"),
            Some("abc".into())
        );
    }
}
