use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard, OnceLock},
};

use anyhow::{Context, Result};
use pumpkin_plugin_api::{
    Inventory, ItemStack, Player, Screen, Server,
    events::{
        BedrockFormResponseEvent, EventData, EventHandler, InventoryClickEvent,
        InventoryCloseEvent, InventoryDragEvent, PlayerChatEvent,
    },
    forms::{CustomFormBuilder, FormResponse},
    gui::Gui,
    java_packets::{CCloseContainer, ClientboundPacket},
    text::TextComponent,
};
use serde_json::Value;

use crate::{
    PERMISSION,
    actions::{self, LifecycleAction},
    api, chat,
};

const CONTENT_ROWS: &[u32] = &[10, 19, 28, 37];
const CONTENT_COLS: usize = 7;
const PAGE_SIZE: usize = CONTENT_ROWS.len() * CONTENT_COLS; // 28

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum View {
    Home,
    Status,
    Players(usize),
    Logs(usize),
    Activity(usize),
    Settings,
    SelectDifficulty,
    SelectGamemode,
    Whitelist(usize),
    ConfirmRemoveWhitelist {
        name: String,
        player_id: String,
        page: usize,
    },
    Snapshots(usize),
    Controls,
    LifecycleConfirm(LifecycleAction),
    RestoreConfirm {
        generation: i64,
        page: usize,
    },
}

