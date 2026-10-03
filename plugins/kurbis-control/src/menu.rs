use std::{
    collections::{HashMap, HashSet},
    sync::{Mutex, MutexGuard, OnceLock},
};

use anyhow::{Context, Result};
use pumpkin_plugin_api::{
    ItemStack, Player, Screen, Server,
    events::{
        EventData, EventHandler, InventoryClickEvent, InventoryCloseEvent, InventoryDragEvent,
        PlayerEditBookEvent,
    },
    gui::Gui,
    text::TextComponent,
};
use serde_json::Value;

use crate::{
    PERMISSION,
    actions::{self, LifecycleAction},
    api, book, chat,
};

const PAGE_SIZE: usize = 18;

#[derive(Clone)]
pub(crate) enum View {
    Home,
    Status,
    Players(usize),
    Logs,
    Activity,
    Settings,
    Whitelist(usize),
    Snapshots(usize),
    Controls,
    LifecycleConfirm(LifecycleAction),
    RestoreConfirm { generation: i64, page: usize },
}

impl View {
    fn with_page(&self, page: usize) -> Option<Self> {
        match self {
            Self::Players(_) => Some(Self::Players(page)),
            Self::Whitelist(_) => Some(Self::Whitelist(page)),
            Self::Snapshots(_) => Some(Self::Snapshots(page)),
            _ => None,
        }
    }
}

