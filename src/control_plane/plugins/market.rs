//! Client for the Pumpkin plugin market (<https://market.pumpkinmc.org>).
//!
//! The market's JSON API lists every published plugin at `/api/plugins`,
//! describes one at `/api/plugins/{id}` and serves its WebAssembly component
//! at `/api/plugins/{id}/download`. Kurbis offers only free listings: a paid
//! plugin checks its license against the market at runtime, which a sandboxed
//! server cannot reach.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use futures::StreamExt;
use reqwest::{Client, Url, redirect::Policy};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::contracts::plugins::MAX_PLUGIN_BYTES;

pub const DEFAULT_MARKET_URL: &str = "https://market.pumpkinmc.org";
/// Icons are shown only from the market's own asset host.
const ICON_PREFIX: &str = "https://assets.pumpkinmc.org/";
const LISTING_TTL: Duration = Duration::from_secs(300);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_LISTING_BYTES: usize = 4 * 1024 * 1024;
const MAX_DESCRIPTION_CHARS: usize = 600;

/// One free, published market plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub id: i64,
    pub name: String,
    pub developer: String,
    pub category: String,
    pub description: String,
    pub version: String,
    pub downloads: i64,
    pub icon_url: Option<String>,
}

/// A listing's binary as the market served it.
pub struct Download {
    pub listing: Listing,
    pub bytes: Vec<u8>,
}

#[derive(Clone)]
pub struct PluginMarket {
    inner: Arc<Inner>,
}

struct Inner {
    base: Url,
    http: Client,
    listings: Mutex<Option<(Instant, Arc<Vec<Listing>>)>>,
}

#[derive(Deserialize)]
struct RawListing {
    id: i64,
    name: String,
    #[serde(default)]
    dev_name: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    downloads: Option<i64>,
    #[serde(default)]
    preview_path: Option<String>,
    #[serde(default)]
    price_cents: i64,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    is_preorder: bool,
    #[serde(default)]
    translated_descriptions: HashMap<String, String>,
}

impl RawListing {
    /// Kurbis installs only free plugins that have been published.
    fn into_listing(self) -> Option<Listing> {
        let installable = self.id > 0
            && self.price_cents == 0
            && self.kind.as_deref() == Some("free")
            && self.status.as_deref() == Some("published")
            && !self.is_preorder;
        if !installable {
            return None;
        }
        let description = self
            .translated_descriptions
            .get("en-US")
            .or_else(|| self.translated_descriptions.values().next())
            .map(|text| text.trim().chars().take(MAX_DESCRIPTION_CHARS).collect())
            .unwrap_or_default();
        Some(Listing {
            id: self.id,
            name: bounded(&self.name, 64)?,
            developer: self
                .dev_name
                .as_deref()
                .and_then(|name| bounded(name, 64))
                .unwrap_or_default(),
            category: self
                .category
                .as_deref()
                .and_then(|name| bounded(name, 64))
                .unwrap_or_default(),
            description,
            version: self
                .version
                .as_deref()
                .and_then(|version| bounded(version, 64))
                .unwrap_or_default(),
            downloads: self.downloads.unwrap_or(0).max(0),
            icon_url: self
                .preview_path
                .filter(|url| url.starts_with(ICON_PREFIX) && url.len() <= 512),
        })
    }
}

/// Trimmed text of at most `max` characters, or nothing when it is empty.
fn bounded(value: &str, max: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.chars().take(max).collect())
}

impl PluginMarket {
    pub fn new(base_url: &str) -> Result<Self> {
        let base = Url::parse(base_url).context("PLUGIN_MARKET_URL must be a URL")?;
        ensure!(
            matches!(base.scheme(), "https" | "http") && base.path() == "/",
            "PLUGIN_MARKET_URL must be an HTTP origin"
        );
        Ok(Self {
            inner: Arc::new(Inner {
                base,
                http: Client::builder()
                    .timeout(REQUEST_TIMEOUT)
                    .redirect(Policy::none())
                    .user_agent("Kurbis")
                    .build()?,
                listings: Mutex::default(),
            }),
        })
    }