impl View {
    fn with_page(&self, page: usize) -> Option<Self> {
        match self {
            Self::Players(_) => Some(Self::Players(page)),
            Self::Logs(_) => Some(Self::Logs(page)),
            Self::Activity(_) => Some(Self::Activity(page)),
            Self::Whitelist(_) => Some(Self::Whitelist(page)),
            Self::Snapshots(_) => Some(Self::Snapshots(page)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
enum Action {
    Open(View),
    Execute(LifecycleAction),
    Login,
    Restore {
        generation: i64,
        page: usize,
    },
    AdjustSetting {
        key: &'static str,
        direction: i8,
    },
    ToggleSetting {
        key: &'static str,
    },
    SetSetting {
        key: &'static str,
        value: &'static str,
    },
    PromptMotd,
    PromptWhitelistAdd(usize),
    ConfirmRemoveWhitelist {
        name: String,
        player_id: String,
        page: usize,
    },
}

#[derive(Clone, Debug)]
enum TextInputTarget {
    Motd,
    WhitelistAdd(usize),
}

struct ActiveMenu {
    inventory: Inventory,
    actions: HashMap<i16, Action>,
    view: View,
}

#[derive(Default)]
struct State {
    menus: HashMap<String, ActiveMenu>,
    pending_text_inputs: HashMap<String, TextInputTarget>,
    bedrock_forms: HashMap<(String, u32), TextInputTarget>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}

fn lock_state() -> MutexGuard<'static, State> {
    state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn player_key(player: &Player) -> String {
    pumpkin_plugin_api::uuid::to_string(player.get_id())
}

fn tell(player: &Player, message: &str) {
    player.send_system_message(TextComponent::text(message), false);
}

pub(crate) fn page_data(view: &View) -> Result<Value> {
    let suffix = match view {
        View::Status | View::Settings | View::SelectDifficulty | View::SelectGamemode => "",
        View::Players(_) => "/players",
        View::Logs(_) => "/logs?limit=100",
        View::Activity(_) => "/activity?limit=50",
        View::Whitelist(_) | View::ConfirmRemoveWhitelist { .. } => "/whitelist",
        View::Snapshots(_) => "/snapshots",
        _ => return Ok(Value::Null),
    };
    api::get(&api::server_path(suffix)?)
}

pub(crate) fn open(player: &Player, view: View) -> Result<()> {
    anyhow::ensure!(
        player.has_permission(PERMISSION),
        "Operator access required"
    );
    let key = player_key(player);
    let data = page_data(&view)?;

    let mut state = lock_state();
    if let Some(active) = state.menus.get_mut(&key) {
        // Reuse open container without closing/reopening screen
        active.view = view.clone();
        active.actions.clear();
        render_menu(&active.inventory, &mut active.actions, &view, &data)?;
        return Ok(());
    }

    let title = view_title(&view);
    let gui = Gui::new(Screen::Generic9x6, TextComponent::text(title));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    let inventory = gui.get_inventory();

    let mut actions = HashMap::new();
    render_menu(&inventory, &mut actions, &view, &data)?;

    state.menus.insert(
        key,
        ActiveMenu {
            inventory,
            actions,
            view,
        },
    );
    player.open_gui(gui);
    Ok(())
}

fn render_menu(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    view: &View,
    data: &Value,
) -> Result<()> {
    inv.clear();
    // Fill background border panes
    for slot in 0..54 {
        inv.set_item(slot, Some(item("black_stained_glass_pane", " ", "")));
    }

    // View-specific contents
    match view {
        View::Home => render_home(inv, actions)?,
        View::Status => render_status(inv, actions, data)?,
        View::Players(page) => render_players(inv, actions, data, *page, view)?,
        View::Logs(page) => render_logs(inv, actions, data, *page, view)?,
        View::Activity(page) => render_activity(inv, actions, data, *page, view)?,
        View::Settings => render_settings(inv, actions, &data["settings"])?,
        View::SelectDifficulty => render_select_difficulty(inv, actions, data)?,
        View::SelectGamemode => render_select_gamemode(inv, actions, data)?,
        View::Whitelist(page) => render_whitelist(inv, actions, data, *page, view)?,
        View::ConfirmRemoveWhitelist {
            name,
            player_id,
            page,
        } => render_confirm_remove_whitelist(inv, actions, name, player_id, *page)?,
        View::Snapshots(page) => render_snapshots(inv, actions, data, *page, view)?,
        View::Controls => render_controls(inv, actions)?,
        View::LifecycleConfirm(action) => render_lifecycle_confirm(inv, actions, *action)?,
        View::RestoreConfirm { generation, page } => {
            render_restore_confirm(inv, actions, *generation, *page)?;
        }
    }

    // Navigation buttons for sub-views
    if !matches!(view, View::Home) {
        button(
            inv,
            actions,
            45,
            "compass",
            "Home",
            "Return to server management",
            Some(Action::Open(View::Home)),
        );
        button(
            inv,
            actions,
            49,
            "clock",
            "Refresh",
            "Reload this page",
            Some(Action::Open(view.clone())),
        );
    }
    Ok(())
}

fn render_home(inv: &Inventory, actions: &mut HashMap<i16, Action>) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "pumpkin",
        "Kurbis • Server management",
        "Operator access • this server only",
        None,
    );
    button(
        inv,
        actions,
        10,
        "compass",
        "Status",
        "Current state and connection addresses",
        Some(Action::Open(View::Status)),
    );
    button(
        inv,
        actions,
        12,
        "player_head",
        "Players",
        "Online players on this server",
        Some(Action::Open(View::Players(0))),
    );
    button(
        inv,
        actions,
        14,
        "paper",
        "Logs",
        "Browse recent server console output",
        Some(Action::Open(View::Logs(0))),
    );
    button(
        inv,
        actions,
        16,
        "clock",
        "Activity",
        "Recent server and lifecycle events",
        Some(Action::Open(View::Activity(0))),
    );
    button(
        inv,
        actions,
        29,
        "redstone",
        "Settings",
        "Adjust difficulty, game mode, MOTD, whitelist, and render distance",
        Some(Action::Open(View::Settings)),
    );
    button(
        inv,
        actions,
        31,
        "iron_bars",
        "Whitelist",
        "Manage allowed players and toggle whitelist enforcement",
        Some(Action::Open(View::Whitelist(0))),
    );
    button(
        inv,
        actions,
        33,
        "chest",
        "Snapshots",
        "Browse snapshots and restore world backups",
        Some(Action::Open(View::Snapshots(0))),
    );
    button(
        inv,
        actions,
        39,
        "lever",
        "Server controls",
        "Start, stop, restart, or snapshot",
        Some(Action::Open(View::Controls)),
    );
    button(
        inv,
        actions,
        41,
        "map",
        "Browser login link",
        "Create a one-use link for this server",
        Some(Action::Login),
    );
    Ok(())
}

fn render_status(inv: &Inventory, actions: &mut HashMap<i16, Action>, data: &Value) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "pumpkin",
        "Kurbis • Status",
        "Server health and connectivity",
        None,
    );
    button(
        inv,
        actions,
        20,
        "nether_star",
        &format!("State: {}", value(data, &["lifecycle", "observed_state"])),
        &format!(
            "Desired: {}\nAddress: {}",
            value(data, &["lifecycle", "desired_state"]),
            value(data, &["java_address"])
        ),
        None,
    );
    button(
        inv,
        actions,
        22,
        "player_head",
        &format!("Players: {}", value(data, &["player_count"])),
        &format!("Server: {}", value(data, &["subdomain"])),
        None,
    );
    button(
        inv,
        actions,
        24,
        "chest",
        "Latest snapshot",
        &format!(
            "Generation: {}",
            value(data, &["latest_snapshot", "generation"])
        ),
        None,
    );
    Ok(())
}

fn render_players(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
    requested_page: usize,
    view: &View,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "player_head",
        "Kurbis • Players",
        "Connected players on this server",
        None,
    );
    let entries = data.as_array().context("players response was not a list")?;
    let page = current_page(entries.len(), requested_page);
    for (index, entry) in page_entries(entries, page).enumerate() {
        let name = value(entry, &["name"]);
        let edition = value(entry, &["edition"]);
        let slot = content_slot(index);
        button(
            inv,
            actions,
            slot,
            "player_head",
            &name,
            &format!("Edition: {edition}"),
            None,
        );
    }
    page_controls(inv, actions, view, page, entries.len());
    Ok(())
}

