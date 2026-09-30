use super::*;
use pumpkin_plugin_api::{ItemStack, Screen, gui::Gui};

const CONTENT: [u32; 28] = [
    10, 11, 12, 13, 14, 15, 16, 19, 20, 21, 22, 23, 24, 25, 28, 29, 30, 31, 32, 33, 34, 37, 38, 39,
    40, 41, 42, 43,
];

fn item(material: &str, label: &str, description: &str) -> ItemStack {
    let item = ItemStack::new(&format!("minecraft:{material}"), 1);
    item.set_custom_name(Some(TextComponent::from_legacy_string(&format!(
        "§b§l{label}"
    ))));
    let mut lore = vec![];
    for line in description.lines().take(8) {
        let chars: Vec<char> = line.chars().collect();
        for part in chars.chunks(48).take(8) {
            lore.push(TextComponent::from_legacy_string(&format!(
                "§7{}",
                part.iter().collect::<String>()
            )));
        }
    }
    item.set_lore(lore);
    item
}
struct Menu {
    gui: Gui,
    actions: HashMap<i16, Action>,
}
impl Menu {
    fn button(
        &mut self,
        slot: u32,
        material: &str,
        label: &str,
        lore: &str,
        action: Option<Action>,
    ) {
        self.gui.set_item(slot, item(material, label, lore));
        if let Some(action) = action {
            self.actions.insert(slot as i16, action);
        }
    }
    fn pagination(&mut self, page: usize, total: usize, previous: View, next: View) {
        if page > 0 {
            self.button(
                46,
                "arrow",
                "Previous page",
                "Browse earlier entries",
                Some(Action::Open(previous)),
            );
        }
        if page.saturating_add(1).saturating_mul(28) < total {
            self.button(
                50,
                "arrow",
                "Next page",
                "Browse more entries",
                Some(Action::Open(next)),
            );
        }
    }
}
pub fn open(player: &Player, view: View) -> anyhow::Result<()> {
    anyhow::ensure!(
        player.has_permission(PERMISSION),
        "Operator access required"
    );
    let name = match &view {
        View::Home => "Workshop",
        View::Settings => "Settings",
        View::Plugins(_) => "Plugins",
        View::Sessions(_) => "Sessions",
        View::Jobs(_) => "Jobs",
        View::Session(_, _) => "Conversation",
        View::Job(_, _) => "Event log",
        View::Event(_, _, _) => "Event details",
    };
    let gui = Gui::new(
        Screen::Generic9x6,
        TextComponent::text(&format!("Vibe • {name}")),
    );
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    let mut menu = Menu {
        gui,
        actions: HashMap::new(),
    };
    for slot in 0..54 {
        menu.gui.set_item(
            slot,
            item(
                if !(9..45).contains(&slot) {
                    "purple_stained_glass_pane"
                } else {
                    "black_stained_glass_pane"
                },
                " ",
                "",
            ),
        );
    }
    menu.button(4, "nether_star", name, "Codex plugin workshop\nOperator access • official Pumpkin\nClick an icon; hover for details", None);
    menu.button(
        48,
        "compass",
        "Home",
        "Return to the workshop",
        Some(Action::Open(View::Home)),
    );
    menu.button(
        49,
        "clock",
        "Refresh",
        "Load current status and events",
        Some(Action::Open(view.clone())),
    );
    match view {
        View::Home => {
            menu.button(
                20,
                "chest",
                "Installed plugins",
                "View revisions and restore a previous version",
                Some(Action::Open(View::Plugins(0))),
            );
            menu.button(
                22,
                "written_book",
                "Conversations",
                "View prompts and responses\nContinue a saved Codex session",
                Some(Action::Open(View::Sessions(0))),
            );
            menu.button(
                24,
                "redstone",
                "Jobs & live logs",
                "Every command, change, message and error",
                Some(Action::Open(View::Jobs(0))),
            );
            menu.button(
                29,
                "writable_book",
                "Create or edit a plugin",
                "Enter plugin-name and your prompt in chat",
                Some(Action::Input(Input::Code)),
            );
            menu.button(
                31,
                "paper",
                "Ask Codex",
                "Discuss a plugin without installing changes",
                Some(Action::Input(Input::Chat)),
            );
            menu.button(
                33,
                "comparator",
                "Settings",
                "Model • reasoning • timeout • chat progress",
                Some(Action::Open(View::Settings)),
            );
            menu.button(
                51,
                "ender_eye",
                "Activation",
                "Installed changes activate after server restart",
                None,
            );
        }
        View::Settings => {
            let settings: Settings = read_json(&storage::path("settings.json"))?;
            menu.button(
                20,
                "name_tag",
                "Model",
                &format!(
                    "{}\nClick to enter a model ID\nType default to use CLI settings",
                    if settings.model.is_empty() {
                        "CLI default"
                    } else {
                        &settings.model
                    }
                ),
                Some(Action::Input(Input::Model)),
            );
            menu.button(
                22,
                "amethyst_shard",
                "Reasoning effort",
                &format!("{}\nClick to cycle effort", settings.reasoning),
                Some(Action::Reasoning),
            );
            menu.button(
                24,
                "clock",
                "Job timeout",
                &format!(
                    "{} seconds\nClick to cycle 5 / 15 / 30 / 60 minutes",
                    settings.timeout_seconds
                ),
                Some(Action::Timeout),
            );
            menu.button(
                31,
                if settings.chat_progress {
                    "lime_dye"
                } else {
                    "gray_dye"
                },
                "Live chat progress",
                &format!(
                    "{}\nFull disk logs are always retained",
                    if settings.chat_progress { "On" } else { "Off" }
                ),
                Some(Action::Verbose),
            );
            menu.button(
                33,
                "milk_bucket",
                "Reset settings",
                "Restore defaults; sessions and plugins stay saved",
                Some(Action::Reset),
            );
            menu.button(
                51,
                "book",
                "Settings apply to the next job",
                "Active jobs keep the settings they started with",
                None,
            );
        }
        View::Plugins(page) => {
            let index: PluginIndex = read_json(&storage::path("plugins.json"))?;
            for (slot, plugin) in CONTENT
                .iter()
                .zip(index.plugins.iter().skip(page.saturating_mul(28)).take(28))
            {
                menu.button(*slot, "chest", &plugin.name, &format!("Revision: {}\nSize: {} bytes\nClick to restore the previous revision\nRestart required to activate", plugin.revision, plugin.artifact_bytes), Some(Action::Revert(plugin.name.clone())));
            }
            menu.pagination(
                page,
                index.plugins.len(),
                View::Plugins(page.saturating_sub(1)),
                View::Plugins(page.saturating_add(1)),
            );
        }
        View::Sessions(page) => {
            let mut sessions: Sessions = read_json(&storage::path("sessions.json"))?;
            sessions
                .sessions
                .sort_by_key(|s| std::cmp::Reverse(s.updated_unix_ms));
            for (slot, session) in CONTENT.iter().zip(
                sessions
                    .sessions
                    .iter()
                    .skip(page.saturating_mul(28))
                    .take(28),
            ) {
                menu.button(
                    *slot,
                    "written_book",
                    &session.plugin,
                    &format!(
                        "{}\nSession: {}\n{} turns • {:?}\nClick to view conversation",
                        session.title,
                        session.id,
                        session.turns.len(),
                        session.action
                    ),
                    Some(Action::Open(View::Session(session.id, 0))),
                );
            }
            menu.pagination(
                page,
                sessions.sessions.len(),
                View::Sessions(page.saturating_sub(1)),
                View::Sessions(page.saturating_add(1)),
            );
        }
        View::Jobs(page) => {
            let jobs = storage::jobs()?;
            for (slot, job) in CONTENT
                .iter()
                .zip(jobs.iter().skip(page.saturating_mul(28)).take(28))
            {
                menu.button(
                    *slot,
                    "book",
                    &format!("{} • {:?}", job.plugin, job.state),
                    &format!("{}\n{}\nClick to inspect every event", job.id, job.message),
                    Some(Action::Open(View::Job(job.id, 0))),
                );
            }
            menu.pagination(
                page,
                jobs.len(),
                View::Jobs(page.saturating_sub(1)),
                View::Jobs(page.saturating_add(1)),
            );
        }
        View::Session(id, page) => {
            let sessions: Sessions = read_json(&storage::path("sessions.json"))?;
            let session = sessions
                .sessions
                .iter()
                .find(|s| s.id == id)
                .ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            for (slot, turn) in CONTENT
                .iter()
                .zip(session.turns.iter().skip(page.saturating_mul(28)).take(28))
            {
                menu.button(
                    *slot,
                    "writable_book",
                    "Prompt & response",
                    &format!(
                        "{}\nJob: {}\nClick to read Codex messages and tool steps",
                        turn.prompt, turn.job_id
                    ),
                    Some(Action::Open(View::Job(turn.job_id, 0))),
                );
            }
            if session.thread_id.is_some() {
                menu.button(
                    51,
                    "emerald",
                    "Continue conversation",
                    "Close the chest and enter the next prompt",
                    Some(Action::Input(Input::Continue(id))),
                );
            }
            menu.pagination(
                page,
                session.turns.len(),
                View::Session(id, page.saturating_sub(1)),
                View::Session(id, page.saturating_add(1)),
            );
        }
        View::Job(id, page) => {
            for (offset, (slot, event)) in CONTENT
                .iter()
                .zip(storage::event_page(id, page, 28)?)
                .enumerate()
            {
                menu.button(
                    *slot,
                    if event.kind == "stderr" {
                        "redstone"
                    } else {
                        "paper"
                    },
                    &format!("#{} {}", event.sequence, event.kind),
                    &event.message,
                    Some(Action::Open(View::Event(
                        id,
                        page.saturating_mul(28) + offset,
                        0,
                    ))),
                );
            }
            menu.pagination(
                page,
                read_json::<Progress>(&storage::path("progress").join(format!("{id}.json")))?
                    .total_events as usize,
                View::Job(id, page.saturating_sub(1)),
                View::Job(id, page.saturating_add(1)),
            );
            menu.button(
                51,
                "spyglass",
                "Watch in chat",
                "Stream new events privately to your chat",
                Some(Action::Watch(id)),
            );
            menu.button(
                52,
                "barrier",
                "Cancel this job",
                "Stop the active Codex process or queued job",
                Some(Action::Cancel(id)),
            );
        }
        View::Event(id, index, page) => {
            let event = storage::event_page(id, index, 1)?
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("Event not found"))?;
            let detail = format!(
                "{}\n\n{}",
                event.message,
                serde_json::to_string_pretty(&event.raw)?
            );
            let chars: Vec<char> = detail.replace('\n', " ").chars().collect();
            let chunks: Vec<String> = chars.chunks(384).map(|c| c.iter().collect()).collect();
            for (offset, (slot, chunk)) in CONTENT
                .iter()
                .zip(chunks.iter().skip(page.saturating_mul(28)).take(28))
                .enumerate()
            {
                menu.button(
                    *slot,
                    "paper",
                    &format!(
                        "#{} • Part {}",
                        event.sequence,
                        page.saturating_mul(28) + offset + 1
                    ),
                    chunk,
                    None,
                );
            }
            menu.pagination(
                page,
                chunks.len(),
                View::Event(id, index, page.saturating_sub(1)),
                View::Event(id, index, page.saturating_add(1)),
            );
            menu.button(
                51,
                "book",
                "Back to event log",
                "Return to this event's page",
                Some(Action::Open(View::Job(id, index / 28))),
            );
        }
    }
    // Opening a GUI closes the previous one. Publish state after that close callback.
    let id = key(player);
    let next_window = state()
        .lock()
        .unwrap()
        .windows
        .get(&id)
        .copied()
        .flatten()
        .map(|current| current % 100 + 1);
    player.open_gui(menu.gui);
    let mut state = state().lock().unwrap();
    state.windows.insert(id.clone(), next_window);
    state.menus.insert(id, menu.actions);
    Ok(())
}
