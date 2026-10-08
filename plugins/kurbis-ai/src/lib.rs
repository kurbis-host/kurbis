mod commands;
mod dashboard;
mod mcp;
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
const PERMISSION: &str = "kurbis-ai:command";
struct KurbisAi;
impl Plugin for KurbisAi {
    fn new() -> Self {
        Self
    }
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "kurbis-ai".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["kurbis Hoster".into()],
            description: "Operator AI for creating Pumpkin plugins and controlling the live server through scoped tools.".into(),
            dependencies: vec![],
            permissions: vec![permissions::FS_READ_DATA.into(), permissions::FS_WRITE_DATA.into()],
        }
    }
    fn on_load(&self, context: Context) -> Result<(), String> {
        storage::initialize().map_err(|e| e.to_string())?;
        context.register_permission(&Permission {
            node: PERMISSION.into(),
            description: "Use Kurbis AI plugin creation and server-control tools.".into(),
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
        context.schedule_repeating_task(2, 2, mcp::poll);
        context.schedule_repeating_task(20, 20, dashboard::poll);
        Ok(())
    }
}
register_plugin!(KurbisAi);