fn render_logs(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
    requested_page: usize,
    view: &View,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "paper",
        "Kurbis • Logs",
        "Recent server console logs",
        None,
    );
    let empty_vec = Vec::new();
    let entries = data["items"].as_array().unwrap_or(&empty_vec);
    let page = current_page(entries.len(), requested_page);
    for (index, entry) in page_entries(entries, page).enumerate() {
        let line = value(entry, &["line"]);
        let slot = content_slot(index);
        button(inv, actions, slot, "paper", &clip(&line, 40), &line, None);
    }
    page_controls(inv, actions, view, page, entries.len());
    Ok(())
}

fn render_activity(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
    requested_page: usize,
    view: &View,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "clock",
        "Kurbis • Activity",
        "Recent server activity and operations",
        None,
    );
    let empty_vec = Vec::new();
    let entries = data.as_array().unwrap_or(&empty_vec);
    let page = current_page(entries.len(), requested_page);
    for (index, entry) in page_entries(entries, page).enumerate() {
        let type_name = value(entry, &["type"]);
        let created_at = value(entry, &["created_at"]);
        let summary = value(entry, &["summary"]);
        let slot = content_slot(index);
        button(
            inv,
            actions,
            slot,
            "clock",
            &type_name,
            &format!("{created_at}\n{summary}"),
            None,
        );
    }
    page_controls(inv, actions, view, page, entries.len());
    Ok(())
}

fn render_settings(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    settings: &Value,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "redstone",
        "Kurbis • Settings",
        "Runtime settings apply on next server start",
        None,
    );
    button(
        inv,
        actions,
        10,
        "writable_book",
        "MOTD",
        &format!("{}\nClick to edit", value(settings, &["motd"])),
        Some(Action::PromptMotd),
    );
    button(
        inv,
        actions,
        19,
        "compass",
        "Difficulty",
        &format!(
            "Current: {}\nClick to choose options",
            value(settings, &["difficulty"])
        ),
        Some(Action::Open(View::SelectDifficulty)),
    );
    button(
        inv,
        actions,
        28,
        "grass_block",
        "Game mode",
        &format!(
            "Current: {}\nClick to choose options",
            value(settings, &["gamemode"])
        ),
        Some(Action::Open(View::SelectGamemode)),
    );
    setting_distance_button(
        inv,
        actions,
        SettingControl {
            slot: 37,
            material: "spyglass",
            label: "View distance",
            current: value(settings, &["view_distance"]),
            previous: Some(Action::AdjustSetting {
                key: "view_distance",
                direction: -1,
            }),
            next: Some(Action::AdjustSetting {
                key: "view_distance",
                direction: 1,
            }),
        },
    );
    setting_distance_button(
        inv,
        actions,
        SettingControl {
            slot: 41,
            material: "clock",
            label: "Simulation distance",
            current: value(settings, &["simulation_distance"]),
            previous: Some(Action::AdjustSetting {
                key: "simulation_distance",
                direction: -1,
            }),
            next: Some(Action::AdjustSetting {
                key: "simulation_distance",
                direction: 1,
            }),
        },
    );
    button(
        inv,
        actions,
        43,
        "name_tag",
        "Tab stats",
        &format!(
            "Current: {}\nClick to toggle",
            value(settings, &["tab_stats"])
        ),
        Some(Action::ToggleSetting { key: "tab_stats" }),
    );
    button(
        inv,
        actions,
        47,
        "redstone_torch",
        "Hardcore",
        &format!(
            "Current: {}\nClick to toggle",
            value(settings, &["hardcore"])
        ),
        Some(Action::ToggleSetting { key: "hardcore" }),
    );
    button(
        inv,
        actions,
        51,
        "iron_bars",
        "Whitelist",
        &format!(
            "Current: {}\nClick to toggle",
            value(settings, &["whitelist"])
        ),
        Some(Action::ToggleSetting { key: "whitelist" }),
    );
    Ok(())
}

fn render_select_difficulty(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "compass",
        "Select Difficulty",
        "Choose server difficulty level",
        None,
    );
    let current = value(data, &["settings", "difficulty"]);
    let options = [
        ("Peaceful", "feather", 20),
        ("Easy", "wooden_sword", 21),
        ("Normal", "iron_sword", 23),
        ("Hard", "diamond_sword", 24),
    ];
    for (name, mat, slot) in options {
        let is_selected = name.eq_ignore_ascii_case(&current);
        let title = if is_selected {
            format!("{name} §a[Selected]")
        } else {
            name.to_owned()
        };
        let desc = if is_selected {
            "Currently active difficulty\nClick to keep".to_owned()
        } else {
            format!("Click to set difficulty to {name}")
        };
        button(
            inv,
            actions,
            slot,
            mat,
            &title,
            &desc,
            Some(Action::SetSetting {
                key: "difficulty",
                value: name,
            }),
        );
    }
    button(
        inv,
        actions,
        45,
        "arrow",
        "Back to Settings",
        "Return to server settings",
        Some(Action::Open(View::Settings)),
    );
    Ok(())
}

