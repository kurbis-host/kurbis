mod actions;
mod api;
mod chat;
mod command;
mod menu;
mod stats;

use std::{
    fs::{self, File},
    io::Read,
    path::Path,
    sync::OnceLock,
};

use pumpkin_plugin_api::{
    Context, Plugin, PluginMetadata,
    events::{
        BedrockFormResponseEvent, EventPriority, InventoryClickEvent, InventoryCloseEvent,
        InventoryDragEvent, PlayerChatEvent,
    },
    permission::{Permission, PermissionDefault, PermissionLevel},
    permissions, register_plugin,
};
use serde::Deserialize;
use uuid::Uuid;

pub(crate) const PERMISSION: &str = "kurbis-control:manage";
static CONFIG: OnceLock<Config> = OnceLock::new();

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub api_base_url: String,
    pub allow_insecure_local_http: bool,
    #[serde(default)]
    pub ci_preview: bool,
    pub server_id: Uuid,
    pub capability: String,
}

impl Config {
    fn load(path: &Path) -> Result<Self, String> {
        let path_metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !path_metadata.file_type().is_file() || path_metadata.len() > 4096 {
            return Err("plugin config must be a regular file smaller than 4 KiB".into());
        }
        let file = File::open(path).map_err(|error| error.to_string())?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > 4096 {
            return Err("plugin config must be a regular file smaller than 4 KiB".into());
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(4097)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > 4096 || bytes.len() as u64 != metadata.len() {
            return Err("plugin config changed or exceeded 4 KiB while reading".into());
        }
        let config: Self = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        let endpoint = api::Endpoint::parse(&config.api_base_url, config.allow_insecure_local_http)
            .map_err(|error| error.to_string())?;
        if config.server_id.is_nil() || !api::valid_token(&config.capability) {
            return Err("plugin config has invalid server credentials".into());
        }
        let _ = endpoint;
        Ok(config)
    }
}

struct KurbisControl;

impl Plugin for KurbisControl {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "kurbis-control".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["kurbis Hoster".into()],
            description: "Manage this hosted server and create a server-only login link.".into(),
            dependencies: vec![],
            permissions: vec![
                permissions::FS_READ_DATA.into(),
                permissions::HTTP_OUTBOUND.into(),
            ],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        let config_path = Path::new(&context.get_data_folder()).join("config.json");
        let config = Config::load(&config_path)?;
        let is_ci_preview = config.ci_preview;
        CONFIG
            .set(config)
            .map_err(|_| "plugin config was initialized more than once".to_owned())?;
        context.register_permission(&Permission {
            node: PERMISSION.into(),
            description: "Manage this Kurbis server from in game.".into(),
            default: PermissionDefault::Op(PermissionLevel::One),
            children: vec![],
        })?;
        context.register_command(command::command(), PERMISSION);
        context.register_event_handler::<InventoryClickEvent, _>(
            menu::Click,
            EventPriority::High,
            true,
        )?;
        context.register_event_handler::<InventoryCloseEvent, _>(
            menu::Close,
            EventPriority::High,
            true,
        )?;
        context.register_event_handler::<InventoryDragEvent, _>(
            menu::Drag,
            EventPriority::High,
            true,
        )?;
        context.register_event_handler::<PlayerChatEvent, _>(
            menu::ChatInput,
            EventPriority::High,
            true,
        )?;
        context.register_event_handler::<BedrockFormResponseEvent, _>(
            menu::BedrockForm,
            EventPriority::High,
            true,
        )?;
        if !is_ci_preview {
            context.register_event_handler::<pumpkin_plugin_api::events::PlayerJoinEvent, _>(
                stats::Join,
                EventPriority::Normal,
                true,
            )?;
            stats::start(&context);
        }
        Ok(())
    }
}

pub(crate) fn config() -> Result<&'static Config, anyhow::Error> {
    CONFIG
        .get()
        .ok_or_else(|| anyhow::anyhow!("Kurbis plugin is not configured"))
}

register_plugin!(KurbisControl);
