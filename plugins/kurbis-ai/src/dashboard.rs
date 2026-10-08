use crate::{PERMISSION, storage};
use kurbis_ai_protocol::{
    JobAction, PluginIndex, Progress, Sessions, Settings, now_ms, read_json, write_json,
};
use pumpkin_plugin_api::{
    Player, Server,
    events::{
        EventData, EventHandler, InventoryClickEvent, InventoryCloseEvent, InventoryDragEvent,
        PlayerChatEvent, PlayerLeaveEvent,
    },
    text::TextComponent,
};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};
use uuid::Uuid;

#[derive(Clone)]
pub enum View {
    Home,
    Settings,
    Plugins(usize),
    Plugin(String),
    Sessions(usize),
    Jobs(usize),
    Session(Uuid, usize),
    Job(Uuid, usize),
    Event(Uuid, usize, usize),
}
#[derive(Clone)]
enum Action {
    Open(View),
    Input(Input),
    Reasoning,
    Timeout,
    Verbose,
    Reset,
    Login,
    Cancel(Uuid),
    Watch(Uuid),
    Activate(JobAction, String),
}
#[derive(Clone)]
enum Input {
    Code,
    Ask,
    Continue(Uuid),
    Model,
}
#[derive(Default)]
struct State {
    menus: HashMap<String, HashMap<i16, Action>>,
    windows: HashMap<String, Option<i32>>,
    inputs: HashMap<String, (Input, u128)>,
    watches: HashMap<String, (Uuid, u64)>,
}
fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}
fn key(player: &Player) -> String {
    pumpkin_plugin_api::uuid::to_string(player.get_id())
}
fn tell(player: &Player, message: &str) {
    player.send_system_message(TextComponent::text(message), false);
}
pub fn watch(player: &Player, job: Uuid) {
    state()
        .lock()
        .unwrap()
        .watches
        .insert(key(player), (job, 0));
}
mod menu;
pub use menu::open;
pub struct Click;
impl EventHandler<InventoryClickEvent> for Click {
    fn handle(
        &self,
        _server: Server,
        mut event: EventData<InventoryClickEvent>,
    ) -> EventData<InventoryClickEvent> {
        let id = key(&event.player);
        let (ours, action) = {
            let state = state().lock().unwrap();
            (
                state.menus.contains_key(&id),
                state
                    .menus
                    .get(&id)
                    .and_then(|actions| actions.get(&event.raw_slot))
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
            && let Err(error) = act(&event.player, action)
        {
            tell(&event.player, &format!("Kurbis AI: {error}"));
        }
        event
    }
}
fn act(player: &Player, action: Action) -> anyhow::Result<()> {
    match action {
        Action::Open(view) => open(player, view)?,
        Action::Input(input) => {
            let instructions = match &input {
                Input::Code => "Close this chest and type: plugin-name your prompt",
                Input::Ask => "Close this chest and type your question or server action request",
                Input::Continue(_) => "Close this chest and type your next prompt",
                Input::Model => "Close this chest and type a model ID, or default",
            };
            state()
                .lock()
                .unwrap()
                .inputs
                .insert(key(player), (input, now_ms() + 120_000));
            tell(
                player,
                &format!(
                    "{instructions}. Type cancel to abort. Your next message is private (2 minute timeout)."
                ),
            );
        }
        Action::Watch(job) => {
            watch(player, job);
            tell(
                player,
                "Watching job events in chat. Toggle live progress in Settings.",
            );
        }
        Action::Login => {
            let operator_uuid =
                Uuid::parse_str(&pumpkin_plugin_api::uuid::to_string(player.get_id()))?;
            let id = storage::enqueue(
                JobAction::Login,
                "kurbis-ai".into(),
                String::new(),
                player.get_name(),
                Some(operator_uuid),
                None,
            )?;
            watch(player, id);
            tell(
                player,
                &format!(
                    "Codex device login queued: {id}. The URL and code will be sent only to you."
                ),
            );
        }
        Action::Cancel(job) => {
            storage::cancel(job)?;
            tell(player, "Cancellation requested.");
        }
        Action::Activate(action, plugin) => {
            let operator_uuid =
                Uuid::parse_str(&pumpkin_plugin_api::uuid::to_string(player.get_id()))?;
            let id = storage::enqueue(
                action,
                plugin,
                String::new(),
                player.get_name(),
                Some(operator_uuid),
                None,
            )?;
            watch(player, id);
            tell(player, &format!("Plugin activation queued: {id}."));
        }
        setting => {
            let mut settings: Settings = read_json(&storage::path("settings.json"))?;
            match setting {
                Action::Reasoning => {
                    let options = ["minimal", "low", "medium", "high", "xhigh"];
                    let index = options
                        .iter()
                        .position(|v| *v == settings.reasoning)
                        .unwrap_or(2);
                    settings.reasoning = options[(index + 1) % options.len()].into();
                }
                Action::Timeout => {
                    let options = [300, 900, 1800, 3600];
                    let index = options
                        .iter()
                        .position(|v| *v == settings.timeout_seconds)
                        .unwrap_or(1);
                    settings.timeout_seconds = options[(index + 1) % options.len()];
                }
                Action::Verbose => settings.chat_progress = !settings.chat_progress,
                Action::Reset => settings = Settings::default(),
                _ => unreachable!(),
            }
            settings.validate()?;
            write_json(&storage::path("settings.json"), &settings)?;
            open(player, View::Settings)?;
        }
    }
    Ok(())
}
pub struct Close;
impl EventHandler<InventoryCloseEvent> for Close {
    fn handle(
        &self,
        _server: Server,
        event: EventData<InventoryCloseEvent>,
    ) -> EventData<InventoryCloseEvent> {
        let id = key(&event.player);
        let mut state = state().lock().unwrap();
        state.menus.remove(&id);
        state.windows.remove(&id);
        event
    }
}
pub struct Drag;
impl EventHandler<InventoryDragEvent> for Drag {
    fn handle(
        &self,
        _server: Server,
        mut event: EventData<InventoryDragEvent>,
    ) -> EventData<InventoryDragEvent> {
        if state()
            .lock()
            .unwrap()
            .menus
            .contains_key(&key(&event.player))
        {
            event.cancelled = true;
        }
        event
    }
}
pub struct Chat;
impl EventHandler<PlayerChatEvent> for Chat {
    fn handle(
        &self,
        _server: Server,
        mut event: EventData<PlayerChatEvent>,
    ) -> EventData<PlayerChatEvent> {
        let input = state().lock().unwrap().inputs.remove(&key(&event.player));
        let Some((input, expires)) = input else {
            return event;
        };
        // Even an expired or deauthorized prompt is consumed privately.
        event.cancelled = true;
        if expires < now_ms() {
            tell(&event.player, "Prompt expired. Open /ai to try again.");
            return event;
        }
        if !event.player.has_permission(PERMISSION) {
            tell(&event.player, "Operator access required.");
            return event;
        }
        if event.message.trim() == "cancel" {
            tell(&event.player, "Prompt cancelled.");
            return event;
        }
        let result = submit(&event.player, input, &event.message);
        match result {
            Ok(Some(job)) => {
                watch(&event.player, job);
                tell(&event.player, &format!("Kurbis AI job queued: {job}"));
            }
            Ok(None) => tell(&event.player, "Model saved. Applies to the next job."),
            Err(error) => tell(&event.player, &format!("Kurbis AI: {error}")),
        }
        event
    }
}
fn submit(player: &Player, input: Input, message: &str) -> anyhow::Result<Option<Uuid>> {
    let name = player.get_name();
    let operator_uuid = Uuid::parse_str(&pumpkin_plugin_api::uuid::to_string(player.get_id()))?;
    match input {
        Input::Code => {
            let (plugin, prompt) = message
                .trim()
                .split_once(char::is_whitespace)
                .ok_or_else(|| anyhow::anyhow!("Use: plugin-name your prompt"))?;
            Ok(Some(storage::enqueue(
                JobAction::Code,
                plugin.into(),
                prompt.trim().into(),
                name,
                Some(operator_uuid),
                None,
            )?))
        }
        Input::Ask => Ok(Some(storage::enqueue(
            JobAction::Ask,
            "server-control".into(),
            message.trim().into(),
            name,
            Some(operator_uuid),
            None,
        )?)),
        Input::Continue(id) => Ok(Some(storage::continue_session(
            id,
            message.into(),
            name,
            Some(operator_uuid),
        )?)),
        Input::Model => {
            let mut settings: Settings = read_json(&storage::path("settings.json"))?;
            settings.model = if message.trim() == "default" {
                String::new()
            } else {
                message.trim().into()
            };
            settings.validate()?;
            write_json(&storage::path("settings.json"), &settings)?;
            Ok(None)
        }
    }
}
pub struct Leave;
impl EventHandler<PlayerLeaveEvent> for Leave {
    fn handle(
        &self,
        _server: Server,
        event: EventData<PlayerLeaveEvent>,
    ) -> EventData<PlayerLeaveEvent> {
        let key = key(&event.player);
        let mut state = state().lock().unwrap();
        state.menus.remove(&key);
        state.windows.remove(&key);
        state.inputs.remove(&key);
        state.watches.remove(&key);
        event
    }
}

fn poll_private_auth_messages(server: &Server) {
    let directory = storage::path("private/auth");
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten().take(8) {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let message = std::fs::read(&path).ok().and_then(|bytes| {
            serde_json::from_slice::<kurbis_ai_protocol::PrivateAuthMessage>(&bytes).ok()
        });
        let Some(message) = message else {
            let _ = std::fs::remove_file(path);
            continue;
        };
        if message.expires_unix_ms <= now_ms() {
            let _ = std::fs::remove_file(path);
            continue;
        }
        let expected_uuid = message.player_uuid.to_string();
        let Some(player) = server
            .get_all_players()
            .into_iter()
            .find(|player| pumpkin_plugin_api::uuid::to_string(player.get_id()) == expected_uuid)
        else {
            continue;
        };
        if !player.has_permission(PERMISSION) {
            continue;
        }
        let _ = std::fs::remove_file(path);
        match message.status {
            kurbis_ai_protocol::PrivateAuthStatus::CodeAvailable => {
                let (Some(url), Some(code)) = (message.verification_url, message.user_code) else {
                    tell(
                        &player,
                        "Kurbis AI could not read the device code. Authenticate on the controller host and retry.",
                    );
                    continue;
                };
                tell(
                    &player,
                    &format!("Codex device login (private): open {url} and enter code {code}."),
                );
            }
            kurbis_ai_protocol::PrivateAuthStatus::Completed => {
                tell(
                    &player,
                    "The shared server-owner Codex account is authenticated.",
                );
            }
            kurbis_ai_protocol::PrivateAuthStatus::Cancelled => {
                tell(&player, "Codex device login was cancelled.");
            }
            kurbis_ai_protocol::PrivateAuthStatus::Failed => {
                tell(
                    &player,
                    "Codex device login failed. Authenticate on the controller host and retry.",
                );
            }
        }
    }
}

pub fn poll(server: Server) {
    storage::reauthorize_pending(&server);
    poll_private_auth_messages(&server);
    let settings: Settings = match read_json(&storage::path("settings.json")) {
        Ok(s) => s,
        Err(_) => return,
    };
    if !settings.chat_progress {
        return;
    }
    for player in server.get_all_players() {
        if !player.has_permission(PERMISSION) {
            continue;
        }
        let key = key(&player);
        let watched = state().lock().unwrap().watches.get(&key).copied();
        let Some((job, mut sequence)) = watched else {
            continue;
        };
        let progress: Progress =
            match read_json(&storage::path("progress").join(format!("{job}.json"))) {
                Ok(p) => p,
                Err(_) => continue,
            };
        if let Some(first) = progress.events.first()
            && first.sequence > sequence + 1
        {
            tell(
                &player,
                "Earlier events are retained in the job log. Open /ai log to read them.",
            );
            sequence = first.sequence - 1;
        }
        for event in progress
            .events
            .iter()
            .filter(|e| e.sequence > sequence)
            .take(8)
        {
            tell(
                &player,
                &format!(
                    "[Kurbis AI #{}] {}: {}",
                    event.sequence,
                    event.kind,
                    event.message.chars().take(350).collect::<String>()
                ),
            );
        }
        let next = progress
            .events
            .iter()
            .filter(|e| e.sequence > sequence)
            .take(8)
            .last()
            .map_or(sequence, |e| e.sequence);
        state().lock().unwrap().watches.insert(key, (job, next));
    }
}