fn render_select_gamemode(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "grass_block",
        "Select Game Mode",
        "Choose default player game mode",
        None,
    );
    let current = value(data, &["settings", "gamemode"]);
    let options = [
        ("Survival", "iron_pickaxe", 20),
        ("Creative", "grass_block", 21),
        ("Adventure", "map", 23),
        ("Spectator", "ender_eye", 24),
    ];
    for (name, mat, slot) in options {
        let is_selected = name.eq_ignore_ascii_case(&current);
        let title = if is_selected {
            format!("{name} §a[Selected]")
        } else {
            name.to_owned()
        };
        let desc = if is_selected {
            "Currently active game mode\nClick to keep".to_owned()
        } else {
            format!("Click to set game mode to {name}")
        };
        button(
            inv,
            actions,
            slot,
            mat,
            &title,
            &desc,
            Some(Action::SetSetting {
                key: "gamemode",
                value: name,
            }),
        );
    }
    button(
        inv,
        actions,
        45,
        "arrow",
        "Back to Settings",
        "Return to server settings",
        Some(Action::Open(View::Settings)),
    );
    Ok(())
}

fn render_whitelist(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
    requested_page: usize,
    view: &View,
) -> Result<()> {
    let empty_vec = Vec::new();
    let entries = data["entries"].as_array().unwrap_or(&empty_vec);
    let page = current_page(entries.len(), requested_page);
    let enabled = data["enabled"] == true;

    // Slot 2: Toggle Whitelist
    button(
        inv,
        actions,
        2,
        "lever",
        if enabled {
            "Whitelist: §aEnabled"
        } else {
            "Whitelist: §cDisabled"
        },
        "Click to toggle whitelist enforcement",
        Some(Action::ToggleSetting { key: "whitelist" }),
    );

    // Slot 4: Info header
    button(
        inv,
        actions,
        4,
        "iron_bars",
        "Kurbis • Whitelist",
        &format!(
            "{} entries • synced: {}",
            entries.len(),
            value(data, &["synchronized"])
        ),
        None,
    );

    // Slot 6: Add player button
    button(
        inv,
        actions,
        6,
        "emerald",
        "§aAdd player",
        "Click to add a player to the whitelist",
        Some(Action::PromptWhitelistAdd(page)),
    );

    // Render whitelisted players on content slots
    for (index, entry) in page_entries(entries, page).enumerate() {
        let name = value(entry, &["name"]);
        let player_id = value(entry, &["player_id"]);
        let slot = content_slot(index);
        button(
            inv,
            actions,
            slot,
            "player_head",
            &name,
            &format!("UUID: {player_id}\n§cClick to remove"),
            Some(Action::Open(View::ConfirmRemoveWhitelist {
                name: name.clone(),
                player_id,
                page,
            })),
        );
    }

    page_controls(inv, actions, view, page, entries.len());
    Ok(())
}

fn render_confirm_remove_whitelist(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    name: &str,
    player_id: &str,
    page: usize,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "iron_bars",
        "Remove Whitelist Entry",
        &format!("Player: {name}"),
        None,
    );
    button(
        inv,
        actions,
        22,
        "red_concrete",
        &format!("§cRemove {}", name),
        &format!("UUID: {player_id}\nClick to confirm removal"),
        Some(Action::ConfirmRemoveWhitelist {
            name: name.to_owned(),
            player_id: player_id.to_owned(),
            page,
        }),
    );
    button(
        inv,
        actions,
        26,
        "arrow",
        "Cancel",
        "Return to whitelist",
        Some(Action::Open(View::Whitelist(page))),
    );
    Ok(())
}

fn render_snapshots(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
    requested_page: usize,
    view: &View,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "chest",
        "Kurbis • Snapshots",
        "Click a snapshot to restore",
        None,
    );
    let empty_vec = Vec::new();
    let snapshots = data.as_array().unwrap_or(&empty_vec);
    let page = current_page(snapshots.len(), requested_page);
    for (index, snapshot) in page_entries(snapshots, page).enumerate() {
        let generation = snapshot["generation"].as_i64().unwrap_or(0);
        let reason = value(snapshot, &["reason"]);
        let size = value(snapshot, &["size_bytes"]);
        let slot = content_slot(index);
        button(
            inv,
            actions,
            slot,
            "chest",
            &format!("Generation {generation}"),
            &format!("{reason}\n{size} bytes\nClick to restore"),
            Some(Action::Open(View::RestoreConfirm { generation, page })),
        );
    }
    page_controls(inv, actions, view, page, snapshots.len());
    Ok(())
}

