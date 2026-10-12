//! The server management pages: status, players, logs, activity, settings,
//! whitelist, snapshots and the lifecycle controls.

use std::collections::HashMap;

use anyhow::{Context, Result};
use pumpkin_plugin_api::Inventory;
use serde_json::Value;

use super::{
    Action, TextInputTarget, View, button, clip, content_slot, current_page, page_controls,
    page_entries, row_slots, value,
};
use crate::actions::{self, LifecycleAction};

pub(super) fn render_home(inv: &Inventory, actions: &mut HashMap<i16, Action>) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "pumpkin",
        "Kurbis • Server management",
        "Operator access • this server only",
        None,
    );
    let ai = crate::config().is_ok_and(|config| !config.ci_preview);
    for (row, entries) in home_rows(ai) {
        for (slot, entry) in row_slots(row, entries.len()).zip(entries) {
            button(
                inv,
                actions,
                slot,
                entry.material,
                entry.label,
                entry.description,
                Some(entry.action),
            );
        }
    }
    Ok(())
}

struct HomeEntry {
    material: &'static str,
    label: &'static str,
    description: &'static str,
    action: Action,
}

/// Home's buttons by row: what to look at, what to configure, what to do.
/// CI previews run without the AI assistant.
fn home_rows(ai: bool) -> [(u32, Vec<HomeEntry>); 3] {
    let entry = |material, label, description, action| HomeEntry {
        material,
        label,
        description,
        action,
    };
    let mut act = vec![entry(
        "lever",
        "Server controls",
        "Start, stop, restart, or snapshot",
        Action::Open(View::Controls),
    )];
    if ai {
        act.push(entry(
            "enchanted_book",
            "AI assistant",
            "Ask the assistant to act on this server",
            Action::Open(View::Ai),
        ));
    }
    act.push(entry(
        "map",
        "Browser login link",
        "Create a one-use link for this server",
        Action::Login,
    ));
    [
        (
            1,
            vec![
                entry(
                    "compass",
                    "Status",
                    "Current state and connection addresses",
                    Action::Open(View::Status),
                ),
                entry(
                    "player_head",
                    "Players",
                    "Online players on this server",
                    Action::Open(View::Players(0)),
                ),
                entry(
                    "paper",
                    "Logs",
                    "Browse recent server console output",
                    Action::Open(View::Logs(0)),
                ),
                entry(
                    "clock",
                    "Activity",
                    "Recent server and lifecycle events",
                    Action::Open(View::Activity(0)),
                ),
            ],
        ),
        (
            3,
            vec![
                entry(
                    "redstone",
                    "Settings",
                    "Adjust difficulty, game mode, MOTD, whitelist, and render distance",
                    Action::Open(View::Settings),
                ),
                entry(
                    "iron_bars",
                    "Whitelist",
                    "Manage allowed players and toggle whitelist enforcement",
                    Action::Open(View::Whitelist(0)),
                ),
                entry(
                    "chest",
                    "Snapshots",
                    "Browse snapshots and restore world backups",
                    Action::Open(View::Snapshots(0)),
                ),
            ],
        ),
        (4, act),
    ]
}

pub(super) fn render_status(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "pumpkin",
        "Kurbis • Status",
        "Server health and connectivity",
        None,
    );
    button(
        inv,
        actions,
        20,
        "nether_star",
        &format!("State: {}", value(data, &["lifecycle", "observed_state"])),
        &format!(
            "Desired: {}\nAddress: {}",
            value(data, &["lifecycle", "desired_state"]),
            value(data, &["java_address"])
        ),
        None,
    );
    button(
        inv,
        actions,
        22,
        "player_head",
        &format!("Players: {}", value(data, &["player_count"])),
        &format!("Server: {}", value(data, &["subdomain"])),
        None,
    );
    button(
        inv,
        actions,
        24,
        "chest",
        "Latest snapshot",
        &format!(
            "Generation: {}",
            value(data, &["latest_snapshot", "generation"])
        ),
        None,
    );
    Ok(())
}

pub(super) fn render_players(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
    requested_page: usize,
    view: &View,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "player_head",
        "Kurbis • Players",
        "Connected players on this server",
        None,
    );
    let entries = data.as_array().context("players response was not a list")?;
    let page = current_page(entries.len(), requested_page);
    for (index, entry) in page_entries(entries, page).enumerate() {
        let name = value(entry, &["name"]);
        let edition = value(entry, &["edition"]);
        let slot = content_slot(index);
        button(
            inv,
            actions,
            slot,
            "player_head",
            &name,
            &format!("Edition: {edition}"),
            None,
        );
    }
    page_controls(inv, actions, view, page, entries.len());
    Ok(())
}