#[derive(Clone)]
enum Action {
    Open(View),
    Execute(LifecycleAction),
    Login,
    Restore { generation: i64, page: usize },
    AdjustSetting { key: &'static str, direction: i8 },
    ToggleSetting { key: &'static str },
    EditMotd,
}

#[derive(Default)]
struct State {
    menus: HashMap<String, HashMap<i16, Action>>,
    opening: HashSet<String>,
    text_inputs: HashMap<String, (pumpkin_plugin_api::common::Hand, Option<ItemStack>)>,
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

fn close_menu(state: &mut State, key: &str) {
    if !state.opening.contains(key) {
        state.menus.remove(key);
    }
}

fn player_key(player: &Player) -> String {
    pumpkin_plugin_api::uuid::to_string(player.get_id())
}

fn tell(player: &Player, message: &str) {
    player.send_system_message(TextComponent::text(message), false);
}

pub(crate) fn page_data(view: &View) -> Result<Value> {
    let suffix = match view {
        View::Status | View::Settings => "",
        View::Players(_) => "/players",
        View::Logs => "/logs?limit=18",
        View::Activity => "/activity?limit=18",
        View::Whitelist(_) => "/whitelist",
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
    let title = view_title(&view);
    if matches!(view, View::Logs | View::Activity) {
        let data = page_data(&view)?;
        return book::open_pages(player, title, &book_pages(&view, &data));
    }
    let gui = Gui::new(Screen::Generic9x6, TextComponent::text(title));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    let mut actions = HashMap::new();
    for slot in 0..54 {
        gui.set_item(slot, item("black_stained_glass_pane", " ", ""));
    }
    button(
        &gui,
        &mut actions,
        4,
        "pumpkin",
        title,
        "Operator access • this server only",
        None,
    );

    match &view {
        View::Home => {
            button(
                &gui,
                &mut actions,
                10,
                "compass",
                "Status",
                "Current state and connection addresses",
                Some(Action::Open(View::Status)),
            );
            button(
                &gui,
                &mut actions,
                12,
                "player_head",
                "Players",
                "Online players on this server",
                Some(Action::Open(View::Players(0))),
            );
            button(
                &gui,
                &mut actions,
                14,
                "written_book",
                "Logs",
                "Recent server log lines",
                Some(Action::Open(View::Logs)),
            );
            button(
                &gui,
                &mut actions,
                16,
                "clock",
                "Activity",
                "Recent server and lifecycle activity",
                Some(Action::Open(View::Activity)),
            );
            button(
                &gui,
                &mut actions,
                29,
                "redstone",
                "Settings",
                "Edit server settings with buttons and a writable book",
                Some(Action::Open(View::Settings)),
            );
            button(
                &gui,
                &mut actions,
                31,
                "iron_bars",
                "Whitelist",
                "View entries; edit with whitelist commands",
                Some(Action::Open(View::Whitelist(0))),
            );
            button(
                &gui,
                &mut actions,
                33,
                "chest",
                "Snapshots",
                "Browse snapshots and confirm a restore",
                Some(Action::Open(View::Snapshots(0))),
            );
            button(
                &gui,
                &mut actions,
                39,
                "lever",
                "Server controls",
                "Start, stop, restart, or snapshot",
                Some(Action::Open(View::Controls)),
            );
            button(
                &gui,
                &mut actions,
                41,
                "map",
                "Browser login link",
                "Create a one-use link for this server",
                Some(Action::Login),
            );
        }
        View::Status
        | View::Settings
        | View::Players(_)
        | View::Logs
        | View::Activity
        | View::Whitelist(_)
        | View::Snapshots(_) => {
            let data = page_data(&view)?;
            render_page(&gui, &mut actions, &view, &data)?;
        }
        View::Controls => {
            for spec in actions::prompted_actions() {
                button(
                    &gui,
                    &mut actions,
                    spec.control_slot
                        .expect("prompted actions have a control slot"),
                    spec.material,
                    spec.label,
                    "Review and confirm this action",
                    Some(Action::Open(View::LifecycleConfirm(spec.action))),
                );
            }
        }
        View::LifecycleConfirm(action) => {
            let spec = actions::spec(*action);
            button(
                &gui,
                &mut actions,
                22,
                spec.material,
                &format!("Confirm {}", spec.command),
                spec.confirmation,
                Some(Action::Execute(*action)),
            );
            button(
                &gui,
                &mut actions,
                26,
                "arrow",
                "Cancel",
                "Return to server controls",
                Some(Action::Open(View::Controls)),
            );
        }
        View::RestoreConfirm { generation, page } => {
            let restore = actions::spec(LifecycleAction::Restore);
            button(
                &gui,
                &mut actions,
                22,
                restore.material,
                restore.label,
                &format!(
                    "Replace the current world with generation {generation}. Click to confirm."
                ),
                Some(Action::Restore {
                    generation: *generation,
                    page: *page,
                }),
            );
            button(
                &gui,
                &mut actions,
                26,
                "arrow",
                "Cancel",
                "Return to snapshots",
                Some(Action::Open(View::Snapshots(*page))),
            );
        }
    }

    button(
        &gui,
        &mut actions,
        45,
        "compass",
        "Home",
        "Return to server management",
        Some(Action::Open(View::Home)),
    );
    button(
        &gui,
        &mut actions,
        49,
        "clock",
        "Refresh",
        "Reload this page",
        Some(Action::Open(view.clone())),
    );
    let key = player_key(player);
    {
        let mut state = lock_state();
        state.opening.insert(key.clone());
        state.menus.insert(key.clone(), actions);
    }
    player.open_gui(gui);
    lock_state().opening.remove(&key);
    Ok(())
}

fn render_page(
    gui: &Gui,
    actions: &mut HashMap<i16, Action>,
    view: &View,
    data: &Value,
) -> Result<()> {
    match view {
        View::Status => {
            button(
                gui,
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
                gui,
                actions,
                22,
                "player_head",
                &format!("Players: {}", value(data, &["player_count"])),
                &format!("Server: {}", value(data, &["subdomain"])),
                None,
            );
            button(
                gui,
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
        }
        View::Settings => render_settings(gui, actions, &data["settings"]),
        View::Players(requested_page) => {
            let entries = data.as_array().context("players response was not a list")?;
            let page = current_page(entries.len(), *requested_page);
            render_list(gui, entries, "player_head", page, |entry| {
                format!(
                    "{} • {}",
                    value(entry, &["name"]),
                    value(entry, &["edition"])
                )
            });
            page_controls(gui, actions, view, page, entries.len());
        }
        View::Whitelist(requested_page) => {
            let entries = data["entries"]
                .as_array()
                .context("whitelist response omitted entries")?;
            let page = current_page(entries.len(), *requested_page);
            button(
                gui,
                actions,
                4,
                "iron_bars",
                if data["enabled"] == true {
                    "Whitelist enabled"
                } else {
                    "Whitelist disabled"
                },
                &format!(
                    "{} entries • synced: {}",
                    entries.len(),
                    value(data, &["synchronized"])
                ),
                None,
            );
            render_list(gui, entries, "name_tag", page, |entry| {
                format!(
                    "{} • {}",
                    value(entry, &["name"]),
                    value(entry, &["player_id"])
                )
            });
            page_controls(gui, actions, view, page, entries.len());
        }
        View::Snapshots(requested_page) => {
            let snapshots = data
                .as_array()
                .context("snapshots response was not a list")?;
            let page = current_page(snapshots.len(), *requested_page);
            render_list(gui, snapshots, "chest", page, |entry| {
                format!(
                    "Generation {} • {} • {} bytes",
                    value(entry, &["generation"]),
                    value(entry, &["reason"]),
                    value(entry, &["size_bytes"])
                )
            });
            page_controls(gui, actions, view, page, snapshots.len());
            for (index, snapshot) in page_entries(snapshots, page).enumerate() {
                if let Some(generation) = snapshot["generation"].as_i64() {
                    actions.insert(
                        content_slot(index) as i16,
                        Action::Open(View::RestoreConfirm { generation, page }),
                    );
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn view_title(view: &View) -> &'static str {
    match view {
        View::Home => "Kurbis • Server management",
        View::Status => "Kurbis • Status",
        View::Players(_) => "Kurbis • Players",
        View::Logs => "Kurbis • Logs",
        View::Activity => "Kurbis • Activity",
        View::Settings => "Kurbis • Settings",
        View::Whitelist(_) => "Kurbis • Whitelist",
        View::Snapshots(_) => "Kurbis • Snapshots",
        View::Controls => "Kurbis • Controls",
        View::LifecycleConfirm(_) => "Kurbis • Confirm action",
        View::RestoreConfirm { .. } => "Kurbis • Confirm restore",
    }
}

fn book_pages(view: &View, data: &Value) -> Vec<String> {
    match view {
        View::Logs => data["items"]
            .as_array()
            .map(|items| items.iter().map(|entry| value(entry, &["line"])).collect())
            .unwrap_or_else(|| vec!["Logs response omitted items.".to_owned()]),
        View::Activity => data
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|entry| {
                        format!(
                            "{}\n{}\n{}",
                            value(entry, &["type"]),
                            value(entry, &["created_at"]),
                            value(entry, &["summary"]),
                        )
                    })
                    .collect()
            })
            .unwrap_or_else(|| vec!["Activity response was not a list.".to_owned()]),
        _ => vec!["No entries.".to_owned()],
    }
}

fn render_settings(gui: &Gui, actions: &mut HashMap<i16, Action>, settings: &Value) {
    button(
        gui,
        actions,
        10,
        "writable_book",
        "MOTD",
        &value(settings, &["motd"]),
        Some(Action::EditMotd),
    );
    setting_button(gui, actions, SettingControl {
        slot: 19,
        material: "compass",
        label: "Difficulty",
        current: value(settings, &["difficulty"]),
        previous: Some(Action::AdjustSetting { key: "difficulty", direction: -1 }),
        next: Some(Action::AdjustSetting { key: "difficulty", direction: 1 }),
    });
    setting_button(gui, actions, SettingControl {
        slot: 28,
        material: "grass_block",
        label: "Game mode",
        current: value(settings, &["gamemode"]),
        previous: Some(Action::AdjustSetting { key: "gamemode", direction: -1 }),
        next: Some(Action::AdjustSetting { key: "gamemode", direction: 1 }),
    });
    setting_button(gui, actions, SettingControl {
        slot: 37,
        material: "spyglass",
        label: "View distance",
        current: value(settings, &["view_distance"]),
        previous: Some(Action::AdjustSetting { key: "view_distance", direction: -1 }),
        next: Some(Action::AdjustSetting { key: "view_distance", direction: 1 }),
    });
    setting_button(gui, actions, SettingControl {
        slot: 41,
        material: "clock",
        label: "Simulation distance",
        current: value(settings, &["simulation_distance"]),
        previous: Some(Action::AdjustSetting { key: "simulation_distance", direction: -1 }),
        next: Some(Action::AdjustSetting { key: "simulation_distance", direction: 1 }),
    });
    setting_button(gui, actions, SettingControl {
        slot: 47,
        material: "redstone_torch",
        label: "Hardcore",
        current: value(settings, &["hardcore"]),
        previous: None,
        next: Some(Action::ToggleSetting { key: "hardcore" }),
    });
    setting_button(gui, actions, SettingControl {
        slot: 51,
        material: "iron_bars",
        label: "Whitelist",
        current: value(settings, &["whitelist"]),
        previous: None,
        next: Some(Action::ToggleSetting { key: "whitelist" }),
    });
}

struct SettingControl {
    slot: u32,
    material: &'static str,
    label: &'static str,
    current: String,
    previous: Option<Action>,
    next: Option<Action>,
}

fn setting_button(gui: &Gui, actions: &mut HashMap<i16, Action>, control: SettingControl) {
    button(
        gui,
        actions,
        control.slot,
        control.material,
        control.label,
        &control.current,
        None,
    );
    if let Some(action) = control.previous {
        button(
            gui,
            actions,
            control.slot - 1,
            "arrow",
            "Previous",
            "Choose the previous value",
            Some(action),
        );
    }
    if let Some(action) = control.next {
        button(
            gui,
            actions,
            control.slot + 1,
            "arrow",
            "Next",
            "Choose the next value",
            Some(action),
        );
    }
}

fn render_list<F>(gui: &Gui, entries: &[Value], material: &str, page: usize, mut title: F)
where
    F: FnMut(&Value) -> String,
{
    for (index, entry) in page_entries(entries, page).enumerate() {
        let summary = title(entry);
        let slot = content_slot(index);
        gui.set_item(
            slot,
            item(material, &clip(&summary, 45), &clip(&summary, 400)),
        );
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
    gui: &Gui,
    actions: &mut HashMap<i16, Action>,
    view: &View,
    page: usize,
    total: usize,
) {
    let page_count = total.div_ceil(PAGE_SIZE).max(1);
    if page_count == 1 {
        return;
    }
    button(
        gui,
        actions,
        46,
        "paper",
        &format!("Page {}/{}", page + 1, page_count),
        &format!("{total} entries"),
        None,
    );
    if page > 0
        && let Some(previous) = view.with_page(page - 1)
    {
        button(
            gui,
            actions,
            47,
            "arrow",
            "Previous page",
            &format!("Page {}", page),
            Some(Action::Open(previous)),
        );
    }
    if page + 1 < page_count
        && let Some(next) = view.with_page(page + 1)
    {
        button(
            gui,
            actions,
            51,
            "arrow",
            "Next page",
            &format!("Page {}", page + 2),
            Some(Action::Open(next)),
        );
    }
}

fn content_slot(index: usize) -> u32 {
    let rows = [10, 19, 28];
    rows[index / 6] + (index % 6) as u32
}

fn button(
    gui: &Gui,
    actions: &mut HashMap<i16, Action>,
    slot: u32,
    material: &str,
    label: &str,
    description: &str,
    action: Option<Action>,
) {
    gui.set_item(slot, item(material, label, description));
    if let Some(action) = action {
        actions.insert(slot as i16, action);
    }
}

fn item(material: &str, label: &str, description: &str) -> ItemStack {
    let item = ItemStack::new(&format!("minecraft:{material}"), 1);
    item.set_custom_name(Some(TextComponent::from_legacy_string(&format!(
        "§6§l{label}"
    ))));
    let lore = description
        .lines()
        .take(6)
        .map(|line| TextComponent::from_legacy_string(&format!("§7{}", clip(line, 46))))
        .collect();
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
        _server: Server,
        mut event: EventData<InventoryClickEvent>,
    ) -> EventData<InventoryClickEvent> {
        let key = player_key(&event.player);
        let (ours, action) = {
            let state = lock_state();
            (
                state.menus.contains_key(&key),
                state
                    .menus
                    .get(&key)
                    .and_then(|items| items.get(&event.raw_slot))
                    .cloned(),
            )
        };
        if !ours {
            return event;
        }
        event.cancelled = true;
        if !event.player.has_permission(PERMISSION) {
            return event;
        }
        if let Some(action) = action
            && let Err(error) = activate(&event.player, action)
        {
            tell(&event.player, &format!("Kurbis: {error:#}"));
        }
        event
    }
}

fn activate(player: &Player, action: Action) -> Result<()> {
    match action {
        Action::Open(view) => open(player, view)?,
        Action::Login => {
            let url = api::create_login_link()?;
            player.send_system_message(chat::server_login_link(&url), false);
        }
        Action::Execute(lifecycle_action) => {
            let action = actions::spec(lifecycle_action).command;
            let response = api::server_command(lifecycle_action, None)?;
            tell(
                player,
                &format!(
                    "{action} requested; current state: {}.",
                    value(&response, &["observed_state"])
                ),
            );
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
            open(player, View::Settings)?;
        }
        Action::ToggleSetting { key } => {
            api::adjust_setting(key, 1)?;
            open(player, View::Settings)?;
        }
        Action::EditMotd => {
            let data = page_data(&View::Settings)?;
            let current = value(&data, &["settings", "motd"]);
            let input = book::open_text_input(player, &current)?;
            lock_state().text_inputs.insert(player_key(player), input);
        }
    }
    Ok(())
}

pub(crate) struct EditBook;
impl EventHandler<PlayerEditBookEvent> for EditBook {
    fn handle(
        &self,
        _server: Server,
        mut event: EventData<PlayerEditBookEvent>,
    ) -> EventData<PlayerEditBookEvent> {
        let key = player_key(&event.player);
        let input = lock_state().text_inputs.remove(&key);
        let Some((hand, original)) = input else {
            return event;
        };

        event.cancelled = true;
        let text = event
            .pages
            .first()
            .map_or_else(String::new, |page| page.trim().to_owned());
        let updated = match api::update_setting("motd", Value::String(text)) {
            Ok(()) => {
                tell(&event.player, "Updated server setting motd.");
                true
            }
            Err(error) => {
                tell(&event.player, &format!("Kurbis: {error:#}"));
                false
            }
        };
        event.player.set_item_in_hand(hand, original);
        if updated {
            let _ = open(&event.player, View::Settings);
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
        close_menu(&mut lock_state(), &key);
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
    fn closing_the_previous_inventory_keeps_the_replacement_menu_actions() {
        let key = "player";
        let mut state = State::default();
        state.opening.insert(key.into());
        state
            .menus
            .insert(key.into(), HashMap::from([(41, Action::Login)]));

        close_menu(&mut state, key);

        assert!(matches!(state.menus[key].get(&41), Some(Action::Login)));

        state.opening.remove(key);
        close_menu(&mut state, key);

        assert!(!state.menus.contains_key(key));
    }

    #[test]
    fn paged_lists_keep_every_entry_reachable_and_clamp_stale_pages() {
        let entries = (0..37)
            .map(|generation| serde_json::json!({"generation":generation}))
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

        assert_eq!(first, (0..18).collect::<Vec<_>>());
        assert_eq!(middle, (18..36).collect::<Vec<_>>());
        assert_eq!(final_page, vec![36]);
        assert_eq!(stale, 2);
        assert_eq!(current_page(0, 0), 0);
    }
}