fn render_controls(inv: &Inventory, actions: &mut HashMap<i16, Action>) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "lever",
        "Kurbis • Server controls",
        "Execute server lifecycle operations",
        None,
    );
    for spec in actions::prompted_actions() {
        if let Some(slot) = spec.control_slot {
            button(
                inv,
                actions,
                slot,
                spec.material,
                spec.label,
                "Review and confirm this action",
                Some(Action::Open(View::LifecycleConfirm(spec.action))),
            );
        }
    }
    Ok(())
}

fn render_lifecycle_confirm(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    action: LifecycleAction,
) -> Result<()> {
    let spec = actions::spec(action);
    button(
        inv,
        actions,
        4,
        spec.material,
        &format!("Confirm {}", spec.command),
        "This action affects the server state",
        None,
    );
    button(
        inv,
        actions,
        22,
        spec.material,
        &format!("Confirm {}", spec.command),
        spec.confirmation,
        Some(Action::Execute(action)),
    );
    button(
        inv,
        actions,
        26,
        "arrow",
        "Cancel",
        "Return to server controls",
        Some(Action::Open(View::Controls)),
    );
    Ok(())
}

fn render_restore_confirm(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    generation: i64,
    page: usize,
) -> Result<()> {
    let restore = actions::spec(LifecycleAction::Restore);
    button(
        inv,
        actions,
        4,
        restore.material,
        "Confirm Restore",
        &format!("Target generation: {generation}"),
        None,
    );
    button(
        inv,
        actions,
        22,
        restore.material,
        restore.label,
        &format!("Replace current world with generation {generation}. Click to confirm."),
        Some(Action::Restore { generation, page }),
    );
    button(
        inv,
        actions,
        26,
        "arrow",
        "Cancel",
        "Return to snapshots",
        Some(Action::Open(View::Snapshots(page))),
    );
    Ok(())
}

struct SettingControl {
    slot: u32,
    material: &'static str,
    label: &'static str,
    current: String,
    previous: Option<Action>,
    next: Option<Action>,
}

fn setting_distance_button(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    control: SettingControl,
) {
    button(
        inv,
        actions,
        control.slot,
        control.material,
        control.label,
        &control.current,
        None,
    );
    if let Some(action) = control.previous {
        button(
            inv,
            actions,
            control.slot - 1,
            "arrow",
            "Decrease",
            "Choose a smaller distance",
            Some(action),
        );
    }
    if let Some(action) = control.next {
        button(
            inv,
            actions,
            control.slot + 1,
            "arrow",
            "Increase",
            "Choose a larger distance",
            Some(action),
        );
    }
}

fn view_title(view: &View) -> &'static str {
    match view {
        View::Home => "Kurbis • Server management",
        View::Status => "Kurbis • Status",
        View::Players(_) => "Kurbis • Players",
        View::Logs(_) => "Kurbis • Logs",
        View::Activity(_) => "Kurbis • Activity",
        View::Settings => "Kurbis • Settings",
        View::SelectDifficulty => "Kurbis • Select difficulty",
        View::SelectGamemode => "Kurbis • Select game mode",
        View::Whitelist(_) => "Kurbis • Whitelist",
        View::ConfirmRemoveWhitelist { .. } => "Kurbis • Remove whitelist entry",
        View::Snapshots(_) => "Kurbis • Snapshots",
        View::Controls => "Kurbis • Controls",
        View::LifecycleConfirm(_) => "Kurbis • Confirm action",
        View::RestoreConfirm { .. } => "Kurbis • Confirm restore",
    }
}

fn page_entries(entries: &[Value], page: usize) -> impl Iterator<Item = &Value> {
    entries
        .iter()
        .skip(page.saturating_mul(PAGE_SIZE))
        .take(PAGE_SIZE)
}

fn current_page(entry_count: usize, requested: usize) -> usize {
    let page_count = entry_count.div_ceil(PAGE_SIZE).max(1);
    requested.min(page_count - 1)
}

fn page_controls(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    view: &View,
    page: usize,
    total: usize,
) {
    let page_count = total.div_ceil(PAGE_SIZE).max(1);
    if page_count > 1 {
        button(
            inv,
            actions,
            46,
            "paper",
            &format!("Page {}/{}", page + 1, page_count),
            &format!("{total} entries total"),
            None,
        );
        if page > 0
            && let Some(previous) = view.with_page(page - 1)
        {
            button(
                inv,
                actions,
                47,
                "arrow",
                "Previous page",
                &format!("Go to page {}", page),
                Some(Action::Open(previous)),
            );
        }
        if page + 1 < page_count
            && let Some(next) = view.with_page(page + 1)
        {
            button(
                inv,
                actions,
                51,
                "arrow",
                "Next page",
                &format!("Go to page {}", page + 2),
                Some(Action::Open(next)),
            );
        }
    }
}