pub(super) fn render_logs(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
    requested_page: usize,
    view: &View,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "paper",
        "Kurbis • Logs",
        "Recent server console logs",
        None,
    );
    let empty_vec = Vec::new();
    let entries = data["items"].as_array().unwrap_or(&empty_vec);
    let page = current_page(entries.len(), requested_page);
    for (index, entry) in page_entries(entries, page).enumerate() {
        let line = value(entry, &["line"]);
        let slot = content_slot(index);
        button(inv, actions, slot, "paper", &clip(&line, 40), &line, None);
    }
    page_controls(inv, actions, view, page, entries.len());
    Ok(())
}

pub(super) fn render_activity(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
    requested_page: usize,
    view: &View,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "clock",
        "Kurbis • Activity",
        "Recent server activity and operations",
        None,
    );
    let empty_vec = Vec::new();
    let entries = data.as_array().unwrap_or(&empty_vec);
    let page = current_page(entries.len(), requested_page);
    for (index, entry) in page_entries(entries, page).enumerate() {
        let type_name = value(entry, &["type"]);
        let created_at = value(entry, &["created_at"]);
        let summary = value(entry, &["summary"]);
        let slot = content_slot(index);
        button(
            inv,
            actions,
            slot,
            "clock",
            &type_name,
            &format!("{created_at}\n{summary}"),
            None,
        );
    }
    page_controls(inv, actions, view, page, entries.len());
    Ok(())
}

pub(super) fn render_settings(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    settings: &Value,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "redstone",
        "Kurbis • Settings",
        "Runtime settings apply on next server start",
        None,
    );
    button(
        inv,
        actions,
        10,
        "writable_book",
        "MOTD",
        &format!("{}\nClick to edit", value(settings, &["motd"])),
        Some(Action::Prompt(TextInputTarget::Motd)),
    );
    button(
        inv,
        actions,
        19,
        "compass",
        "Difficulty",
        &format!(
            "Current: {}\nClick to choose options",
            value(settings, &["difficulty"])
        ),
        Some(Action::Open(View::SelectDifficulty)),
    );
    button(
        inv,
        actions,
        28,
        "grass_block",
        "Game mode",
        &format!(
            "Current: {}\nClick to choose options",
            value(settings, &["gamemode"])
        ),
        Some(Action::Open(View::SelectGamemode)),
    );
    setting_distance_button(
        inv,
        actions,
        SettingControl {
            slot: 37,
            material: "spyglass",
            label: "View distance",
            current: value(settings, &["view_distance"]),
            previous: Some(Action::AdjustSetting {
                key: "view_distance",
                direction: -1,
            }),
            next: Some(Action::AdjustSetting {
                key: "view_distance",
                direction: 1,
            }),
        },
    );
    setting_distance_button(
        inv,
        actions,
        SettingControl {
            slot: 41,
            material: "clock",
            label: "Simulation distance",
            current: value(settings, &["simulation_distance"]),
            previous: Some(Action::AdjustSetting {
                key: "simulation_distance",
                direction: -1,
            }),
            next: Some(Action::AdjustSetting {
                key: "simulation_distance",
                direction: 1,
            }),
        },
    );
    button(
        inv,
        actions,
        43,
        "name_tag",
        "Tab stats",
        &format!(
            "Current: {}\nClick to toggle",
            value(settings, &["tab_stats"])
        ),
        Some(Action::ToggleSetting { key: "tab_stats" }),
    );
    button(
        inv,
        actions,
        47,
        "redstone_torch",
        "Hardcore",
        &format!(
            "Current: {}\nClick to toggle",
            value(settings, &["hardcore"])
        ),
        Some(Action::ToggleSetting { key: "hardcore" }),
    );
    button(
        inv,
        actions,
        51,
        "iron_bars",
        "Whitelist",
        &format!(
            "Current: {}\nClick to toggle",
            value(settings, &["whitelist"])
        ),
        Some(Action::ToggleSetting { key: "whitelist" }),
    );
    Ok(())
}

