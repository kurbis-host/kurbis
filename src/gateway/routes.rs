use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use sqlx::{PgPool, postgres::PgListener};
use tokio::sync::RwLock;

use crate::{
    control_plane::api::resolve_subdomain,
    model::{RouteTarget, validate_subdomain},
};

#[derive(Clone)]
pub struct RouteCache {
    pool: PgPool,
    root_domain: String,
    cache: Arc<RwLock<CacheState>>,
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
            pool,
            root_domain: root_domain.trim_end_matches('.').to_ascii_lowercase(),
            cache: Arc::new(RwLock::new(CacheState::default())),
            ttl: Duration::from_secs(10),
        }
    }

    pub async fn resolve(&self, hostname: &str) -> Result<Option<RouteTarget>> {
        let Some(subdomain) = subdomain_for(hostname, &self.root_domain) else {
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
            let target = resolve_subdomain(&self.pool, &subdomain).await?;
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

    pub async fn invalidate_all(&self) {
        let mut cache = self.cache.write().await;
        cache.revision = cache.revision.wrapping_add(1);
        cache.entries.clear();
    }

    pub async fn invalidation_listener(&self) -> Result<PgListener> {
        let mut listener = PgListener::connect_with(&self.pool).await?;
        listener.listen("route_changes").await?;
        Ok(listener)
    }

    pub async fn listen_for_invalidations(self, mut listener: PgListener) -> Result<()> {
        loop {
            listener.recv().await?;
            self.invalidate_all().await;
        }
    }
}

fn subdomain_for(hostname: &str, root_domain: &str) -> Option<String> {
    let hostname = normalize_requested_hostname(hostname)?;
    let suffix = format!(
        ".{}",
        root_domain.trim_end_matches('.').to_ascii_lowercase()
    );
    let subdomain = hostname.strip_suffix(&suffix)?;
    if subdomain.contains('.') || !validate_subdomain(subdomain) {
        return None;
    }
    Some(subdomain.to_owned())
}

pub fn normalize_requested_hostname(value: &str) -> Option<String> {
    let value = value.split('\0').next()?.trim();
    let without_port = if let Some((host, port)) = value.rsplit_once(':') {
        if !host.contains(':') && port.parse::<u16>().is_ok() {
            host
        } else {
            value
        }
    } else {
        value
    };
    let hostname = without_port.trim_end_matches('.').to_ascii_lowercase();
    (!hostname.is_empty()).then_some(hostname)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_parser_rejects_nested_and_foreign_names() {
        assert_eq!(
            subdomain_for("abc.play.example.com", "play.example.com"),
            Some("abc".into())
        );
        assert_eq!(
            subdomain_for("abc.def.play.example.com", "play.example.com"),
            None
        );
        assert_eq!(
            subdomain_for("abc.other.example.com", "play.example.com"),
            None
        );
        assert_eq!(
            subdomain_for("abc.play.example.com:19132", "play.example.com"),
            Some("abc".into())
        );
    }
}