fn content_slot(index: usize) -> u32 {
    let row = index / CONTENT_COLS;
    let col = index % CONTENT_COLS;
    CONTENT_ROWS[row] + col as u32
}

fn button(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    slot: u32,
    material: &str,
    label: &str,
    description: &str,
    action: Option<Action>,
) {
    inv.set_item(slot, Some(item(material, label, description)));
    if let Some(action) = action {
        actions.insert(slot as i16, action);
    }
}

fn item(material: &str, label: &str, description: &str) -> ItemStack {
    let item = ItemStack::new(&format!("minecraft:{material}"), 1);
    item.set_custom_name(Some(TextComponent::from_legacy_string(&format!(
        "§6§l{label}"
    ))));
    let mut lore = Vec::new();
    for raw_line in description.lines().take(6) {
        lore.push(TextComponent::from_legacy_string(&format!(
            "§7{}",
            clip(raw_line, 46)
        )));
    }
    item.set_lore(lore);
    item
}

fn value(value: &Value, path: &[&str]) -> String {
    let mut current = value;
    for part in path {
        current = &current[*part];
    }
    match current {
        Value::Null => "—".into(),
        Value::String(text) => text.clone(),
        _ => current.to_string(),
    }
}

fn clip(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let clipped: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{clipped}…")
    } else {
        clipped
    }
}

pub(crate) struct Click;
impl EventHandler<InventoryClickEvent> for Click {
    fn handle(
        &self,
        server: Server,
        mut event: EventData<InventoryClickEvent>,
    ) -> EventData<InventoryClickEvent> {
        let key = player_key(&event.player);
        let action = {
            let state = lock_state();
            state
                .menus
                .get(&key)
                .and_then(|menu| menu.actions.get(&event.slot).cloned())
        };
        let Some(action) = action else {
            let state = lock_state();
            if state.menus.contains_key(&key) {
                event.cancelled = true;
            }
            return event;
        };

        event.cancelled = true;
        if !event.player.has_permission(PERMISSION) {
            return event;
        }

        if let Err(error) = activate(&event.player, &server, action) {
            tell(&event.player, &format!("Kurbis: {error:#}"));
        }
        event
    }
}

fn activate(player: &Player, server: &Server, action: Action) -> Result<()> {
    match action {
        Action::Open(view) => open(player, view)?,
        Action::Login => {
            let url = api::create_login_link()?;
            player.send_system_message(chat::server_login_link(&url), false);
        }
        Action::Execute(lifecycle_action) => {
            let action_name = actions::spec(lifecycle_action).command;
            let response = api::server_command(lifecycle_action, None)?;
            tell(
                player,
                &format!(
                    "{action_name} requested; current state: {}.",
                    value(&response, &["observed_state"])
                ),
            );
            open(player, View::Controls)?;
        }
        Action::Restore { generation, page } => {
            api::server_command(LifecycleAction::Restore, Some(generation))?;
            tell(
                player,
                &format!("Restore of snapshot {generation} requested."),
            );
            open(player, View::Snapshots(page))?;
        }
        Action::AdjustSetting { key, direction } => {
            api::adjust_setting(key, direction)?;
            tell(
                player,
                "Setting saved. Runtime configuration changes apply after the next server start.",
            );
            open(player, View::Settings)?;
        }
        Action::ToggleSetting { key } => {
            api::adjust_setting(key, 1)?;
            if key == "tab_stats" {
                crate::stats::refresh_now(server);
            }
            tell(
                player,
                if key == "whitelist" {
                    "Whitelist setting saved; live server synchronization is queued."
                } else {
                    "Setting saved. Runtime configuration changes apply after the next server start."
                },
            );
            open(
                player,
                if key == "whitelist" {
                    View::Whitelist(0)
                } else {
                    View::Settings
                },
            )?;
        }
        Action::SetSetting { key, value } => {
            api::update_setting(key, Value::String(value.to_owned()))?;
            tell(
                player,
                "Setting saved. Runtime configuration changes apply after the next server start.",
            );
            open(player, View::Settings)?;
        }
        Action::PromptMotd => {
            prompt_text_input(player, TextInputTarget::Motd)?;
        }
        Action::PromptWhitelistAdd(page) => {
            prompt_text_input(player, TextInputTarget::WhitelistAdd(page))?;
        }
        Action::ConfirmRemoveWhitelist {
            name,
            player_id,
            page,
        } => {
            api::remove_whitelist_entry(&player_id)?;
            tell(player, &format!("Removed {name} from the whitelist."));
            open(player, View::Whitelist(page))?;
        }
    }
    Ok(())
}

