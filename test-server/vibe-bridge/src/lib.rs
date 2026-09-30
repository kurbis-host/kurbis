mod commands;
mod dashboard;
mod storage;

use pumpkin_plugin_api::{
    Context, Plugin, PluginMetadata,
    events::{
        EventPriority, InventoryClickEvent, InventoryCloseEvent, InventoryDragEvent,
        PlayerChatEvent, PlayerLeaveEvent,
    },
    permission::{Permission, PermissionDefault, PermissionLevel},
    permissions, register_plugin,
    scheduler::SchedulerExt,
};
const PERMISSION: &str = "vibe-bridge:command";
struct VibeBridge;
impl Plugin for VibeBridge {
    fn new() -> Self {
        Self
    }
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "vibe-bridge".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Melon Hoster".into()],
            description: "Codex workshop: settings, sessions and live progress for operators."
                .into(),
            dependencies: vec![],
            permissions: vec![permissions::FS_WRITE_DATA.into()],
        }
    }
    fn on_load(&self, context: Context) -> Result<(), String> {
        storage::initialize().map_err(|e| e.to_string())?;
        context.register_permission(&Permission {
            node: PERMISSION.into(),
            description: "Use the Codex workshop.".into(),
            default: PermissionDefault::Op(PermissionLevel::One),
            children: vec![],
        })?;
        context.register_command(commands::command(), PERMISSION);
        context.register_event_handler::<InventoryClickEvent, _>(
            dashboard::Click,
            EventPriority::High,
            true,
        )?;
        context.register_event_handler::<InventoryCloseEvent, _>(
            dashboard::Close,
            EventPriority::High,
            true,
        )?;
        context.register_event_handler::<InventoryDragEvent, _>(
            dashboard::Drag,
            EventPriority::High,
            true,
        )?;
        context.register_event_handler::<PlayerChatEvent, _>(
            dashboard::Chat,
            EventPriority::High,
            true,
        )?;
        context.register_event_handler::<PlayerLeaveEvent, _>(
            dashboard::Leave,
            EventPriority::High,
            true,
        )?;
        context.schedule_repeating_task(20, 20, dashboard::poll);
        Ok(())
    }
}
register_plugin!(VibeBridge);
