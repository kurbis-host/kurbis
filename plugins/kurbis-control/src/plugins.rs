//! `/plugins`: operators install plugins from the Pumpkin market and turn
//! installed plugins on and off. Kurbis loads a change into the running
//! server; `/plugins` alone opens the plugins page of the server menu.
//!
//! The command replaces Pumpkin's own `/plugins` listing, which shows only
//! what is loaded and none of what Kurbis manages.

use anyhow::{Context, Result};
use pumpkin_plugin_api::{
    Server,
    command::{
        ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType,
    },
    commands::CommandHandler,
    common::NamedColor,
    text::TextComponent,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    api,
    command::arg,
    menu::{self, View},
};

/// Market listings a chat search prints.
const SEARCH_RESULTS: usize = 8;

pub(crate) fn command() -> Command {
    Command::new(
        &["plugins".into()],
        "Install and manage this server's plugins.",
    )
    .execute(Handler::Menu)
    .then(CommandNode::literal("market").execute(Handler::Market))
    .then(CommandNode::literal("search").then(name(Handler::Search)))
    .then(CommandNode::literal("install").then(name(Handler::Install)))
    .then(CommandNode::literal("enable").then(name(Handler::Enable)))
    .then(CommandNode::literal("disable").then(name(Handler::Disable)))
    .then(
        CommandNode::literal("remove")
            .then(CommandNode::literal("confirm").then(name(Handler::RemoveConfirmed)))
            .then(name(Handler::Remove)),
    )
}

fn name(handler: Handler) -> CommandNode {
    CommandNode::argument("name", &ArgumentType::String(StringType::Greedy)).execute(handler)
}

pub(crate) fn installed() -> Result<Value> {
    api::get(&api::server_path("/plugins")?)
}

pub(crate) fn market(query: Option<&str>) -> Result<Value> {
    let suffix = match query {
        Some(query) => format!("/plugins/market?query={}", encode_query(query)),
        None => "/plugins/market".into(),
    };
    api::get(&api::server_path(&suffix)?)
}

/// Install a market plugin, or update it to the market's current version.
pub(crate) fn install(market_plugin_id: i64) -> Result<Value> {
    api::post(
        &api::server_path("/plugins/market")?,
        &json!({ "market_plugin_id": market_plugin_id }),
    )
}

pub(crate) fn set_enabled(plugin_id: Uuid, enabled: bool) -> Result<Value> {
    api::patch(
        &api::server_path(&format!("/plugins/{plugin_id}"))?,
        &json!({ "enabled": enabled }),
    )
}

pub(crate) fn remove(plugin_id: Uuid) -> Result<()> {
    api::delete(&api::server_path(&format!("/plugins/{plugin_id}"))?)?;
    Ok(())
}

/// What the server does with an installed plugin, for people.
pub(crate) fn state_label(plugin: &Value) -> &'static str {
    match plugin["state"].as_str() {
        Some("loaded") => "Running",
        Some("failed") => "Failed to load",
        Some("disabled") => "Turned off",
        _ => "Waiting to load",
    }
}

/// The installed plugin called `name`, ignoring case.
fn find_installed(name: &str) -> Result<Value> {
    let name = name.trim();
    installed()?["plugins"]
        .as_array()
        .and_then(|plugins| {
            plugins
                .iter()
                .find(|plugin| {
                    plugin["name"]
                        .as_str()
                        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
                })
                .cloned()
        })
        .with_context(|| format!("No installed plugin is called {name}. See /plugins."))
}

/// The market listing with this id or name.
fn find_listing(name: &str) -> Result<i64> {
    let name = name.trim();
    if let Ok(id) = name.parse::<i64>() {
        return Ok(id);
    }
    market(Some(name))?["plugins"]
        .as_array()
        .and_then(|plugins| {
            plugins.iter().find_map(|plugin| {
                plugin["name"]
                    .as_str()
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
                    .then(|| plugin["id"].as_i64())
                    .flatten()
            })
        })
        .with_context(|| {
            format!("The market has no free plugin called {name}. Try /plugins search {name}.")
        })
}

fn plugin_id(plugin: &Value) -> Result<Uuid> {
    plugin["id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .context("the control plane sent a plugin without an id")
}

#[derive(Clone, Copy)]
enum Handler {
    Menu,
    Market,
    Search,
    Install,
    Enable,
    Disable,
    Remove,
    RemoveConfirmed,
}

impl CommandHandler for Handler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let run = || -> Result<()> {
            if let Some(player) = sender.as_player() {
                menu::forget_text_input(&player);
            }
            match self {
                Self::Menu => match sender.as_player() {
                    Some(player) => menu::open(&player, View::Plugins)?,
                    None => print_installed(&sender)?,
                },
                Self::Market => match sender.as_player() {
                    Some(player) => menu::open(
                        &player,
                        View::Market {
                            query: None,
                            page: 0,
                        },
                    )?,
                    None => print_search(&sender, None)?,
                },
                Self::Search => {
                    let query = arg(&args, "name")?;
                    match sender.as_player() {
                        Some(player) => menu::open(
                            &player,
                            View::Market {
                                query: Some(query),
                                page: 0,
                            },
                        )?,
                        None => print_search(&sender, Some(&query))?,
                    }
                }
                Self::Install => {
                    let plugin = install(find_listing(&arg(&args, "name")?)?)?;
                    say(&sender, &installed_message(&plugin));
                }
                Self::Enable | Self::Disable => {
                    let enabled = matches!(self, Self::Enable);
                    let plugin = find_installed(&arg(&args, "name")?)?;
                    let plugin = set_enabled(plugin_id(&plugin)?, enabled)?;
                    say(
                        &sender,
                        &format!(
                            "Turned {} {}; the server applies it now.",
                            text(&plugin, "name"),
                            if enabled { "on" } else { "off" }
                        ),
                    );
                }
                Self::Remove => {
                    let plugin = find_installed(&arg(&args, "name")?)?;
                    let name = text(&plugin, "name");
                    say(
                        &sender,
                        &format!(
                            "Removing {name} unloads it and deletes it from this server; its saved data stays. Confirm with /plugins remove confirm {name}."
                        ),
                    );
                }
                Self::RemoveConfirmed => {
                    let plugin = find_installed(&arg(&args, "name")?)?;
                    remove(plugin_id(&plugin)?)?;
                    say(&sender, &format!("Removed {}.", text(&plugin, "name")));
                }
            }
            Ok(())
        };
        run().map(|()| 1).map_err(|error| {
            CommandError::CommandFailed(TextComponent::text(&format!("Kurbis: {error:#}")))
        })
    }
}