pub(super) fn render_select_difficulty(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "compass",
        "Select Difficulty",
        "Choose server difficulty level",
        None,
    );
    let current = value(data, &["settings", "difficulty"]);
    let options = [
        ("Peaceful", "feather", 20),
        ("Easy", "wooden_sword", 21),
        ("Normal", "iron_sword", 23),
        ("Hard", "diamond_sword", 24),
    ];
    for (name, mat, slot) in options {
        let is_selected = name.eq_ignore_ascii_case(&current);
        let title = if is_selected {
            format!("{name} §a[Selected]")
        } else {
            name.to_owned()
        };
        let desc = if is_selected {
            "Currently active difficulty\nClick to keep".to_owned()
        } else {
            format!("Click to set difficulty to {name}")
        };
        button(
            inv,
            actions,
            slot,
            mat,
            &title,
            &desc,
            Some(Action::SetSetting {
                key: "difficulty",
                value: name,
            }),
        );
    }
    Ok(())
}

pub(super) fn render_select_gamemode(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "grass_block",
        "Select Game Mode",
        "Choose default player game mode",
        None,
    );
    let current = value(data, &["settings", "gamemode"]);
    let options = [
        ("Survival", "iron_pickaxe", 20),
        ("Creative", "grass_block", 21),
        ("Adventure", "map", 23),
        ("Spectator", "ender_eye", 24),
    ];
    for (name, mat, slot) in options {
        let is_selected = name.eq_ignore_ascii_case(&current);
        let title = if is_selected {
            format!("{name} §a[Selected]")
        } else {
            name.to_owned()
        };
        let desc = if is_selected {
            "Currently active game mode\nClick to keep".to_owned()
        } else {
            format!("Click to set game mode to {name}")
        };
        button(
            inv,
            actions,
            slot,
            mat,
            &title,
            &desc,
            Some(Action::SetSetting {
                key: "gamemode",
                value: name,
            }),
        );
    }
    Ok(())
}

pub(super) fn render_whitelist(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
    requested_page: usize,
    view: &View,
) -> Result<()> {
    let empty_vec = Vec::new();
    let entries = data["entries"].as_array().unwrap_or(&empty_vec);
    let page = current_page(entries.len(), requested_page);
    let enabled = data["enabled"] == true;

    // Slot 2: Toggle Whitelist
    button(
        inv,
        actions,
        2,
        "lever",
        if enabled {
            "Whitelist: §aEnabled"
        } else {
            "Whitelist: §cDisabled"
        },
        "Click to toggle whitelist enforcement",
        Some(Action::ToggleSetting { key: "whitelist" }),
    );

    // Slot 4: Info header
    button(
        inv,
        actions,
        4,
        "iron_bars",
        "Kurbis • Whitelist",
        &format!(
            "{} entries • synced: {}",
            entries.len(),
            value(data, &["synchronized"])
        ),
        None,
    );

    // Slot 6: Add player button
    button(
        inv,
        actions,
        6,
        "emerald",
        "§aAdd player",
        "Click to add a player to the whitelist",
        Some(Action::Prompt(TextInputTarget::WhitelistAdd(page))),
    );

    // Render whitelisted players on content slots
    for (index, entry) in page_entries(entries, page).enumerate() {
        let name = value(entry, &["name"]);
        let player_id = value(entry, &["player_id"]);
        let slot = content_slot(index);
        button(
            inv,
            actions,
            slot,
            "player_head",
            &name,
            &format!("UUID: {player_id}\n§cClick to remove"),
            Some(Action::Open(View::ConfirmRemoveWhitelist {
                name: name.clone(),
                player_id,
                page,
            })),
        );
    }

    page_controls(inv, actions, view, page, entries.len());
    Ok(())
}

pub(super) fn render_confirm_remove_whitelist(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    name: &str,
    player_id: &str,
    page: usize,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "iron_bars",
        "Remove Whitelist Entry",
        &format!("Player: {name}"),
        None,
    );
    button(
        inv,
        actions,
        22,
        "red_concrete",
        &format!("§cRemove {}", name),
        &format!("UUID: {player_id}\nClick to confirm removal"),
        Some(Action::ConfirmRemoveWhitelist {
            name: name.to_owned(),
            player_id: player_id.to_owned(),
            page,
        }),
    );
    button(
        inv,
        actions,
        26,
        "arrow",
        "Cancel",
        "Return to whitelist",
        Some(Action::Open(View::Whitelist(page))),
    );
    Ok(())
}

