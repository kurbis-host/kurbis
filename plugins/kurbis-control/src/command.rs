use anyhow::{Result, bail};
use pumpkin_plugin_api::{
    Server,
    command::{
        Arg, ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs,
        StringType,
    },
    commands::CommandHandler,
    text::TextComponent,
};
use serde_json::Value;

use crate::{
    actions, api, chat,
    menu::{self, View},
    stats,
};

pub(crate) fn command() -> Command {
    let mut command = Command::new(&["kurbis".into()], "Manage this Kurbis server.")
        .execute(Handler("menu"))
        .then(CommandNode::literal("login").execute(Handler("login")))
        .then(view("status"))
        .then(view("players"))
        .then(view("logs"))
        .then(view("activity"))
        .then(view("settings"))
        .then(view("whitelist"))
        .then(view("snapshots"))
        .then(settings_command())
        .then(whitelist_command())
        .then(restore_command());
    for action in actions::prompted_actions() {
        command = command.then(confirmed_lifecycle_action(action));
    }
    command
}

fn confirmed_lifecycle_action(spec: &'static actions::ActionSpec) -> CommandNode {
    CommandNode::literal(spec.command)
        .execute(Handler(spec.command))
        .then(CommandNode::literal("confirm").execute(Handler(spec.confirmation_handler)))
}

fn view(name: &'static str) -> CommandNode {
    CommandNode::literal(name).execute(Handler(name))
}

fn word(name: &str) -> CommandNode {
    CommandNode::argument(name, &ArgumentType::String(StringType::SingleWord))
}

fn settings_command() -> CommandNode {
    CommandNode::literal("set").then(
        word("key").then(
            CommandNode::argument("value", &ArgumentType::String(StringType::Greedy))
                .execute(Handler("set")),
        ),
    )
}

fn whitelist_command() -> CommandNode {
    CommandNode::literal("whitelist")
        .execute(Handler("whitelist"))
        .then(CommandNode::literal("add").then(word("name").execute(Handler("whitelist_add"))))
        .then(
            CommandNode::literal("remove")
                .then(word("player_id").execute(Handler("whitelist_remove"))),
        )
}

fn restore_command() -> CommandNode {
    CommandNode::literal(actions::spec(actions::LifecycleAction::Restore).command)
        .then(
            CommandNode::literal("confirm").then(word("generation").execute(Handler(
                actions::spec(actions::LifecycleAction::Restore).confirmation_handler,
            ))),
        )
        .then(word("generation").execute(Handler(actions::RESTORE_PROMPT_HANDLER)))
}

pub(crate) fn arg(args: &ConsumedArgs, name: &str) -> Result<String> {
    match args.get_value(name) {
        Arg::Simple(value) | Arg::Msg(value) => Ok(value),
        _ => bail!("missing {name}"),
    }
}

