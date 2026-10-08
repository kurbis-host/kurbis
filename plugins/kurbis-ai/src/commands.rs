use crate::{
    dashboard::{self, View},
    storage,
};
use kurbis_ai_protocol::{JobAction, Sessions, Settings, read_json, write_json};
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

pub fn command() -> Command {
    Command::new(&["ai".into()], "Open Kurbis AI.")
        .execute(Handler("menu"))
        .then(CommandNode::literal("settings").execute(Handler("settings")))
        .then(CommandNode::literal("sessions").execute(Handler("sessions")))
        .then(CommandNode::literal("list").execute(Handler("list")))
        .then(CommandNode::literal("jobs").execute(Handler("jobs")))
        .then(with_prompt("code", "plugin"))
        .then(with_greedy_prompt("ask"))
        .then(with_prompt("continue", "session"))
        .then(CommandNode::literal("login").execute(Handler("login")))
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
fn with_greedy_prompt(name: &'static str) -> CommandNode {
    CommandNode::literal(name).then(
        CommandNode::argument("prompt", &ArgumentType::String(StringType::Greedy))
            .execute(Handler(name)),
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
                "code" | "ask" | "continue" | "revert" | "reload" | "login" => {
                    let prompt = if matches!(self.0, "revert" | "reload" | "login") {
                        String::new()
                    } else {
                        arg(&args, "prompt")?
                    };
                    let player = sender
                        .as_player()
                        .ok_or_else(|| anyhow::anyhow!("Run this action in game"))?;
                    let operator_uuid =
                        Uuid::parse_str(&pumpkin_plugin_api::uuid::to_string(player.get_id()))?;
                    let id = if self.0 == "continue" {
                        storage::continue_session(
                            Uuid::parse_str(&arg(&args, "session")?)?,
                            prompt,
                            player.get_name(),
                            Some(operator_uuid),
                        )?
                    } else {
                        let (action, plugin) = match self.0 {
                            "ask" => (JobAction::Ask, "server-control".to_owned()),
                            "login" => (JobAction::Login, "kurbis-ai".to_owned()),
                            "revert" => (JobAction::Revert, arg(&args, "plugin")?),
                            "reload" => (JobAction::Reload, arg(&args, "plugin")?),
                            _ => (JobAction::Code, arg(&args, "plugin")?),
                        };
                        storage::enqueue(
                            action,
                            plugin,
                            prompt,
                            player.get_name(),
                            Some(operator_uuid),
                            None,
                        )?
                    };
                    dashboard::watch(&player, id);
                    let response = if self.0 == "login" {
                        format!("Codex device login queued: {id}. Details will be sent privately.")
                    } else {
                        format!("Kurbis AI job queued: {id}")
                    };
                    sender.send_message(TextComponent::text(&response));
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
                    let index: kurbis_ai_protocol::PluginIndex =
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
