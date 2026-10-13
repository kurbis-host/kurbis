//! The plugin pages: installed plugins, one plugin's controls, and the
//! market with search.

use std::collections::HashMap;

use anyhow::{Context, Result};
use pumpkin_plugin_api::Inventory;
use serde_json::Value;
use uuid::Uuid;

use super::{
    Action, TextInputTarget, View, button, content_slot, current_page, page_controls, page_entries,
    value, wrap,
};
use crate::plugins::{name_and_version, state_label};

const DESCRIPTION_WIDTH: usize = 40;

pub(super) fn render_installed(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
) -> Result<()> {
    let plugins = data["plugins"].as_array().cloned().unwrap_or_default();
    button(
        inv,
        actions,
        4,
        "ender_chest",
        "Kurbis • Plugins",
        &format!(
            "{} of {} installed\n{}",
            plugins.len(),
            value(data, &["max_plugins"]),
            if data["synchronized"] == true {
                "The server runs the current list"
            } else {
                "The server loads changes when it runs"
            }
        ),
        None,
    );
    button(
        inv,
        actions,
        6,
        "emerald",
        "§aBrowse the market",
        "Install free plugins from market.pumpkinmc.org",
        Some(Action::Open(View::Market {
            query: None,
            page: 0,
        })),
    );
    button(
        inv,
        actions,
        2,
        "map",
        "Upload a plugin",
        "Upload your own .wasm plugin in the web panel\nClick for a login link",
        Some(Action::Login),
    );
    for (index, plugin) in plugins.iter().take(super::PAGE_SIZE).enumerate() {
        let id = plugin["id"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok());
        button(
            inv,
            actions,
            content_slot(index),
            state_material(plugin),
            &name_and_version(plugin),
            &plugin_lore(plugin, "Click to manage"),
            id.map(|id| Action::Open(View::Plugin(id))),
        );
    }
    Ok(())
}

pub(super) fn render_plugin(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
    id: Uuid,
) -> Result<()> {
    let plugin = installed_plugin(data, id)?;
    let name = name_and_version(plugin);
    button(
        inv,
        actions,
        4,
        state_material(plugin),
        &name,
        &plugin_lore(plugin, ""),
        None,
    );
    let enabled = plugin["enabled"] == true;
    button(
        inv,
        actions,
        20,
        "lever",
        if enabled {
            "Plugin: §aOn"
        } else {
            "Plugin: §cOff"
        },
        if enabled {
            "Click to unload it and keep it off"
        } else {
            "Click to load it again"
        },
        Some(Action::SetPluginEnabled {
            id,
            enabled: !enabled,
        }),
    );
    if let Some(market_id) = plugin["market_plugin_id"].as_i64() {
        button(
            inv,
            actions,
            22,
            "experience_bottle",
            "Update",
            "Install the market's current version",
            Some(Action::InstallMarketPlugin { id: market_id }),
        );
    }
    button(
        inv,
        actions,
        24,
        "lava_bucket",
        "§cRemove",
        "Unload it and delete it from this server",
        Some(Action::Open(View::ConfirmRemovePlugin(id))),
    );
    Ok(())
}

pub(super) fn render_confirm_remove(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
    id: Uuid,
) -> Result<()> {
    let plugin = installed_plugin(data, id)?;
    let name = name_and_version(plugin);
    button(inv, actions, 4, "lava_bucket", "Remove plugin", &name, None);
    button(
        inv,
        actions,
        22,
        "red_concrete",
        &format!("§cRemove {name}"),
        "Its saved data stays on the server\nClick to confirm",
        Some(Action::RemovePlugin { id }),
    );
    button(
        inv,
        actions,
        26,
        "arrow",
        "Cancel",
        "Keep the plugin",
        Some(Action::Open(View::Plugin(id))),
    );
    Ok(())
}

pub(super) fn render_market(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
    view: &View,
) -> Result<()> {
    let View::Market { query, page } = view else {
        anyhow::bail!("the market page needs a market view");
    };
    let listings = data["plugins"].as_array().cloned().unwrap_or_default();
    let page = current_page(listings.len(), *page);
    button(
        inv,
        actions,
        4,
        "emerald",
        "Kurbis • Plugin market",
        &match query {
            Some(query) => format!("{} free plugins match \"{query}\"", listings.len()),
            None => format!("{} free plugins", listings.len()),
        },
        None,
    );
    button(
        inv,
        actions,
        6,
        "spyglass",
        "Search",
        "Search names, developers and descriptions",
        Some(Action::Prompt(TextInputTarget::MarketSearch)),
    );
    if query.is_some() {
        button(
            inv,
            actions,
            2,
            "barrier",
            "Clear search",
            "Show every free plugin",
            Some(Action::Open(View::Market {
                query: None,
                page: 0,
            })),
        );
    }
    for (index, listing) in page_entries(&listings, page).enumerate() {
        let Some(id) = listing["id"].as_i64() else {
            continue;
        };
        let installed = listing["installed_plugin_id"].is_string();
        button(
            inv,
            actions,
            content_slot(index),
            if installed { "enchanted_book" } else { "book" },
            &name_and_version(listing),
            &listing_lore(listing),
            Some(Action::Open(View::MarketPlugin {
                id,
                query: query.clone(),
                page,
            })),
        );
    }
    page_controls(inv, actions, view, page, listings.len());
    Ok(())
}

