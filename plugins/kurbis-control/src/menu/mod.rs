//! The `/kurbis` chest menu: views and their navigation, click actions, text
//! input and the shared item widgets. The pages themselves live in `server`
//! and `ai`.

mod ai;
mod server;

use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard, OnceLock},
};

use anyhow::Result;
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
use uuid::Uuid;

use crate::{
    PERMISSION,
    actions::{self, LifecycleAction},
    api, chat,
};

const CONTENT_ROWS: &[u32] = &[10, 19, 28, 37];
const CONTENT_COLS: usize = 7;
const PAGE_SIZE: usize = CONTENT_ROWS.len() * CONTENT_COLS; // 28
/// Characters per lore line; longer lines are clipped.
const LORE_WIDTH: usize = 46;
/// Lore lines an item shows.
const LORE_LINES: usize = 6;

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
    Ai,
    AiRequest {
        job: Uuid,
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
            Self::AiRequest { job, .. } => Some(Self::AiRequest { job: *job, page }),
            _ => None,
        }
    }

    /// The page the Back button returns to; Home has none.
    fn parent(&self) -> Option<Self> {
        match self {
            Self::Home => None,
            Self::Status
            | Self::Players(_)
            | Self::Logs(_)
            | Self::Activity(_)
            | Self::Settings
            | Self::Whitelist(_)
            | Self::Snapshots(_)
            | Self::Controls
            | Self::Ai => Some(Self::Home),
            Self::SelectDifficulty | Self::SelectGamemode => Some(Self::Settings),
            Self::ConfirmRemoveWhitelist { page, .. } => Some(Self::Whitelist(*page)),
            Self::LifecycleConfirm(_) => Some(Self::Controls),
            Self::RestoreConfirm { page, .. } => Some(Self::Snapshots(*page)),
            Self::AiRequest { .. } => Some(Self::Ai),
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
    Prompt(TextInputTarget),
    ConfirmRemoveWhitelist {
        name: String,
        player_id: String,
        page: usize,
    },
    CancelAiJob {
        job: Uuid,
        page: usize,
    },
    ShowAiOutcome {
        heading: String,
        outcome: String,
        failed: bool,
    },
}

/// Text the player types after closing the menu: in chat on Java, in a form
/// on Bedrock.
#[derive(Clone, Debug)]
enum TextInputTarget {
    Motd,
    WhitelistAdd(usize),
    AiPrompt { new_conversation: bool },
}

struct InputPrompt {
    /// The form's title, and what the chat prompt offers.
    title: &'static str,
    /// The form field's label.
    label: &'static str,
    /// The command the chat prompt suggests instead, and its argument.
    command: &'static str,
    argument: &'static str,
    /// What to type in chat.
    typed: &'static str,
}

impl TextInputTarget {
    fn prompt(&self) -> InputPrompt {
        let ask = |title, command| InputPrompt {
            title,
            label: "Question or instruction:",
            command,
            argument: "<question>",
            typed: "your question",
        };
        match self {
            Self::Motd => InputPrompt {
                title: "Edit MOTD",
                label: "Server message of the day:",
                command: "/kurbis set motd ",
                argument: "<message>",
                typed: "the message",
            },
            Self::WhitelistAdd(_) => InputPrompt {
                title: "Add a whitelist player",
                label: "Player username:",
                command: "/kurbis whitelist add ",
                argument: "<name>",
                typed: "the player name",
            },
            Self::AiPrompt {
                new_conversation: false,
            } => ask("Ask the AI assistant", "/ai ask "),
            Self::AiPrompt {
                new_conversation: true,
            } => ask("Start a new AI conversation", "/ai new "),
        }
    }

    /// The page the input was started from.
    fn view(&self) -> View {
        match self {
            Self::Motd => View::Settings,
            Self::WhitelistAdd(page) => View::Whitelist(*page),
            Self::AiPrompt { .. } => View::Ai,
        }
    }

    /// Apply the text and reopen the page, except after asking the AI: its
    /// progress and answer arrive in chat.
    fn submit(&self, player: &Player, text: &str) {
        let done = match self {
            Self::Motd => api::update_setting("motd", Value::String(text.to_owned()))
                .map(|()| Some("Updated server setting motd.".to_owned())),
            Self::WhitelistAdd(_) => api::add_whitelist_entry(text)
                .map(|()| Some(format!("Added {text} to the whitelist."))),
            Self::AiPrompt { new_conversation } => {
                crate::ai::ask(player, text, *new_conversation).map(|()| None)
            }
        };
        match done {
            Ok(Some(message)) => tell(player, &message),
            Ok(None) => {}
            Err(error) => tell(player, &format!("Kurbis: {error:#}")),
        }
        if !matches!(self, Self::AiPrompt { .. }) {
            let _ = open(player, self.view());
        }
    }
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
        View::Status | View::Settings | View::SelectDifficulty | View::SelectGamemode => {
            String::new()
        }
        View::Players(_) => "/players".into(),
        View::Logs(_) => "/logs?limit=100".into(),
        View::Activity(_) => "/activity?limit=50".into(),
        View::Whitelist(_) | View::ConfirmRemoveWhitelist { .. } => "/whitelist".into(),
        View::Snapshots(_) => "/snapshots".into(),
        View::Ai => "/ai".into(),
        View::AiRequest { job, .. } => format!("/ai/jobs/{job}"),
        View::Home | View::Controls | View::LifecycleConfirm(_) | View::RestoreConfirm { .. } => {
            return Ok(Value::Null);
        }
    };
    api::get(&api::server_path(&suffix)?)
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

    // Navigation comes first so it still works when a page fails to render.
    if let Some(parent) = view.parent() {
        let (material, label) = if parent == View::Home {
            ("compass", "Home")
        } else {
            ("arrow", "Back")
        };
        button(
            inv,
            actions,
            45,
            material,
            label,
            &format!("Return to {}", page_name(&parent)),
            Some(Action::Open(parent)),
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

    match view {
        View::Home => server::render_home(inv, actions)?,
        View::Status => server::render_status(inv, actions, data)?,
        View::Players(page) => server::render_players(inv, actions, data, *page, view)?,
        View::Logs(page) => server::render_logs(inv, actions, data, *page, view)?,
        View::Activity(page) => server::render_activity(inv, actions, data, *page, view)?,
        View::Settings => server::render_settings(inv, actions, &data["settings"])?,
        View::SelectDifficulty => server::render_select_difficulty(inv, actions, data)?,
        View::SelectGamemode => server::render_select_gamemode(inv, actions, data)?,
        View::Whitelist(page) => server::render_whitelist(inv, actions, data, *page, view)?,
        View::ConfirmRemoveWhitelist {
            name,
            player_id,
            page,
        } => server::render_confirm_remove_whitelist(inv, actions, name, player_id, *page)?,
        View::Snapshots(page) => server::render_snapshots(inv, actions, data, *page, view)?,
        View::Controls => server::render_controls(inv, actions)?,
        View::LifecycleConfirm(action) => {
            server::render_lifecycle_confirm(inv, actions, *action)?;
        }
        View::RestoreConfirm { generation, page } => {
            server::render_restore_confirm(inv, actions, *generation, *page)?;
        }
        View::Ai => ai::render_overview(inv, actions, data)?,
        View::AiRequest { page, .. } => ai::render_request(inv, actions, data, *page, view)?,
    }
    Ok(())
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
        View::Ai => "Kurbis • AI assistant",
        View::AiRequest { .. } => "Kurbis • AI request",
    }
}

/// A view's name in "Return to …".
fn page_name(view: &View) -> &'static str {
    let title = view_title(view);
    title.strip_prefix("Kurbis • ").unwrap_or(title)
}

