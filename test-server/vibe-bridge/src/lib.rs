use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use pumpkin_plugin_api::{
    Context, Plugin, PluginMetadata,
    command::{
        Arg, ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs,
        StringType,
    },
    commands::CommandHandler,
    permission::{Permission, PermissionChild, PermissionDefault, PermissionLevel},
    permissions, register_plugin,
    text::TextComponent,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const PERMISSION: &str = "vibe-bridge:command";
const DATA_ROOT: &str = "data";

struct VibeBridge;

impl Plugin for VibeBridge {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "vibe-bridge".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Melon Hoster".into()],
            description: "OP-only bridge from Pumpkin commands to the local Codex controller."
                .into(),
            dependencies: vec![],
            permissions: vec![permissions::FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        for dir in ["requests", "results", "cancel"] {
            fs::create_dir_all(Path::new(DATA_ROOT).join(dir))
                .map_err(|error| error.to_string())?;
        }

        context
            .register_permission(&Permission {
                node: PERMISSION.into(),
                description: "Use the local Pumpkin vibe-coding controller.".into(),
                default: PermissionDefault::Op(PermissionLevel::One),
                children: Vec::<PermissionChild>::new(),
            })
            .map_err(|error| format!("register permission: {error}"))?;

        let command = Command::new(&["vibe".into()], "Vibe-code Pumpkin plugins with Codex.")
            .then(
                CommandNode::literal("code").then(
                    CommandNode::argument("plugin", &ArgumentType::String(StringType::SingleWord))
                        .then(
                            CommandNode::argument(
                                "prompt",
                                &ArgumentType::String(StringType::Greedy),
                            )
                            .execute(CodeHandler),
                        ),
                ),
            )
            .then(
                CommandNode::literal("status").then(
                    CommandNode::argument("job", &ArgumentType::String(StringType::SingleWord))
                        .execute(StatusHandler),
                ),
            )
            .then(CommandNode::literal("list").execute(ListHandler))
            .then(
                CommandNode::literal("cancel").then(
                    CommandNode::argument("job", &ArgumentType::String(StringType::SingleWord))
                        .execute(CancelHandler),
                ),
            )
            .then(
                CommandNode::literal("revert").then(
                    CommandNode::argument("plugin", &ArgumentType::String(StringType::SingleWord))
                        .execute(RevertHandler),
                ),
            );
        context.register_command(command, PERMISSION);
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum JobAction {
    Code,
    Revert,
}

#[derive(Serialize)]
struct JobRequest {
    id: Uuid,
    action: JobAction,
    plugin: String,
    prompt: String,
    player: String,
    created_unix_ms: u128,
}

#[derive(Deserialize)]
struct JobResult {
    state: String,
    message: String,
}

#[derive(Deserialize, Default)]
struct PluginIndex {
    plugins: Vec<PluginEntry>,
}

#[derive(Deserialize)]
struct PluginEntry {
    name: String,
    revision: String,
    artifact_bytes: u64,
}

struct CodeHandler;
struct StatusHandler;
struct ListHandler;
struct CancelHandler;
struct RevertHandler;

impl CommandHandler for CodeHandler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: pumpkin_plugin_api::Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let plugin = string_arg(&args, "plugin")?;
        let prompt = string_arg(&args, "prompt")?;
        let id =
            enqueue(JobAction::Code, plugin, prompt, sender.get_name()).map_err(command_failure)?;
        sender.send_message(TextComponent::text(&format!("Vibe job queued: {id}")));
        Ok(1)
    }
}

impl CommandHandler for RevertHandler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: pumpkin_plugin_api::Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let plugin = string_arg(&args, "plugin")?;
        let id = enqueue(JobAction::Revert, plugin, String::new(), sender.get_name())
            .map_err(command_failure)?;
        sender.send_message(TextComponent::text(&format!("Vibe revert queued: {id}")));
        Ok(1)
    }
}

impl CommandHandler for StatusHandler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: pumpkin_plugin_api::Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let job = string_arg(&args, "job")?;
        let id = Uuid::parse_str(&job).map_err(|_| command_failure("invalid job id".into()))?;
        let path = Path::new(DATA_ROOT)
            .join("results")
            .join(format!("{id}.json"));
        match fs::read(path) {
            Ok(bytes) => {
                let result: JobResult = serde_json::from_slice(&bytes)
                    .map_err(|error| command_failure(error.to_string()))?;
                sender.send_message(TextComponent::text(&format!(
                    "{}: {}",
                    result.state, result.message
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                sender.send_message(TextComponent::text("queued or not started yet"));
            }
            Err(error) => return Err(command_failure(error.to_string())),
        }
        Ok(1)
    }
}

impl CommandHandler for ListHandler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: pumpkin_plugin_api::Server,
        _args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let path = Path::new(DATA_ROOT).join("plugins.json");
        let index = match fs::read(path) {
            Ok(bytes) => serde_json::from_slice::<PluginIndex>(&bytes)
                .map_err(|error| command_failure(error.to_string()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => PluginIndex::default(),
            Err(error) => return Err(command_failure(error.to_string())),
        };
        if index.plugins.is_empty() {
            sender.send_message(TextComponent::text("No vibe-coded plugins deployed."));
        } else {
            let lines = index
                .plugins
                .into_iter()
                .map(|plugin| {
                    let short = plugin.revision.chars().take(8).collect::<String>();
                    format!(
                        "{} @ {} ({} bytes)",
                        plugin.name, short, plugin.artifact_bytes
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            sender.send_message(TextComponent::text(&lines));
        }
        Ok(1)
    }
}

impl CommandHandler for CancelHandler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: pumpkin_plugin_api::Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let job = string_arg(&args, "job")?;
        let id = Uuid::parse_str(&job).map_err(|_| command_failure("invalid job id".into()))?;
        let path = Path::new(DATA_ROOT)
            .join("cancel")
            .join(format!("{id}.json"));
        atomic_write(&path, br#"{"cancel":true}"#).map_err(command_failure)?;
        sender.send_message(TextComponent::text(&format!(
            "Cancellation requested for {id}"
        )));
        Ok(1)
    }
}

fn enqueue(
    action: JobAction,
    plugin: String,
    prompt: String,
    player: String,
) -> Result<Uuid, String> {
    let id = Uuid::new_v4();
    let request = JobRequest {
        id,
        action,
        plugin,
        prompt,
        player,
        created_unix_ms: now_ms(),
    };
    let bytes = serde_json::to_vec_pretty(&request).map_err(|error| error.to_string())?;
    let path = Path::new(DATA_ROOT)
        .join("requests")
        .join(format!("{id}.json"));
    atomic_write(&path, &bytes)?;
    Ok(id)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temp = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    fs::write(&temp, bytes).map_err(|error| error.to_string())?;
    fs::rename(&temp, path).map_err(|error| error.to_string())
}

fn string_arg(args: &ConsumedArgs, name: &str) -> Result<String, CommandError> {
    match args.get_value(name) {
        Arg::Simple(value) | Arg::Msg(value) => Ok(value),
        _ => Err(CommandError::InvalidConsumption(Some(format!(
            "expected string argument {name}"
        )))),
    }
}

fn command_failure(message: String) -> CommandError {
    CommandError::CommandFailed(TextComponent::text(&message))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

register_plugin!(VibeBridge);