fn prompt_text_input(player: &Player, target: TextInputTarget) -> Result<()> {
    let key = player_key(player);
    lock_state().menus.remove(&key);

    if let Some(bedrock) = player.as_bedrock() {
        let (title, prompt, default) = match &target {
            TextInputTarget::Motd => {
                let data = page_data(&View::Settings)?;
                let current = value(&data, &["settings", "motd"]);
                ("Edit MOTD", "Server message of the day:", current)
            }
            TextInputTarget::WhitelistAdd(_) => {
                ("Add Whitelist Player", "Player username:", String::new())
            }
        };
        let form = CustomFormBuilder::new(TextComponent::text(title))
            .input(TextComponent::text(prompt), default.clone(), default)
            .build();
        let form_id = bedrock.open_form(form);
        lock_state().bedrock_forms.insert((key, form_id), target);
        return Ok(());
    }

    if let Some(java) = player.as_java() {
        java.send_packet(&ClientboundPacket::CCloseContainer(CCloseContainer {
            sync_id: 1,
        }));
    }
    lock_state().pending_text_inputs.insert(key, target.clone());

    match target {
        TextInputTarget::Motd => {
            let prompt = TextComponent::text("§6[Kurbis] §fClick to set MOTD: ")
                .add_child(
                    TextComponent::text("§b§n[/kurbis set motd <message>]")
                        .click_suggest_command("/kurbis set motd ")
                        .underlined(true),
                )
                .add_text(" §7or type in chat (type 'cancel' to abort).");
            player.send_system_message(prompt, false);
        }
        TextInputTarget::WhitelistAdd(_) => {
            let prompt = TextComponent::text("§6[Kurbis] §fClick to add player: ")
                .add_child(
                    TextComponent::text("§b§n[/kurbis whitelist add <name>]")
                        .click_suggest_command("/kurbis whitelist add ")
                        .underlined(true),
                )
                .add_text(" §7or type player name in chat (type 'cancel' to abort).");
            player.send_system_message(prompt, false);
        }
    }
    Ok(())
}

pub(crate) struct ChatInput;
impl EventHandler<PlayerChatEvent> for ChatInput {
    fn handle(
        &self,
        _server: Server,
        mut event: EventData<PlayerChatEvent>,
    ) -> EventData<PlayerChatEvent> {
        let key = player_key(&event.player);
        let target = {
            let mut state = lock_state();
            state.pending_text_inputs.remove(&key)
        };
        let Some(target) = target else {
            return event;
        };

        event.cancelled = true;
        let text = event.message.trim().to_owned();
        if text.eq_ignore_ascii_case("cancel") {
            tell(&event.player, "Cancelled.");
            let view = match target {
                TextInputTarget::Motd => View::Settings,
                TextInputTarget::WhitelistAdd(page) => View::Whitelist(page),
            };
            let _ = open(&event.player, view);
            return event;
        }

        match target {
            TextInputTarget::Motd => match api::update_setting("motd", Value::String(text)) {
                Ok(()) => {
                    tell(&event.player, "Updated server setting motd.");
                    let _ = open(&event.player, View::Settings);
                }
                Err(error) => {
                    tell(&event.player, &format!("Kurbis: {error:#}"));
                    let _ = open(&event.player, View::Settings);
                }
            },
            TextInputTarget::WhitelistAdd(page) => match api::add_whitelist_entry(&text) {
                Ok(()) => {
                    tell(&event.player, &format!("Added {text} to the whitelist."));
                    let _ = open(&event.player, View::Whitelist(page));
                }
                Err(error) => {
                    tell(&event.player, &format!("Kurbis: {error:#}"));
                    let _ = open(&event.player, View::Whitelist(page));
                }
            },
        }
        event
    }
}

pub(crate) struct BedrockForm;
impl EventHandler<BedrockFormResponseEvent> for BedrockForm {
    fn handle(
        &self,
        _server: Server,
        event: EventData<BedrockFormResponseEvent>,
    ) -> EventData<BedrockFormResponseEvent> {
        let key = player_key(&event.player);
        let target = {
            let mut state = lock_state();
            state.bedrock_forms.remove(&(key, event.form_id))
        };
        let Some(target) = target else {
            return event;
        };

        if let FormResponse::Custom(values) = FormResponse::parse(event.response_data.clone())
            && let Some(text) = values.first().and_then(Value::as_str)
        {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                match target {
                    TextInputTarget::Motd => {
                        if let Err(e) =
                            api::update_setting("motd", Value::String(trimmed.to_owned()))
                        {
                            tell(&event.player, &format!("Kurbis: {e:#}"));
                        } else {
                            tell(&event.player, "Updated server setting motd.");
                        }
                        let _ = open(&event.player, View::Settings);
                    }
                    TextInputTarget::WhitelistAdd(page) => {
                        if let Err(e) = api::add_whitelist_entry(trimmed) {
                            tell(&event.player, &format!("Kurbis: {e:#}"));
                        } else {
                            tell(&event.player, &format!("Added {trimmed} to the whitelist."));
                        }
                        let _ = open(&event.player, View::Whitelist(page));
                    }
                }
            }
        }
        event
    }
}

