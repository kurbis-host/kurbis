use crate::{
    dashboard::{self, View},
    storage,
};
use pumpkin_plugin_api::{
    Server,
    command::{
        Arg, ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs,
        StringType,
    },
    commands::CommandHandler,
    text::TextComponent,
};
use uuid::Uuid;
use vibe_protocol::{JobAction, Sessions, Settings, read_json, write_json};

pub fn command() -> Command {
    Command::new(&["vibe".into()], "Open the Codex workshop.")
        .execute(Handler("menu"))
        .then(CommandNode::literal("settings").execute(Handler("settings")))
        .then(CommandNode::literal("sessions").execute(Handler("sessions")))
        .then(CommandNode::literal("list").execute(Handler("list")))
        .then(CommandNode::literal("jobs").execute(Handler("jobs")))
        .then(with_prompt("code", "plugin"))
        .then(with_prompt("chat", "plugin"))
        .then(with_prompt("continue", "session"))
        .then(with_id("status", "job"))
        .then(with_id("cancel", "job"))
        .then(with_id("revert", "plugin"))
        .then(with_id("reload", "plugin"))
        .then(
            CommandNode::literal("log").then(
                string("job")
                    .execute(Handler("log"))
                    .then(string("page").execute(Handler("log"))),
            ),
        )
        .then(
            CommandNode::literal("set").then(
                string("key").then(
                    CommandNode::argument("value", &ArgumentType::String(StringType::Greedy))
                        .execute(Handler("set")),
                ),
            ),
        )
}
fn string(name: &str) -> CommandNode {
    CommandNode::argument(name, &ArgumentType::String(StringType::SingleWord))
}
fn with_id(name: &'static str, arg: &str) -> CommandNode {
    CommandNode::literal(name).then(string(arg).execute(Handler(name)))
}
fn with_prompt(name: &'static str, arg: &str) -> CommandNode {
    CommandNode::literal(name).then(
        string(arg).then(
            CommandNode::argument("prompt", &ArgumentType::String(StringType::Greedy))
                .execute(Handler(name)),
        ),
    )
}
fn arg(args: &ConsumedArgs, name: &str) -> anyhow::Result<String> {
    match args.get_value(name) {
        Arg::Simple(s) | Arg::Msg(s) => Ok(s),
        _ => anyhow::bail!("Missing {name}"),
    }
}
struct Handler(&'static str);
impl CommandHandler for Handler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let run = || -> anyhow::Result<()> {
            match self.0 {
                "menu" | "settings" | "jobs" => {
                    let player = sender
                        .as_player()
                        .ok_or_else(|| anyhow::anyhow!("Open this menu in game"))?;
                    let view = match self.0 {
                        "settings" => View::Settings,
                        "jobs" => View::Jobs(0),
                        _ => View::Home,
                    };
                    dashboard::open(&player, view)?;
                }
                "sessions" => {
                    if let Some(player) = sender.as_player() {
                        dashboard::open(&player, View::Sessions(0))?;
                    } else {
                        let sessions: Sessions = read_json(&storage::path("sessions.json"))?;
                        for session in sessions.sessions {
                            sender.send_message(TextComponent::text(&format!(
                                "{} | {} | {}",
                                session.id, session.plugin, session.title
                            )));
                        }
                    }
                }
                "code" | "chat" | "continue" | "revert" | "reload" => {
                    let prompt = if matches!(self.0, "revert" | "reload") {
                        String::new()
                    } else {
                        arg(&args, "prompt")?
                    };
                    let id = if self.0 == "continue" {
                        storage::continue_session(
                            Uuid::parse_str(&arg(&args, "session")?)?,
                            prompt,
                            sender.get_name(),
                        )?
                    } else {
                        let action = match self.0 {
                            "chat" => JobAction::Chat,
                            "revert" => JobAction::Revert,
                            "reload" => JobAction::Reload,
                            _ => JobAction::Code,
                        };
                        storage::enqueue(
                            action,
                            arg(&args, "plugin")?,
                            prompt,
                            sender.get_name(),
                            None,
                        )?
                    };
                    if let Some(player) = sender.as_player() {
                        dashboard::watch(&player, id);
                    }
                    sender.send_message(TextComponent::text(&format!("Vibe job queued: {id}")));
                }
                "status" => {
                    let id = Uuid::parse_str(&arg(&args, "job")?)?;
                    let job = storage::jobs()?
                        .into_iter()
                        .find(|j| j.id == id)
                        .ok_or_else(|| anyhow::anyhow!("Job not found"))?;
                    sender.send_message(TextComponent::text(&format!(
                        "{:?}: {}",
                        job.state, job.message
                    )));
                }
                "cancel" => storage::cancel(Uuid::parse_str(&arg(&args, "job")?)?)?,
                "list" => {
                    let index: vibe_protocol::PluginIndex =
                        read_json(&storage::path("plugins.json"))?;
                    for plugin in index.plugins {
                        sender.send_message(TextComponent::text(&format!(
                            "{} @ {} ({} bytes)",
                            plugin.name, plugin.revision, plugin.artifact_bytes
                        )));
                    }
                }
                "log" => {
                    let id = Uuid::parse_str(&arg(&args, "job")?)?;
                    let page = arg(&args, "page")
                        .ok()
                        .map(|s| s.parse::<usize>())
                        .transpose()?
                        .unwrap_or(0);
                    if let Some(player) = sender.as_player() {
                        dashboard::open(&player, View::Job(id, page))?;
                    } else {
                        for event in storage::event_page(id, page, 20)? {
                            sender.send_message(TextComponent::text(&format!(
                                "#{} {}: {}",
                                event.sequence, event.kind, event.message
                            )));
                        }
                    }
                }
                "set" => {
                    let mut settings: Settings = read_json(&storage::path("settings.json"))?;
                    match arg(&args, "key")?.as_str() {
                        "model" => {
                            settings.model = {
                                let value = arg(&args, "value")?;
                                if value.trim() == "default" {
                                    String::new()
                                } else {
                                    value.trim().into()
                                }
                            }
                        }
                        "reasoning" => settings.reasoning = arg(&args, "value")?,
                        "timeout" => settings.timeout_seconds = arg(&args, "value")?.parse()?,
                        "verbose" => settings.chat_progress = arg(&args, "value")?.parse()?,
                        _ => anyhow::bail!("Keys: model, reasoning, timeout, verbose"),
                    }
                    settings.validate()?;
                    write_json(&storage::path("settings.json"), &settings)?;
                    sender.send_message(TextComponent::text(
                        "Settings saved. Applies to the next job.",
                    ));
                }
                _ => unreachable!(),
            }
            Ok(())
        };
        run().map_err(|e| CommandError::CommandFailed(TextComponent::text(&e.to_string())))?;
        Ok(1)
    }
}
