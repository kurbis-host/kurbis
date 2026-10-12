//! Runs the tool calls the AI gateway hands out, as the operator who asked.
//!
//! The gateway validates every call before it reaches the plugin and refuses
//! the commands the assistant may not run, so this side only resolves the
//! operator and reads or acts on the live server.

use pumpkin_plugin_api::{
    EntityType, Player, Server, World, server::CommandSender, uuid as wit_uuid,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::PERMISSION;

/// The gateway's limit for one tool result.
pub(super) const MAX_OUTPUT_BYTES: usize = 16 * 1024;
const TRUNCATION_MARKER: &str = "… (truncated)";
const DEFAULT_LISTED_ENTITIES: u64 = 50;
const MAX_LISTED_ENTITIES: u64 = 100;

/// Run one call and return its output and whether it failed.
pub(super) fn run(server: &Server, operator_id: Uuid, name: &str, input: &Value) -> (String, bool) {
    let result = operator(server, operator_id).and_then(|operator| match name {
        "get_server_status" => Ok(server_status(server)),
        "list_players" => Ok(players(server)),
        "list_worlds" => Ok(worlds(server)),
        "list_entities" => entities(server, &operator, input),
        "run_server_command" => run_command(server, operator, input),
        _ => Err(format!("Unknown tool: {name}")),
    });
    match result {
        Ok(output) => (truncate(output), false),
        Err(error) => (truncate(error), true),
    }
}

/// The asking operator, while online and still allowed to manage the server.
fn operator(server: &Server, operator_id: Uuid) -> Result<Player, String> {
    wit_uuid::parse(&operator_id.to_string())
        .and_then(|id| server.get_player_by_uuid(id))
        .filter(|player| player.has_permission(PERMISSION))
        .ok_or_else(|| {
            "The operator who asked is no longer online or no longer an operator.".into()
        })
}

fn server_status(server: &Server) -> String {
    json!({
        "player_count": server.get_player_count(),
        "max_players": server.get_max_players(),
        "difficulty": format!("{:?}", server.get_difficulty()),
        "tps": server.get_tps(),
        "mspt": server.get_mspt(),
        "worlds": server
            .get_all_worlds()
            .into_iter()
            .map(|world| world_summary(server, world))
            .collect::<Vec<_>>(),
    })
    .to_string()
}

fn worlds(server: &Server) -> String {
    Value::Array(
        server
            .get_all_worlds()
            .into_iter()
            .map(|world| world_summary(server, world))
            .collect(),
    )
    .to_string()
}

fn world_summary(server: &Server, world: World) -> Value {
    let mut summary = json!({
        "name": world.get_name(),
        "dimension": world.get_dimension(),
        "time_of_day": world.get_time_of_day(),
        "world_age_ticks": world.get_world_age(),
        "raining": world.is_raining(),
        "thundering": world.is_thundering(),
    });
    summary["players"] = server.get_player_count_in_world(world).into();
    summary
}

fn players(server: &Server) -> String {
    Value::Array(
        server
            .get_all_players()
            .iter()
            .map(|player| {
                let (x, y, z) = player.get_position();
                json!({
                    "name": player.get_name(),
                    "uuid": wit_uuid::to_string(player.get_id()),
                    "world": player.get_world().get_name(),
                    "position": {"x": x, "y": y, "z": z},
                    "game_mode": format!("{:?}", player.get_gamemode()),
                    "permission_level": format!("{:?}", player.get_permission_level()),
                    "ping_ms": player.get_ping(),
                })
            })
            .collect(),
    )
    .to_string()
}

fn entities(server: &Server, operator: &Player, input: &Value) -> Result<String, String> {
    let world = match input.get("world").and_then(Value::as_str).map(str::trim) {
        Some(name) if !name.is_empty() => server
            .get_world_by_name(name)
            .ok_or_else(|| format!("World not found: {name}"))?,
        _ => operator.get_world(),
    };
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_LISTED_ENTITIES)
        .clamp(1, MAX_LISTED_ENTITIES) as usize;
    let all = world.get_entities();
    let listed = all
        .iter()
        .take(limit)
        .map(|entity| {
            let (x, y, z) = entity.get_position();
            json!({
                "type": entity_type_id(entity.get_type()),
                "uuid": wit_uuid::to_string(entity.get_uuid()),
                "position": {"x": x, "y": y, "z": z},
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "world": world.get_name(),
        "total": all.len(),
        "entities": listed,
    })
    .to_string())
}

/// `EntityType::IronGolem` → `minecraft:iron_golem`, the spelling commands
/// accept. The bindings expose an entity type only as its variant, whose
/// Debug name is the WIT case in camel case; the cases contain no digits, so
/// the conversion is lossless.
fn entity_type_id(entity_type: EntityType) -> String {
    let debug = format!("{entity_type:?}");
    let variant = debug.rsplit("::").next().unwrap_or(&debug);
    let mut id = String::from("minecraft:");
    for (index, character) in variant.chars().enumerate() {
        if character.is_ascii_uppercase() {
            if index > 0 {
                id.push('_');
            }
            id.push(character.to_ascii_lowercase());
        } else {
            id.push(character);
        }
    }
    id
}

fn run_command(server: &Server, operator: Player, input: &Value) -> Result<String, String> {
    let command = input
        .get("command")
        .and_then(Value::as_str)
        .map(|command| command.trim().trim_start_matches('/'))
        .filter(|command| !command.is_empty())
        .ok_or("Missing the 'command' string.")?;
    server.execute_command(command, CommandSender::Player(operator));
    Ok("Command dispatched; its feedback went to the operator's chat.".into())
}

fn truncate(mut output: String) -> String {
    if output.len() <= MAX_OUTPUT_BYTES {
        return output;
    }
    let mut end = MAX_OUTPUT_BYTES - TRUNCATION_MARKER.len();
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    output.truncate(end);
    output.push_str(TRUNCATION_MARKER);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_types_use_command_spelling() {
        assert_eq!(
            entity_type_id(EntityType::IronGolem),
            "minecraft:iron_golem"
        );
        assert_eq!(entity_type_id(EntityType::Pig), "minecraft:pig");
        assert_eq!(
            entity_type_id(EntityType::AcaciaChestBoat),
            "minecraft:acacia_chest_boat"
        );
    }

    #[test]
    fn truncation_keeps_utf8_and_the_byte_limit() {
        let output = truncate("🎃".repeat(MAX_OUTPUT_BYTES));
        assert!(output.len() <= MAX_OUTPUT_BYTES);
        assert!(output.ends_with(TRUNCATION_MARKER));
        assert_eq!(truncate("short".into()), "short");
    }
}
