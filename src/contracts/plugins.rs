//! Server plugin contracts shared by the browser panel, the in-game plugin and
//! workers.

use chrono::{DateTime, Utc};
use garde::Validate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Largest plugin binary a server may install, in bytes.
pub const MAX_PLUGIN_BYTES: usize = 32 * 1024 * 1024;
/// Plugins one server may have installed, enabled or not.
pub const MAX_SERVER_PLUGINS: usize = 16;
/// The header on a plugin binary the control plane serves a worker, naming the
/// binary's SHA-256.
pub const PLUGIN_SHA256_HEADER: &str = "x-kurbis-plugin-sha256";

string_enum!(
    /// Where an installed plugin came from.
    PluginSource {
        Market => "market",
        Upload => "upload",
    }
);

string_enum!(
    /// Whether the runtime runs the installed binary. `pending` means the
    /// runtime has not loaded this binary yet; a sleeping server loads it when
    /// it next starts.
    PluginState {
        Pending => "pending",
        Loaded => "loaded",
        Failed => "failed",
        Disabled => "disabled",
    }
);

/// One plugin installed on a server.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServerPlugin {
    pub id: Uuid,
    pub source: PluginSource,
    /// The market listing this plugin was installed from.
    pub market_plugin_id: Option<i64>,
    pub name: String,
    pub version: Option<String>,
    pub sha256: String,
    pub size_bytes: i64,
    pub enabled: bool,
    pub state: PluginState,
    /// The name and version the plugin reported when the runtime loaded it.
    pub runtime_name: Option<String>,
    pub runtime_version: Option<String>,
    /// Why the runtime could not load this binary.
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServerPlugins {
    pub plugins: Vec<ServerPlugin>,
    pub revision: i64,
    pub applied_revision: i64,
    /// Whether a running server has loaded the current list.
    pub synchronized: bool,
    pub max_plugins: usize,
    pub max_plugin_bytes: usize,
}

/// One free plugin listed on the Pumpkin market.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarketPlugin {
    pub id: i64,
    pub name: String,
    pub developer: String,
    pub category: String,
    pub description: String,
    pub version: String,
    pub downloads: i64,
    /// HTTPS image on the market's asset host.
    pub icon_url: Option<String>,
    /// The installed copy of this listing on the server, if any.
    pub installed_plugin_id: Option<Uuid>,
    pub installed_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarketPlugins {
    pub plugins: Vec<MarketPlugin>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarketPluginQuery {
    /// Matches names, developers, categories and descriptions; at most 64
    /// characters are used.
    pub query: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct MarketInstallRequest {
    /// The market listing to install, or to update to its latest version.
    #[garde(range(min = 1))]
    pub market_plugin_id: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginUploadQuery {
    /// The uploaded file's name; its stem names the plugin.
    pub file_name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct PluginUpdateRequest {
    #[garde(skip)]
    pub enabled: bool,
}

/// One enabled plugin the worker writes into the runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkerServerPlugin {
    pub id: Uuid,
    pub sha256: String,
    pub size_bytes: i64,
}

impl WorkerServerPlugin {
    /// The plugin's file in the runtime's installed-plugin directory. Its
    /// binary is part of the name, so an update is a new file.
    pub fn file_name(&self) -> String {
        format!("{}-{}.wasm", self.id.simple(), &self.sha256[..16])
    }
}

/// What the runtime did with one plugin file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkerPluginReport {
    pub id: Uuid,
    pub sha256: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkerPluginsApplied {
    pub generation: i64,
    pub revision: i64,
    pub plugins: Vec<WorkerPluginReport>,
}

/// Lowercase hex SHA-256, which names a stored plugin binary.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Pumpkin loads WebAssembly components only; a core module or any other
/// file is refused before it is stored.
pub fn validate_component(bytes: &[u8]) -> Result<(), &'static str> {
    const COMPONENT_HEADER: [u8; 8] = [0x00, b'a', b's', b'm', 0x0d, 0x00, 0x01, 0x00];
    if bytes.len() > MAX_PLUGIN_BYTES {
        return Err("the plugin is larger than 32 MiB");
    }
    if bytes.get(..4) != Some(&COMPONENT_HEADER[..4]) {
        return Err("the file is not a WebAssembly plugin");
    }
    if bytes.get(..8) != Some(&COMPONENT_HEADER[..]) {
        return Err(
            "the file is a WebAssembly module, not a component; build the plugin for wasm32-wasip2",
        );
    }
    Ok(())
}