pub(super) fn render_listing(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
    id: i64,
) -> Result<()> {
    let listing = data["plugins"]
        .as_array()
        .and_then(|listings| listings.iter().find(|listing| listing["id"] == id))
        .context("This plugin is no longer on the market")?;
    let mut lore = vec![
        format!(
            "By {} • {}",
            value(listing, &["developer"]),
            value(listing, &["category"])
        ),
        format!("{} downloads", value(listing, &["downloads"])),
    ];
    lore.extend(wrap(
        listing["description"].as_str().unwrap_or_default(),
        super::LORE_WIDTH,
        super::LORE_LINES - lore.len(),
    ));
    button(
        inv,
        actions,
        22,
        "book",
        &name_and_version(listing),
        &lore.join("\n"),
        None,
    );
    let installed = listing["installed_version"].as_str();
    let current = listing["version"].as_str();
    let (label, description) = match installed {
        None => ("§aInstall", "Install and load it now".to_owned()),
        Some(version) if Some(version) == current => (
            "Installed",
            format!("Version {version} is installed\nClick to reinstall it"),
        ),
        Some(version) => (
            "§aUpdate",
            format!(
                "Installed: {version}\nUpdate to {}",
                current.unwrap_or("the current version")
            ),
        ),
    };
    button(
        inv,
        actions,
        40,
        "emerald_block",
        label,
        &description,
        Some(Action::InstallMarketPlugin { id }),
    );
    Ok(())
}

fn installed_plugin(data: &Value, id: Uuid) -> Result<&Value> {
    let id = id.to_string();
    data["plugins"]
        .as_array()
        .and_then(|plugins| plugins.iter().find(|plugin| plugin["id"] == id.as_str()))
        .context("This plugin is no longer installed")
}

fn state_material(plugin: &Value) -> &'static str {
    match plugin["state"].as_str() {
        Some("loaded") => "lime_dye",
        Some("failed") => "red_dye",
        Some("disabled") => "gray_dye",
        _ => "yellow_dye",
    }
}

fn plugin_lore(plugin: &Value, hint: &str) -> String {
    let source = match plugin["source"].as_str() {
        Some("market") => "From the market",
        _ => "Uploaded",
    };
    let mut lines = vec![state_label(plugin).to_owned(), source.to_owned()];
    if let Some(error) = plugin["error"].as_str() {
        lines.extend(
            wrap(error, DESCRIPTION_WIDTH, 3)
                .into_iter()
                .map(|line| format!("§c{line}")),
        );
    }
    if !hint.is_empty() {
        lines.push(hint.to_owned());
    }
    lines.join("\n")
}

fn listing_lore(listing: &Value) -> String {
    let mut lines = vec![format!(
        "By {} • {}",
        value(listing, &["developer"]),
        value(listing, &["category"])
    )];
    lines.extend(wrap(
        listing["description"].as_str().unwrap_or_default(),
        DESCRIPTION_WIDTH,
        3,
    ));
    lines.push(match listing["installed_version"].as_str() {
        Some(_) => "§aInstalled".to_owned(),
        None => "Click for details".to_owned(),
    });
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plugin_lore_shows_state_source_and_error() {
        let lore = plugin_lore(
            &json!({"state": "failed", "source": "upload", "error": "missing export"}),
            "Click to manage",
        );
        assert_eq!(
            lore,
            "Failed to load\nUploaded\n§cmissing export\nClick to manage"
        );
    }

    #[test]
    fn listing_lore_marks_installed_listings() {
        let listing = json!({
            "developer": "PING", "category": "Admin Tools",
            "description": "Performance in a boss bar.", "installed_version": "0.1"
        });
        assert!(listing_lore(&listing).ends_with("§aInstalled"));
        assert!(listing_lore(&listing).starts_with("By PING • Admin Tools\n"));
    }
}