pub(super) fn render_snapshots(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
    requested_page: usize,
    view: &View,
) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "chest",
        "Kurbis • Snapshots",
        "Click a snapshot to restore",
        None,
    );
    let empty_vec = Vec::new();
    let snapshots = data.as_array().unwrap_or(&empty_vec);
    let page = current_page(snapshots.len(), requested_page);
    for (index, snapshot) in page_entries(snapshots, page).enumerate() {
        let generation = snapshot["generation"].as_i64().unwrap_or(0);
        let reason = value(snapshot, &["reason"]);
        let size = value(snapshot, &["size_bytes"]);
        let slot = content_slot(index);
        button(
            inv,
            actions,
            slot,
            "chest",
            &format!("Generation {generation}"),
            &format!("{reason}\n{size} bytes\nClick to restore"),
            Some(Action::Open(View::RestoreConfirm { generation, page })),
        );
    }
    page_controls(inv, actions, view, page, snapshots.len());
    Ok(())
}

pub(super) fn render_controls(inv: &Inventory, actions: &mut HashMap<i16, Action>) -> Result<()> {
    button(
        inv,
        actions,
        4,
        "lever",
        "Kurbis • Server controls",
        "Execute server lifecycle operations",
        None,
    );
    for spec in actions::prompted_actions() {
        if let Some(slot) = spec.control_slot {
            button(
                inv,
                actions,
                slot,
                spec.material,
                spec.label,
                "Review and confirm this action",
                Some(Action::Open(View::LifecycleConfirm(spec.action))),
            );
        }
    }
    Ok(())
}

pub(super) fn render_lifecycle_confirm(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    action: LifecycleAction,
) -> Result<()> {
    let spec = actions::spec(action);
    button(
        inv,
        actions,
        4,
        spec.material,
        &format!("Confirm {}", spec.command),
        "This action affects the server state",
        None,
    );
    button(
        inv,
        actions,
        22,
        spec.material,
        &format!("Confirm {}", spec.command),
        spec.confirmation,
        Some(Action::Execute(action)),
    );
    button(
        inv,
        actions,
        26,
        "arrow",
        "Cancel",
        "Return to server controls",
        Some(Action::Open(View::Controls)),
    );
    Ok(())
}

pub(super) fn render_restore_confirm(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    generation: i64,
    page: usize,
) -> Result<()> {
    let restore = actions::spec(LifecycleAction::Restore);
    button(
        inv,
        actions,
        4,
        restore.material,
        "Confirm Restore",
        &format!("Target generation: {generation}"),
        None,
    );
    button(
        inv,
        actions,
        22,
        restore.material,
        restore.label,
        &format!("Replace current world with generation {generation}. Click to confirm."),
        Some(Action::Restore { generation, page }),
    );
    button(
        inv,
        actions,
        26,
        "arrow",
        "Cancel",
        "Return to snapshots",
        Some(Action::Open(View::Snapshots(page))),
    );
    Ok(())
}

struct SettingControl {
    slot: u32,
    material: &'static str,
    label: &'static str,
    current: String,
    previous: Option<Action>,
    next: Option<Action>,
}

fn setting_distance_button(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    control: SettingControl,
) {
    button(
        inv,
        actions,
        control.slot,
        control.material,
        control.label,
        &control.current,
        None,
    );
    if let Some(action) = control.previous {
        button(
            inv,
            actions,
            control.slot - 1,
            "arrow",
            "Decrease",
            "Choose a smaller distance",
            Some(action),
        );
    }
    if let Some(action) = control.next {
        button(
            inv,
            actions,
            control.slot + 1,
            "arrow",
            "Increase",
            "Choose a larger distance",
            Some(action),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_rows_stay_centred_with_and_without_the_assistant() {
        let slots = |ai| {
            home_rows(ai)
                .into_iter()
                .map(|(row, entries)| row_slots(row, entries.len()).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            slots(false),
            vec![vec![10, 12, 14, 16], vec![29, 31, 33], vec![39, 41]]
        );
        assert_eq!(
            slots(true),
            vec![vec![10, 12, 14, 16], vec![29, 31, 33], vec![38, 40, 42]]
        );
        assert_eq!(row_slots(1, 5).collect::<Vec<_>>(), vec![9, 11, 13, 15, 17]);
        assert_eq!(row_slots(2, 0).count(), 0);
    }
}