pub(crate) fn installed_message(plugin: &Value) -> String {
    format!(
        "Installed {}; the server loads it now.",
        name_and_version(plugin)
    )
}

/// "Name 1.0", or the name alone when the plugin has no version.
pub(crate) fn name_and_version(plugin: &Value) -> String {
    match plugin["version"]
        .as_str()
        .filter(|version| !version.is_empty())
    {
        Some(version) => format!("{} {version}", text(plugin, "name")),
        None => text(plugin, "name"),
    }
}

fn print_installed(sender: &CommandSender) -> Result<()> {
    let data = installed()?;
    let plugins = data["plugins"].as_array().cloned().unwrap_or_default();
    if plugins.is_empty() {
        say(
            sender,
            "No plugins are installed. Browse the market with /plugins market.",
        );
        return Ok(());
    }
    let mut message =
        TextComponent::text(&format!("Plugins ({}):", plugins.len())).color_named(NamedColor::Gold);
    for plugin in &plugins {
        let error = plugin["error"]
            .as_str()
            .map(|error| format!(" ({error})"))
            .unwrap_or_default();
        message = message.add_child(TextComponent::text(&format!(
            "\n- {}: {}{error}",
            name_and_version(plugin),
            state_label(plugin)
        )));
    }
    sender.send_message(message);
    Ok(())
}

fn print_search(sender: &CommandSender, query: Option<&str>) -> Result<()> {
    let data = market(query)?;
    let plugins = data["plugins"].as_array().cloned().unwrap_or_default();
    if plugins.is_empty() {
        say(sender, "The market has no free plugin matching that.");
        return Ok(());
    }
    let mut message = TextComponent::text(&format!(
        "Market plugins ({}{}):",
        plugins.len().min(SEARCH_RESULTS),
        if plugins.len() > SEARCH_RESULTS {
            format!(" of {}", plugins.len())
        } else {
            String::new()
        }
    ))
    .color_named(NamedColor::Gold);
    for plugin in plugins.iter().take(SEARCH_RESULTS) {
        let id = plugin["id"].as_i64().unwrap_or_default();
        let installed = if plugin["installed_plugin_id"].is_string() {
            " (installed)"
        } else {
            ""
        };
        message = message
            .add_child(TextComponent::text(&format!(
                "\n- {} by {}{installed} ",
                name_and_version(plugin),
                text(plugin, "developer"),
            )))
            .add_child(
                TextComponent::text("[Install]")
                    .color_named(NamedColor::Green)
                    .underlined(true)
                    .click_run_command(&format!("/plugins install {id}"))
                    .hover_show_text(TextComponent::text(
                        &menu::wrap(plugin["description"].as_str().unwrap_or_default(), 40, 8)
                            .join("\n"),
                    )),
            );
    }
    sender.send_message(message);
    Ok(())
}

fn say(sender: &CommandSender, message: &str) {
    sender.send_message(TextComponent::text(message));
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or("unknown").to_owned()
}

/// Percent-encode a query string value.
fn encode_query(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_queries_are_percent_encoded() {
        assert_eq!(encode_query("tps bar"), "tps%20bar");
        assert_eq!(encode_query("a&b=c#d/é"), "a%26b%3Dc%23d%2F%C3%A9");
        assert_eq!(encode_query("Name-1_.~"), "Name-1_.~");
    }

    #[test]
    fn plugin_states_read_as_what_the_server_does() {
        assert_eq!(state_label(&json!({"state": "loaded"})), "Running");
        assert_eq!(state_label(&json!({"state": "failed"})), "Failed to load");
        assert_eq!(state_label(&json!({"state": "disabled"})), "Turned off");
        assert_eq!(state_label(&json!({"state": "pending"})), "Waiting to load");
    }

    #[test]
    fn install_messages_skip_a_missing_version() {
        assert_eq!(
            installed_message(&json!({"name": "TPSBar", "version": "0.1"})),
            "Installed TPSBar 0.1; the server loads it now."
        );
        assert_eq!(
            installed_message(&json!({"name": "Mine", "version": null})),
            "Installed Mine; the server loads it now."
        );
    }
}