fn page_entries<T>(entries: &[T], page: usize) -> impl Iterator<Item = &T> {
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

/// Slots for `count` buttons centred in a row, one empty slot apart.
fn row_slots(row: u32, count: usize) -> impl Iterator<Item = u32> {
    debug_assert!(count <= 5, "a row holds at most five spaced buttons");
    let first = 9 * row + 4 - count.saturating_sub(1) as u32;
    (0..count as u32).map(move |index| first + 2 * index)
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
    for raw_line in description.lines().take(LORE_LINES) {
        lore.push(TextComponent::from_legacy_string(&format!(
            "§7{}",
            clip(raw_line, LORE_WIDTH)
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

/// Word-wrap text into at most `max_lines` lines of `width` characters.
/// Words longer than a line fill it and continue on the next; the last line
/// ends in "…" when text was left out.
fn wrap(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for mut word in text.split_whitespace() {
        let mut length = word.chars().count();
        if used > 0 && used + 1 + length > width && length <= width {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        if used > 0 {
            if used + 1 < width {
                line.push(' ');
                used += 1;
            } else {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
        }
        while used + length > width {
            let fits = width - used;
            let (head, tail) = word.split_at(
                word.char_indices()
                    .nth(fits)
                    .map_or(word.len(), |(index, _)| index),
            );
            line.push_str(head);
            lines.push(std::mem::take(&mut line));
            used = 0;
            word = tail;
            length -= fits;
        }
        line.push_str(word);
        used += length;
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            let kept: String = last.chars().take(width - 1).collect();
            *last = format!("{kept}…");
        }
    }
    lines
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
        Action::Prompt(target) => prompt_text_input(player, target)?,
        Action::ConfirmRemoveWhitelist {
            name,
            player_id,
            page,
        } => {
            api::remove_whitelist_entry(&player_id)?;
            tell(player, &format!("Removed {name} from the whitelist."));
            open(player, View::Whitelist(page))?;
        }
        Action::CancelAiJob { job, page } => {
            crate::ai::cancel_job(job)?;
            open(player, View::AiRequest { job, page })?;
        }
        Action::ShowAiOutcome {
            heading,
            outcome,
            failed,
        } => ai::show_outcome(player, &heading, &outcome, failed),
    }
    Ok(())
}

fn prompt_text_input(player: &Player, target: TextInputTarget) -> Result<()> {
    let key = player_key(player);
    let prompt = target.prompt();

    if let Some(bedrock) = player.as_bedrock() {
        let current = match target {
            TextInputTarget::Motd => value(&page_data(&View::Settings)?, &["settings", "motd"]),
            TextInputTarget::WhitelistAdd(_) | TextInputTarget::AiPrompt { .. } => String::new(),
        };
        lock_state().menus.remove(&key);
        let form = CustomFormBuilder::new(TextComponent::text(prompt.title))
            .input(TextComponent::text(prompt.label), current.clone(), current)
            .build();
        let form_id = bedrock.open_form(form);
        lock_state().bedrock_forms.insert((key, form_id), target);
        return Ok(());
    }

    close_screen(player);
    lock_state().pending_text_inputs.insert(key, target);
    let message = TextComponent::text(&format!("§6[Kurbis] §f{}: click ", prompt.title))
        .add_child(
            TextComponent::text(&format!("§b§n[{}{}]", prompt.command, prompt.argument))
                .click_suggest_command(prompt.command)
                .underlined(true),
        )
        .add_text(&format!(
            " §7or type {} in chat (type 'cancel' to abort).",
            prompt.typed
        ));
    player.send_system_message(message, false);
    Ok(())
}

/// Close the menu so chat shows. Only Java clients can be told to close it;
/// on Bedrock a form takes its place.
fn close_screen(player: &Player) {
    lock_state().menus.remove(&player_key(player));
    if let Some(java) = player.as_java() {
        java.send_packet(&ClientboundPacket::CCloseContainer(CCloseContainer {
            sync_id: 1,
        }));
    }
}

/// Drop a pending chat prompt once the player runs a command instead,
/// usually the one the prompt suggested, so their next chat line is chat.
pub(crate) fn forget_text_input(player: &Player) {
    lock_state().pending_text_inputs.remove(&player_key(player));
}

pub(crate) struct ChatInput;
impl EventHandler<PlayerChatEvent> for ChatInput {
    fn handle(
        &self,
        _server: Server,
        mut event: EventData<PlayerChatEvent>,
    ) -> EventData<PlayerChatEvent> {
        let key = player_key(&event.player);
        let target = lock_state().pending_text_inputs.remove(&key);
        let Some(target) = target else {
            return event;
        };

        event.cancelled = true;
        let text = event.message.trim().to_owned();
        if text.eq_ignore_ascii_case("cancel") {
            tell(&event.player, "Cancelled.");
            let _ = open(&event.player, target.view());
        } else {
            target.submit(&event.player, &text);
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
        let target = lock_state().bedrock_forms.remove(&(key, event.form_id));
        let Some(target) = target else {
            return event;
        };

        if let FormResponse::Custom(values) = FormResponse::parse(event.response_data.clone())
            && let Some(text) = values.first().and_then(Value::as_str)
            && !text.trim().is_empty()
        {
            target.submit(&event.player, text.trim());
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

    fn every_view() -> Vec<View> {
        vec![
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
            View::Ai,
            View::AiRequest {
                job: Uuid::nil(),
                page: 0,
            },
        ]
    }

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
        let job = Uuid::nil();
        assert_eq!(View::Players(0).with_page(1), Some(View::Players(1)));
        assert_eq!(View::Logs(0).with_page(2), Some(View::Logs(2)));
        assert_eq!(View::Activity(0).with_page(3), Some(View::Activity(3)));
        assert_eq!(View::Whitelist(0).with_page(4), Some(View::Whitelist(4)));
        assert_eq!(View::Snapshots(0).with_page(5), Some(View::Snapshots(5)));
        assert_eq!(
            View::AiRequest { job, page: 0 }.with_page(2),
            Some(View::AiRequest { job, page: 2 })
        );
        assert_eq!(View::Home.with_page(1), None);
        assert_eq!(View::Settings.with_page(1), None);
        assert_eq!(View::Ai.with_page(1), None);
    }

    #[test]
    fn back_leads_from_every_view_to_home() {
        for view in every_view() {
            let mut current = view.clone();
            for _ in 0..2 {
                if let Some(parent) = current.parent() {
                    current = parent;
                }
            }
            assert_eq!(current, View::Home, "{view:?}");
        }
        let job = Uuid::nil();
        assert_eq!(View::AiRequest { job, page: 1 }.parent(), Some(View::Ai));
        assert_eq!(View::SelectGamemode.parent(), Some(View::Settings));
        assert_eq!(
            View::RestoreConfirm {
                generation: 3,
                page: 1
            }
            .parent(),
            Some(View::Snapshots(1))
        );
        assert_eq!(page_name(&View::Ai), "AI assistant");
    }

    #[test]
    fn every_view_has_a_descriptive_title() {
        for view in every_view() {
            let title = view_title(&view);
            assert!(title.starts_with("Kurbis • "), "title: {title}");
        }
    }

    #[test]
    fn text_inputs_suggest_their_command_and_return_to_their_page() {
        let ask = TextInputTarget::AiPrompt {
            new_conversation: false,
        };
        let fresh = TextInputTarget::AiPrompt {
            new_conversation: true,
        };
        assert_eq!(ask.prompt().command, "/ai ask ");
        assert_eq!(fresh.prompt().command, "/ai new ");
        assert_eq!(TextInputTarget::Motd.prompt().command, "/kurbis set motd ");
        assert_eq!(ask.view(), View::Ai);
        assert_eq!(TextInputTarget::Motd.view(), View::Settings);
        assert_eq!(TextInputTarget::WhitelistAdd(2).view(), View::Whitelist(2));
    }

    #[test]
    fn string_clipping_handles_short_exact_and_long_strings() {
        assert_eq!(clip("hello", 10), "hello");
        assert_eq!(clip("hello", 5), "hello");
        assert_eq!(clip("hello world", 5), "hello…");
        assert_eq!(clip("", 5), "");
    }

    #[test]
    fn wrapping_fills_lines_splits_long_words_and_marks_cut_text() {
        assert_eq!(
            wrap("the quick brown fox", 10, 3),
            vec!["the quick", "brown fox"]
        );
        assert_eq!(wrap("  spaced\n\nout  ", 10, 3), vec!["spaced out"]);
        assert_eq!(wrap("abcdefghijkl", 5, 3), vec!["abcde", "fghij", "kl"]);
        assert_eq!(wrap("élan vital", 4, 3), vec!["élan", "vita", "l"]);
        assert_eq!(wrap("one two three four", 9, 2), vec!["one two", "three…"]);
        assert!(wrap("", 10, 2).is_empty());
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
}