    /// Every installable listing, most downloaded first. The market is asked
    /// at most every five minutes.
    pub async fn listings(&self) -> Result<Arc<Vec<Listing>>> {
        let mut cached = self.inner.listings.lock().await;
        if let Some((fetched_at, listings)) = cached.as_ref()
            && fetched_at.elapsed() < LISTING_TTL
        {
            return Ok(listings.clone());
        }
        let raw: Vec<RawListing> =
            serde_json::from_slice(&self.get("api/plugins", MAX_LISTING_BYTES).await?)
                .context("the plugin market sent an invalid listing")?;
        let mut listings = raw
            .into_iter()
            .filter_map(RawListing::into_listing)
            .collect::<Vec<_>>();
        listings.sort_by(|a, b| b.downloads.cmp(&a.downloads).then(a.id.cmp(&b.id)));
        let listings = Arc::new(listings);
        *cached = Some((Instant::now(), listings.clone()));
        Ok(listings)
    }

    /// The listing as it is now, or nothing when it cannot be installed.
    pub async fn listing(&self, id: i64) -> Result<Option<Listing>> {
        let raw: RawListing = serde_json::from_slice(
            &self
                .get(&format!("api/plugins/{id}"), MAX_LISTING_BYTES)
                .await?,
        )
        .context("the plugin market sent an invalid plugin")?;
        ensure!(
            raw.id == id,
            "the plugin market answered for another plugin"
        );
        Ok(raw.into_listing())
    }

    /// Download a listing's current binary.
    pub async fn download(&self, listing: Listing) -> Result<Download> {
        let bytes = self
            .get(
                &format!("api/plugins/{}/download", listing.id),
                MAX_PLUGIN_BYTES,
            )
            .await?;
        Ok(Download { listing, bytes })
    }

    async fn get(&self, path: &str, max_bytes: usize) -> Result<Vec<u8>> {
        let url = self.inner.base.join(path)?;
        let response = self.inner.http.get(url).send().await?;
        let status = response.status();
        if !status.is_success() {
            bail!("the plugin market answered {status} for /{path}");
        }
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes as u64)
        {
            bail!("the plugin market's answer for /{path} is too large");
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            ensure!(
                body.len() + chunk.len() <= max_bytes,
                "the plugin market's answer for /{path} is too large"
            );
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw(value: serde_json::Value) -> RawListing {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn only_free_published_plugins_are_installable() {
        let free = json!({
            "id": 55, "name": " TPSBar ", "dev_name": "PING", "category": "Admin Tools",
            "version": "0.1", "downloads": 215, "price_cents": 0, "type": "free",
            "status": "published", "is_preorder": false,
            "preview_path": "https://assets.pumpkinmc.org/icons/icon.jpg",
            "translated_descriptions": {"zh-CN": "性能", "en-US": "Performance in a boss bar."}
        });
        let listing = raw(free.clone()).into_listing().unwrap();
        assert_eq!(listing.name, "TPSBar");
        assert_eq!(listing.description, "Performance in a boss bar.");
        assert_eq!(
            listing.icon_url.as_deref(),
            Some("https://assets.pumpkinmc.org/icons/icon.jpg")
        );

        for (key, value) in [
            ("price_cents", json!(499)),
            ("type", json!("paid")),
            ("status", json!("draft")),
            ("is_preorder", json!(true)),
            ("name", json!("  ")),
        ] {
            let mut listing = free.clone();
            listing[key] = value;
            assert!(raw(listing).into_listing().is_none(), "{key}");
        }
    }

    #[test]
    fn icons_from_other_hosts_are_dropped() {
        let listing = raw(json!({
            "id": 1, "name": "X", "price_cents": 0, "type": "free", "status": "published",
            "preview_path": "https://tracker.example/icon.png"
        }))
        .into_listing()
        .unwrap();
        assert_eq!(listing.icon_url, None);
    }
}
