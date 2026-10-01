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
use serde_json::{Value, json};

use crate::{
    api,
    menu::{self, View},
};

pub(crate) fn command() -> Command {
    Command::new(&["kurbis".into()], "Manage this Kurbis server.")
        .execute(Handler("menu"))
        .then(CommandNode::literal("login").execute(Handler("login")))
        .then(view("status"))
        .then(view("players"))
        .then(view("logs"))
        .then(view("activity"))
        .then(view("settings"))
        .then(view("snapshots"))
        .then(confirmed_lifecycle_action("start"))
        .then(confirmed_lifecycle_action("stop"))
        .then(confirmed_lifecycle_action("restart"))
        .then(confirmed_lifecycle_action("snapshot"))
        .then(settings_command())
        .then(whitelist_command())
        .then(restore_command())
}

fn confirmed_lifecycle_action(action: &'static str) -> CommandNode {
    let confirm_handler = match action {
        "start" => "start_confirm",
        "stop" => "stop_confirm",
        "restart" => "restart_confirm",
        "snapshot" => "snapshot_confirm",
        _ => unreachable!("only supported lifecycle actions are registered"),
    };
    CommandNode::literal(action)
        .execute(Handler(action))
        .then(CommandNode::literal("confirm").execute(Handler(confirm_handler)))
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
        .then(
            CommandNode::literal("add")
                .then(word("player_id").then(word("name").execute(Handler("whitelist_add")))),
        )
        .then(
            CommandNode::literal("remove")
                .then(word("player_id").execute(Handler("whitelist_remove"))),
        )
}

fn restore_command() -> CommandNode {
    CommandNode::literal("restore")
        .then(
            CommandNode::literal("confirm")
                .then(word("generation").execute(Handler("restore_confirm"))),
        )
        .then(word("generation").execute(Handler("restore_prompt")))
}

fn arg(args: &ConsumedArgs, name: &str) -> Result<String> {
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
        _server: Server,
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
                    say(
                        &sender,
                        &format!("One-use server login link (valid for 15 minutes): {url}"),
                    );
                }
                "status" => page(&sender, View::Status)?,
                "players" => page(&sender, View::Players(0))?,
                "logs" => page(&sender, View::Logs)?,
                "activity" => page(&sender, View::Activity)?,
                "settings" => page(&sender, View::Settings)?,
                "whitelist" => page(&sender, View::Whitelist(0))?,
                "snapshots" => page(&sender, View::Snapshots(0))?,
                "start" | "stop" | "restart" | "snapshot" => {
                    let action = self.0;
                    say(
                        &sender,
                        &format!(
                            "This will {action} this server. Confirm with /kurbis {action} confirm."
                        ),
                    );
                }
                "start_confirm" | "stop_confirm" | "restart_confirm" | "snapshot_confirm" => {
                    let action = self.0.strip_suffix("_confirm").unwrap_or(self.0);
                    let result = api::server_action(action)?;
                    say(
                        &sender,
                        &format!(
                            "{action} requested; current state: {}.",
                            text(&result, "observed_state")
                        ),
                    );
                }
                "set" => update_setting(&sender, &args)?,
                "whitelist_add" => {
                    let player_id = uuid::Uuid::parse_str(&arg(&args, "player_id")?)?;
                    let name = arg(&args, "name")?;
                    api::post(
                        &api::server_path("/whitelist")?,
                        &json!({"player_id": player_id, "name": name}),
                    )?;
                    say(&sender, &format!("Added {name} to the whitelist."));
                }
                "whitelist_remove" => {
                    let player_id = uuid::Uuid::parse_str(&arg(&args, "player_id")?)?;
                    api::delete(&format!(
                        "{}?player_id={player_id}",
                        api::server_path("/whitelist")?
                    ))?;
                    say(&sender, &format!("Removed {player_id} from the whitelist."));
                }
                "restore_prompt" => {
                    let generation = positive_generation(&arg(&args, "generation")?)?;
                    say(
                        &sender,
                        &format!(
                            "Restoring snapshot {generation} replaces the current world. Confirm with /kurbis restore confirm {generation}."
                        ),
                    );
                }
                "restore_confirm" => {
                    let generation = positive_generation(&arg(&args, "generation")?)?;
                    api::restore_snapshot(generation)?;
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

fn update_setting(sender: &CommandSender, args: &ConsumedArgs) -> Result<()> {
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
        "hardcore" | "whitelist" => Value::Bool(raw.parse::<bool>()?),
        _ => bail!("invalid settings key or value"),
    };
    let mut settings = serde_json::Map::new();
    settings.insert(key.clone(), value);
    api::patch(&api::server_path("")?, &json!({"settings": settings}))?;
    say(
        sender,
        &format!(
            "Updated server setting {key}. Settings can be changed while the server is sleeping."
        ),
    );
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