struct Handler(&'static str);

impl CommandHandler for Handler {
    fn handle(
        &self,
        sender: CommandSender,
        server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let run = || -> Result<()> {
            match self.0 {
                "menu" => {
                    if let Some(player) = sender.as_player() {
                        menu::open(&player, View::Home)?;
                    } else {
                        say(
                            &sender,
                            "Use /kurbis status, players, logs, activity, settings, whitelist, snapshots, or login.",
                        );
                    }
                }
                "login" => {
                    let url = api::create_login_link()?;
                    sender.send_message(chat::server_login_link(&url));
                }
                "status" => page(&sender, View::Status)?,
                "players" => page(&sender, View::Players(0))?,
                "logs" => page(&sender, View::Logs(0))?,
                "activity" => page(&sender, View::Activity(0))?,
                "settings" => page(&sender, View::Settings)?,
                "whitelist" => page(&sender, View::Whitelist(0))?,
                "snapshots" => page(&sender, View::Snapshots(0))?,
                command if actions::by_command(command).is_some() => {
                    let action = actions::by_command(command).expect("matched action");
                    let spec = actions::spec(action);
                    say(
                        &sender,
                        &format!(
                            "This will {} this server. Confirm with /kurbis {} confirm.",
                            spec.command, spec.command,
                        ),
                    );
                }
                handler
                    if actions::by_confirmation_handler(handler)
                        .is_some_and(|action| action != actions::LifecycleAction::Restore) =>
                {
                    let action = actions::by_confirmation_handler(handler).expect("matched action");
                    let command = actions::spec(action).command;
                    let result = api::server_command(action, None)?;
                    say(
                        &sender,
                        &format!(
                            "{command} requested; current state: {}.",
                            text(&result, "observed_state")
                        ),
                    );
                }
                "set" => update_setting(&sender, &server, &args)?,
                "whitelist_add" => {
                    let name = arg(&args, "name")?;
                    api::add_whitelist_entry(&name)?;
                    say(&sender, &format!("Added {name} to the whitelist."));
                    if let Some(player) = sender.as_player() {
                        let _ = menu::open(&player, View::Whitelist(0));
                    }
                }
                "whitelist_remove" => {
                    let player_id = arg(&args, "player_id")?;
                    api::remove_whitelist_entry(&player_id)?;
                    say(&sender, &format!("Removed {player_id} from the whitelist."));
                    if let Some(player) = sender.as_player() {
                        let _ = menu::open(&player, View::Whitelist(0));
                    }
                }
                actions::RESTORE_PROMPT_HANDLER => {
                    let generation = positive_generation(&arg(&args, "generation")?)?;
                    say(
                        &sender,
                        &format!(
                            "Restoring snapshot {generation} replaces the current world. Confirm with /kurbis restore confirm {generation}."
                        ),
                    );
                }
                handler
                    if handler
                        == actions::spec(actions::LifecycleAction::Restore)
                            .confirmation_handler =>
                {
                    let generation = positive_generation(&arg(&args, "generation")?)?;
                    api::server_command(actions::LifecycleAction::Restore, Some(generation))?;
                    say(
                        &sender,
                        &format!("Restore of snapshot {generation} requested."),
                    );
                }
                _ => bail!("unknown Kurbis command"),
            }
            Ok(())
        };

        run().map(|()| 1).map_err(|error| {
            CommandError::CommandFailed(TextComponent::text(&format!("Kurbis: {error:#}")))
        })
    }
}

fn page(sender: &CommandSender, view: View) -> Result<()> {
    if let Some(player) = sender.as_player() {
        menu::open(&player, view)?;
    } else {
        let value = menu::page_data(&view)?;
        say(sender, &compact(&value));
    }
    Ok(())
}

fn update_setting(sender: &CommandSender, server: &Server, args: &ConsumedArgs) -> Result<()> {
    let key = arg(args, "key")?;
    let raw = arg(args, "value")?;
    let value = match key.as_str() {
        "motd" => Value::String(raw),
        "difficulty" if matches!(raw.as_str(), "Peaceful" | "Easy" | "Normal" | "Hard") => {
            Value::String(raw)
        }
        "gamemode"
            if matches!(
                raw.as_str(),
                "Survival" | "Creative" | "Adventure" | "Spectator"
            ) =>
        {
            Value::String(raw)
        }
        "view_distance" | "simulation_distance" => Value::Number(raw.parse::<u8>()?.into()),
        "hardcore" | "whitelist" | "tab_stats" => Value::Bool(raw.parse::<bool>()?),
        _ => bail!("invalid settings key or value"),
    };
    api::update_setting(&key, value)?;
    if key == "tab_stats" {
        stats::refresh_now(server);
    }
    say(
        sender,
        &format!(
            "Updated server setting {key}. Runtime configuration changes apply after the next server start."
        ),
    );
    if let Some(player) = sender.as_player() {
        let _ = menu::open(&player, View::Settings);
    }
    Ok(())
}

fn positive_generation(value: &str) -> Result<i64> {
    let generation = value.parse::<i64>()?;
    anyhow::ensure!(generation > 0, "snapshot generation must be positive");
    Ok(generation)
}

fn say(sender: &CommandSender, message: &str) {
    sender.send_message(TextComponent::text(message));
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned()
}

fn compact(value: &Value) -> String {
    let serialized = serde_json::to_string(value).unwrap_or_else(|_| "No data".into());
    serialized.chars().take(240).collect()
}