pub(crate) struct Close;
impl EventHandler<InventoryCloseEvent> for Close {
    fn handle(
        &self,
        _server: Server,
        event: EventData<InventoryCloseEvent>,
    ) -> EventData<InventoryCloseEvent> {
        let key = player_key(&event.player);
        lock_state().menus.remove(&key);
        event
    }
}

pub(crate) struct Drag;
impl EventHandler<InventoryDragEvent> for Drag {
    fn handle(
        &self,
        _server: Server,
        mut event: EventData<InventoryDragEvent>,
    ) -> EventData<InventoryDragEvent> {
        if lock_state().menus.contains_key(&player_key(&event.player)) {
            event.cancelled = true;
        }
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paged_lists_keep_every_entry_reachable_and_clamp_stale_pages() {
        let entries = (0..57)
            .map(|generation| serde_json::json!({"generation": generation}))
            .collect::<Vec<_>>();

        let first = page_entries(&entries, current_page(entries.len(), 0))
            .filter_map(|entry| entry["generation"].as_i64())
            .collect::<Vec<_>>();
        let middle = page_entries(&entries, current_page(entries.len(), 1))
            .filter_map(|entry| entry["generation"].as_i64())
            .collect::<Vec<_>>();
        let final_page = page_entries(&entries, current_page(entries.len(), 2))
            .filter_map(|entry| entry["generation"].as_i64())
            .collect::<Vec<_>>();
        let stale = current_page(entries.len(), usize::MAX);

        assert_eq!(first, (0..28).collect::<Vec<_>>());
        assert_eq!(middle, (28..56).collect::<Vec<_>>());
        assert_eq!(final_page, vec![56]);
        assert_eq!(stale, 2);
        assert_eq!(current_page(0, 0), 0);
    }

    #[test]
    fn content_slot_positions_are_within_valid_container_slots() {
        for index in 0..PAGE_SIZE {
            let slot = content_slot(index);
            assert!(
                slot < 54,
                "slot {slot} should be within generic 9x6 container"
            );
            assert!(
                !matches!(slot, 0..=8 | 45..=53),
                "slot {slot} should not overwrite header or footer row"
            );
        }
    }

    #[test]
    fn view_with_page_supports_all_paginated_views() {
        assert_eq!(View::Players(0).with_page(1), Some(View::Players(1)));
        assert_eq!(View::Logs(0).with_page(2), Some(View::Logs(2)));
        assert_eq!(View::Activity(0).with_page(3), Some(View::Activity(3)));
        assert_eq!(View::Whitelist(0).with_page(4), Some(View::Whitelist(4)));
        assert_eq!(View::Snapshots(0).with_page(5), Some(View::Snapshots(5)));
        assert_eq!(View::Home.with_page(1), None);
        assert_eq!(View::Settings.with_page(1), None);
    }
}

#[cfg(test)]
mod detailed_tests {
    use super::*;

    #[test]
    fn string_clipping_handles_short_exact_and_long_strings() {
        assert_eq!(clip("hello", 10), "hello");
        assert_eq!(clip("hello", 5), "hello");
        assert_eq!(clip("hello world", 5), "hello…");
        assert_eq!(clip("", 5), "");
    }

    #[test]
    fn value_extraction_resolves_nested_keys_and_fallbacks() {
        let json = serde_json::json!({
            "a": {
                "b": "found",
                "c": 42,
                "d": null
            }
        });
        assert_eq!(value(&json, &["a", "b"]), "found");
        assert_eq!(value(&json, &["a", "c"]), "42");
        assert_eq!(value(&json, &["a", "d"]), "—");
        assert_eq!(value(&json, &["a", "nonexistent"]), "—");
    }

    #[test]
    fn every_view_has_a_descriptive_title() {
        let views = [
            View::Home,
            View::Status,
            View::Players(0),
            View::Logs(0),
            View::Activity(0),
            View::Settings,
            View::SelectDifficulty,
            View::SelectGamemode,
            View::Whitelist(0),
            View::ConfirmRemoveWhitelist {
                name: "Steve".into(),
                player_id: "00000000-0000-0000-0000-000000000000".into(),
                page: 0,
            },
            View::Snapshots(0),
            View::Controls,
            View::LifecycleConfirm(LifecycleAction::Restart),
            View::RestoreConfirm {
                generation: 1,
                page: 0,
            },
        ];
        for view in views {
            let title = view_title(&view);
            assert!(
                title.starts_with("Kurbis • ") || title.starts_with("Select "),
                "title: {title}"
            );
        }
    }
}
