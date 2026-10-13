//! Plugins a server's owner installs from the Pumpkin market or uploads.
//!
//! `server_plugins` is the authoritative list. Each binary is stored once in
//! object storage under `plugins/<sha256>.wasm`, however many servers use it,
//! and recorded in `plugin_blobs`. Workers write a server's enabled plugins
//! into its runtime and report what Pumpkin loaded; see `worker::plugins`.

pub mod market;

use std::sync::Arc;

use anyhow::Context;
use chrono::{DateTime, Utc};
use object_store::{ObjectStore, PutPayload, path::Path};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    contracts::plugins::{
        MAX_PLUGIN_BYTES, MAX_SERVER_PLUGINS, MarketPlugin, PluginSource, PluginState,
        ServerPlugin, ServerPlugins, WorkerPluginReport, WorkerServerPlugin, sha256_hex,
        validate_component,
    },
    control_plane::object_cleanup::delete_if_present,
};

pub use market::{DEFAULT_MARKET_URL, PluginMarket};

/// An unused binary is kept this long, so an install that is storing it
/// cannot lose it to garbage collection.
const UNUSED_BLOB_GRACE: &str = "1 hour";
const GC_BATCH: i64 = 64;

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin not found")]
    NotFound,
    #[error("this plugin is not a free, published plugin on the market")]
    NotInstallable,
    #[error("the plugin market could not be reached")]
    MarketUnavailable(#[source] anyhow::Error),
    #[error("{0}")]
    InvalidBinary(&'static str),
    #[error("a server can have at most {MAX_SERVER_PLUGINS} plugins")]
    TooMany,
    #[error("this plugin is already installed")]
    AlreadyInstalled,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

#[derive(sqlx::FromRow)]
struct PluginRow {
    id: Uuid,
    source: String,
    market_plugin_id: Option<i64>,
    name: String,
    version: Option<String>,
    sha256: String,
    size_bytes: i64,
    enabled: bool,
    reported_sha256: Option<String>,
    runtime_name: Option<String>,
    runtime_version: Option<String>,
    runtime_error: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

const PLUGIN_COLUMNS: &str = "p.id, p.source, p.market_plugin_id, p.name, p.version, p.sha256,
    b.size_bytes, p.enabled, p.reported_sha256, p.runtime_name, p.runtime_version,
    p.runtime_error, p.created_at, p.updated_at";

impl PluginRow {
    fn into_plugin(self) -> ServerPlugin {
        let reported = self.reported_sha256.as_deref() == Some(self.sha256.as_str());
        let state = if !self.enabled {
            PluginState::Disabled
        } else if !reported {
            PluginState::Pending
        } else if self.runtime_error.is_some() {
            PluginState::Failed
        } else {
            PluginState::Loaded
        };
        ServerPlugin {
            id: self.id,
            source: PluginSource::parse(&self.source).unwrap_or(PluginSource::Upload),
            market_plugin_id: self.market_plugin_id,
            name: self.name,
            version: self.version,
            sha256: self.sha256,
            size_bytes: self.size_bytes,
            enabled: self.enabled,
            state,
            runtime_name: reported.then_some(self.runtime_name).flatten(),
            runtime_version: reported.then_some(self.runtime_version).flatten(),
            error: reported.then_some(self.runtime_error).flatten(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

pub fn blob_path(sha256: &str) -> Path {
    Path::from(format!("plugins/{sha256}.wasm"))
}

pub async fn server_plugins(pool: &PgPool, server_id: Uuid) -> Result<ServerPlugins, PluginError> {
    let plugins = sqlx::query_as::<_, PluginRow>(&format!(
        "SELECT {PLUGIN_COLUMNS}
         FROM server_plugins p JOIN plugin_blobs b ON b.sha256 = p.sha256
         WHERE p.server_id = $1
         ORDER BY lower(p.name), p.created_at, p.id"
    ))
    .bind(server_id)
    .fetch_all(pool)
    .await?;
    let (revision, applied_revision) = sqlx::query_as::<_, (i64, i64)>(
        "SELECT revision, applied_revision FROM server_plugin_state WHERE server_id = $1",
    )
    .bind(server_id)
    .fetch_optional(pool)
    .await?
    .unwrap_or((0, 0));
    Ok(ServerPlugins {
        plugins: plugins.into_iter().map(PluginRow::into_plugin).collect(),
        revision,
        applied_revision,
        synchronized: revision == applied_revision,
        max_plugins: MAX_SERVER_PLUGINS,
        max_plugin_bytes: MAX_PLUGIN_BYTES,
    })
}

async fn server_plugin(
    pool: &PgPool,
    server_id: Uuid,
    id: Uuid,
) -> Result<ServerPlugin, PluginError> {
    sqlx::query_as::<_, PluginRow>(&format!(
        "SELECT {PLUGIN_COLUMNS}
         FROM server_plugins p JOIN plugin_blobs b ON b.sha256 = p.sha256
         WHERE p.server_id = $1 AND p.id = $2"
    ))
    .bind(server_id)
    .bind(id)
    .fetch_optional(pool)
    .await?
    .map(PluginRow::into_plugin)
    .ok_or(PluginError::NotFound)
}

/// The market's installable listings matching `query`, each marked with the
/// server's installed copy.
pub async fn market_plugins(
    pool: &PgPool,
    market: &PluginMarket,
    server_id: Uuid,
    query: Option<&str>,
) -> Result<Vec<MarketPlugin>, PluginError> {
    let listings = market
        .listings()
        .await
        .map_err(PluginError::MarketUnavailable)?;
    let installed: Vec<(i64, Uuid, Option<String>)> = sqlx::query_as(
        "SELECT market_plugin_id, id, version FROM server_plugins
         WHERE server_id = $1 AND market_plugin_id IS NOT NULL",
    )
    .bind(server_id)
    .fetch_all(pool)
    .await?;
    let terms = query
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(str::to_lowercase);
    Ok(listings
        .iter()
        .filter(|listing| {
            terms.as_deref().is_none_or(|terms| {
                [
                    &listing.name,
                    &listing.developer,
                    &listing.category,
                    &listing.description,
                ]
                .iter()
                .any(|field| field.to_lowercase().contains(terms))
            })
        })
        .map(|listing| {
            let installed = installed.iter().find(|(id, _, _)| *id == listing.id);
            MarketPlugin {
                id: listing.id,
                name: listing.name.clone(),
                developer: listing.developer.clone(),
                category: listing.category.clone(),
                description: listing.description.clone(),
                version: listing.version.clone(),
                downloads: listing.downloads,
                icon_url: listing.icon_url.clone(),
                installed_plugin_id: installed.map(|(_, id, _)| *id),
                installed_version: installed.and_then(|(_, _, version)| version.clone()),
            }
        })
        .collect())
}

/// Install a market plugin, or update an installed one to the listing's
/// current version.
pub async fn install_market(
    pool: &PgPool,
    store: &Arc<dyn ObjectStore>,
    market: &PluginMarket,
    server_id: Uuid,
    market_plugin_id: i64,
) -> Result<ServerPlugin, PluginError> {
    let listing = market
        .listing(market_plugin_id)
        .await
        .map_err(PluginError::MarketUnavailable)?
        .ok_or(PluginError::NotInstallable)?;
    let installed: Option<(Uuid, Option<String>)> = sqlx::query_as(
        "SELECT id, version FROM server_plugins WHERE server_id = $1 AND market_plugin_id = $2",
    )
    .bind(server_id)
    .bind(market_plugin_id)
    .fetch_optional(pool)
    .await?;
    match &installed {
        Some((id, version)) if version.as_deref() == Some(listing.version.as_str()) => {
            return server_plugin(pool, server_id, *id).await;
        }
        Some(_) => {}
        None => ensure_room(pool, server_id).await?,
    }
    let download = market
        .download(listing)
        .await
        .map_err(PluginError::MarketUnavailable)?;
    let (sha256, size) = store_blob(pool, store, download.bytes).await?;
    let listing = download.listing;
    let version = (!listing.version.is_empty()).then_some(listing.version);
    let mut tx = pool.begin().await?;
    lock_server(&mut tx, server_id).await?;
    if installed.is_none() {
        ensure_room(&mut *tx, server_id).await?;
    }
    let id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO server_plugins(server_id, source, market_plugin_id, name, version, sha256)
         VALUES($1, 'market', $2, $3, $4, $5)
         ON CONFLICT(server_id, market_plugin_id) WHERE market_plugin_id IS NOT NULL
         DO UPDATE SET name = EXCLUDED.name, version = EXCLUDED.version,
                       sha256 = EXCLUDED.sha256, updated_at = now()
         RETURNING id",
    )
    .bind(server_id)
    .bind(listing.id)
    .bind(&listing.name)
    .bind(version)
    .bind(&sha256)
    .fetch_one(&mut *tx)
    .await
    .map_err(duplicate_binary)?;
    tx.commit().await?;
    tracing::info!(%server_id, plugin_id = %id, market_plugin_id, size, "installed market plugin");
    server_plugin(pool, server_id, id).await
}

/// Install a plugin binary the owner uploaded. Its file name names it.
pub async fn install_upload(
    pool: &PgPool,
    store: &Arc<dyn ObjectStore>,
    server_id: Uuid,
    file_name: &str,
    bytes: Vec<u8>,
) -> Result<ServerPlugin, PluginError> {
    let name = upload_name(file_name).ok_or(PluginError::InvalidBinary(
        "the file name must name the plugin",
    ))?;
    ensure_room(pool, server_id).await?;
    let (sha256, size) = store_blob(pool, store, bytes).await?;
    let mut tx = pool.begin().await?;
    lock_server(&mut tx, server_id).await?;
    ensure_room(&mut *tx, server_id).await?;
    let id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO server_plugins(server_id, source, name, sha256)
         VALUES($1, 'upload', $2, $3)
         RETURNING id",
    )
    .bind(server_id)
    .bind(&name)
    .bind(&sha256)
    .fetch_one(&mut *tx)
    .await
    .map_err(duplicate_binary)?;
    tx.commit().await?;
    tracing::info!(%server_id, plugin_id = %id, size, "installed uploaded plugin");
    server_plugin(pool, server_id, id).await
}

pub async fn set_enabled(
    pool: &PgPool,
    server_id: Uuid,
    id: Uuid,
    enabled: bool,
) -> Result<ServerPlugin, PluginError> {
    let updated = sqlx::query(
        "UPDATE server_plugins SET enabled = $3, updated_at = now()
         WHERE server_id = $1 AND id = $2 AND enabled <> $3",
    )
    .bind(server_id)
    .bind(id)
    .bind(enabled)
    .execute(pool)
    .await?;
    let plugin = server_plugin(pool, server_id, id).await?;
    if updated.rows_affected() > 0 {
        tracing::info!(%server_id, plugin_id = %id, enabled, "changed plugin");
    }
    Ok(plugin)
}

pub async fn remove(pool: &PgPool, server_id: Uuid, id: Uuid) -> Result<(), PluginError> {
    let removed = sqlx::query("DELETE FROM server_plugins WHERE server_id = $1 AND id = $2")
        .bind(server_id)
        .bind(id)
        .execute(pool)
        .await?;
    if removed.rows_affected() == 0 {
        return Err(PluginError::NotFound);
    }
    tracing::info!(%server_id, plugin_id = %id, "removed plugin");
    Ok(())
}

/// The plugins a worker writes into the runtime.
pub async fn worker_plugins(
    pool: &PgPool,
    server_id: Uuid,
) -> Result<Vec<WorkerServerPlugin>, sqlx::Error> {
    let rows: Vec<(Uuid, String, i64)> = sqlx::query_as(
        "SELECT p.id, p.sha256, b.size_bytes
         FROM server_plugins p JOIN plugin_blobs b ON b.sha256 = p.sha256
         WHERE p.server_id = $1 AND p.enabled
         ORDER BY p.id",
    )
    .bind(server_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, sha256, size_bytes)| WorkerServerPlugin {
            id,
            sha256,
            size_bytes,
        })
        .collect())
}

/// The binary of one of the server's enabled plugins.
pub async fn enabled_plugin_blob(
    pool: &PgPool,
    server_id: Uuid,
    id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT sha256 FROM server_plugins WHERE server_id = $1 AND id = $2 AND enabled",
    )
    .bind(server_id)
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Record what the runtime loaded and, when nothing changed since the worker
/// read the list, that it applied `revision`.
pub async fn record_applied(
    tx: &mut Transaction<'_, Postgres>,
    server_id: Uuid,
    revision: i64,
    reports: &[WorkerPluginReport],
) -> Result<(), sqlx::Error> {
    for report in reports {
        sqlx::query(
            "UPDATE server_plugins
             SET reported_sha256 = $3, runtime_name = $4, runtime_version = $5,
                 runtime_error = $6, reported_at = now()
             WHERE server_id = $1 AND id = $2 AND sha256 = $3",
        )
        .bind(server_id)
        .bind(report.id)
        .bind(&report.sha256)
        .bind(report.name.as_deref().map(|name| truncate(name, 64)))
        .bind(
            report
                .version
                .as_deref()
                .map(|version| truncate(version, 64)),
        )
        .bind(report.error.as_deref().map(|error| truncate(error, 1024)))
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query(
        "UPDATE server_plugin_state SET applied_revision = $2, updated_at = now()
         WHERE server_id = $1 AND revision = $2 AND applied_revision <> $2",
    )
    .bind(server_id)
    .bind(revision)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Delete binaries no server has used for an hour. Each binary's row stays
/// locked until its object is gone, so an install of the same binary waits
/// and then stores it again.
pub async fn collect_garbage(pool: &PgPool, store: &Arc<dyn ObjectStore>) -> anyhow::Result<u64> {
    let mut deleted = 0;
    for _ in 0..GC_BATCH {
        let mut tx = pool.begin().await?;
        let Some(sha256) = sqlx::query_scalar::<_, String>(&format!(
            "SELECT b.sha256 FROM plugin_blobs b
             WHERE b.last_used_at < now() - interval '{UNUSED_BLOB_GRACE}'
               AND NOT EXISTS(SELECT 1 FROM server_plugins p WHERE p.sha256 = b.sha256)
             ORDER BY b.last_used_at
             LIMIT 1
             FOR UPDATE SKIP LOCKED"
        ))
        .fetch_optional(&mut *tx)
        .await?
        else {
            break;
        };
        delete_if_present(store.as_ref(), &blob_path(&sha256))
            .await
            .context("delete unused plugin binary")?;
        sqlx::query("DELETE FROM plugin_blobs WHERE sha256 = $1")
            .bind(&sha256)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        deleted += 1;
    }
    Ok(deleted)
}

/// Validate and store a binary, returning its hash and size.
async fn store_blob(
    pool: &PgPool,
    store: &Arc<dyn ObjectStore>,
    bytes: Vec<u8>,
) -> Result<(String, i64), PluginError> {
    validate_component(&bytes).map_err(PluginError::InvalidBinary)?;
    let sha256 = sha256_hex(&bytes);
    let size = i64::try_from(bytes.len()).context("plugin size")?;
    // Recording the binary as used first keeps garbage collection away from
    // it while it is written.
    sqlx::query(
        "INSERT INTO plugin_blobs(sha256, size_bytes) VALUES($1, $2)
         ON CONFLICT(sha256) DO UPDATE SET last_used_at = now()",
    )
    .bind(&sha256)
    .bind(size)
    .execute(pool)
    .await?;
    store
        .put(&blob_path(&sha256), PutPayload::from(bytes))
        .await
        .context("store plugin binary")?;
    Ok((sha256, size))
}

async fn lock_server(
    tx: &mut Transaction<'_, Postgres>,
    server_id: Uuid,
) -> Result<(), PluginError> {
    sqlx::query("SELECT 1 FROM servers WHERE id = $1 FOR UPDATE")
        .bind(server_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(PluginError::NotFound)?;
    Ok(())
}

async fn ensure_room<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    server_id: Uuid,
) -> Result<(), PluginError> {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM server_plugins WHERE server_id = $1")
        .bind(server_id)
        .fetch_one(executor)
        .await?;
    if count >= MAX_SERVER_PLUGINS as i64 {
        return Err(PluginError::TooMany);
    }
    Ok(())
}

fn duplicate_binary(error: sqlx::Error) -> PluginError {
    match &error {
        sqlx::Error::Database(database) if database.is_unique_violation() => {
            PluginError::AlreadyInstalled
        }
        _ => error.into(),
    }
}

/// The plugin name an uploaded file's name gives: its stem without control
/// characters, at most 64 characters.
fn upload_name(file_name: &str) -> Option<String> {
    let base = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    let stem = base
        .len()
        .checked_sub(5)
        .filter(|&end| base.is_char_boundary(end) && base[end..].eq_ignore_ascii_case(".wasm"))
        .map_or(base, |end| &base[..end]);
    let name = stem
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    let name = name.trim();
    (!name.is_empty()).then(|| truncate(name, 64))
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_names_come_from_the_file_stem() {
        assert_eq!(upload_name("TPSBar.wasm").as_deref(), Some("TPSBar"));
        assert_eq!(
            upload_name("C:\\builds\\my plugin.WASM").as_deref(),
            Some("my plugin")
        );
        assert_eq!(upload_name("../../x.wasm").as_deref(), Some("x"));
        assert_eq!(upload_name("plugin").as_deref(), Some("plugin"));
        assert_eq!(upload_name(" .wasm"), None);
        assert_eq!(upload_name("a\u{7}b.wasm").as_deref(), Some("ab"));
        assert_eq!(upload_name(&"é".repeat(80)).unwrap().chars().count(), 64);
    }

    #[test]
    fn only_webassembly_components_are_accepted() {
        let component = [0x00, b'a', b's', b'm', 0x0d, 0x00, 0x01, 0x00, 0x01];
        assert!(validate_component(&component).is_ok());
        let module = [0x00, b'a', b's', b'm', 0x01, 0x00, 0x00, 0x00];
        assert!(validate_component(&module).unwrap_err().contains("module"));
        assert!(validate_component(b"PK\x03\x04").is_err());
        assert!(validate_component(b"").is_err());
    }

    #[test]
    fn state_follows_the_reported_binary() {
        let row = |enabled, reported: Option<&str>, error: Option<&str>| {
            PluginRow {
                id: Uuid::nil(),
                source: "market".into(),
                market_plugin_id: Some(1),
                name: "X".into(),
                version: None,
                sha256: "a".repeat(64),
                size_bytes: 1,
                enabled,
                reported_sha256: reported.map(str::to_owned),
                runtime_name: Some("x".into()),
                runtime_version: None,
                runtime_error: error.map(str::to_owned),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            }
            .into_plugin()
        };
        let current = "a".repeat(64);
        let older = "b".repeat(64);
        assert_eq!(row(true, None, None).state, PluginState::Pending);
        assert_eq!(
            row(true, Some(&older), Some("boom")).state,
            PluginState::Pending
        );
        assert_eq!(row(true, Some(&older), Some("boom")).error, None);
        assert_eq!(row(true, Some(&current), None).state, PluginState::Loaded);
        assert_eq!(
            row(true, Some(&current), Some("boom")).state,
            PluginState::Failed
        );
        assert_eq!(
            row(false, Some(&current), None).state,
            PluginState::Disabled
        );
    }
}
