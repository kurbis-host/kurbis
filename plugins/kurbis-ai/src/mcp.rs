use crate::storage;
use kurbis_ai_protocol::{
    MAX_MCP_REQUEST_BYTES, MAX_MCP_RESULT_BYTES, McpActionRequest, McpActionResult, McpJobLease,
    now_ms, truncate_utf8, write_json,
};
use pumpkin_plugin_api::{
    EntityType, Player, Server, World, wit::pumpkin::plugin::server::CommandSender,
};
use serde_json::{Value, json};
use std::fs;
use uuid::Uuid;

const MAX_REQUEST_BYTES: u64 = MAX_MCP_REQUEST_BYTES;
const MAX_RESULT_BYTES: usize = MAX_MCP_RESULT_BYTES;
const MAX_ENTITIES: usize = 200;

pub fn poll(server: Server) {
    let directory = storage::path("mcp/requests");
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten().take(4) {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let request = match fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.file_type().is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() <= MAX_REQUEST_BYTES =>
            {
                fs::read(&path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<McpActionRequest>(&bytes).ok())
            }
            _ => None,
        };
        let _ = fs::remove_file(&path);
        let Some(request) = request else {
            continue;
        };
        pumpkin_plugin_api::logging::log(
            pumpkin_plugin_api::logging::LogLevel::Info,
            &format!(
                "Kurbis AI is handling MCP action {} for job {}",
                request.tool, request.job_id
            ),
        );
        let lease_path = storage::path(&format!("mcp/jobs/{}.json", request.job_id));
        let lease: Option<McpJobLease> = fs::symlink_metadata(&lease_path)
            .ok()
            .filter(|metadata| metadata.file_type().is_file() && !metadata.file_type().is_symlink())
            .and_then(|_| fs::read(&lease_path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let result = handle_request(&server, request, lease);
        let result_path = storage::path(&format!("mcp/results/{}.json", result.id));
        let audit_path = storage::path(&format!("mcp/audit/{}.json", result.id));
        if let Err(error) = write_json(&audit_path, &result) {
            pumpkin_plugin_api::logging::log(
                pumpkin_plugin_api::logging::LogLevel::Error,
                &format!("Kurbis AI could not record MCP audit result: {error}"),
            );
        }
        if let Err(error) = write_json(&result_path, &result) {
            pumpkin_plugin_api::logging::log(
                pumpkin_plugin_api::logging::LogLevel::Error,
                &format!("Kurbis AI could not persist MCP result: {error}"),
            );
        }
    }
}

fn handle_request(
    server: &Server,
    request: McpActionRequest,
    lease: Option<McpJobLease>,
) -> McpActionResult {
    let (operator_uuid, outcome) = match lease {
        Some(lease) if lease.job_id == request.job_id && lease.expires_unix_ms >= now_ms() => {
            let operator_uuid = lease.operator_uuid;
            let player = find_authorized_player(server, operator_uuid);
            let outcome = match player {
                Some(player) => execute_tool(server, player, &request.tool, &request.arguments),
                None => Err("The requesting operator is no longer online or authorized.".into()),
            };
            (operator_uuid, outcome)
        }
        _ => (Uuid::nil(), Err("This AI job is no longer active.".into())),
    };
    let (success, output) = match outcome {
        Ok(output) => (true, output),
        Err(error) => (false, error),
    };
    let mut output = output;
    if output.len() > MAX_RESULT_BYTES {
        output = format!(
            "{}… (truncated)",
            truncate_utf8(&output, MAX_RESULT_BYTES - 16)
        );
    }
    McpActionResult {
        id: request.id,
        job_id: request.job_id,
        tool: request.tool,
        operator_uuid,
        arguments: request.arguments,
        success,
        output,
    }
}

fn find_authorized_player(server: &Server, expected: Uuid) -> Option<Player> {
    server.get_all_players().into_iter().find(|player| {
        pumpkin_plugin_api::uuid::to_string(player.get_id()) == expected.to_string()
            && player.has_permission("kurbis-ai:command")
    })
}

fn execute_tool(
    server: &Server,
    operator: Player,
    name: &str,
    arguments: &Value,
) -> Result<String, String> {
    match name {
        "get_server_status" => Ok(serde_json::to_string(&json!({
            "player_count": server.get_player_count(),
            "max_players": server.get_max_players(),
            "difficulty": format!("{:?}", server.get_difficulty()),
            "tps": server.get_tps(),
            "mspt": server.get_mspt(),
            "worlds": server.get_all_worlds().into_iter().map(|world| json!({
                "name": world.get_name(),
                "dimension": world.get_dimension(),
                "world_age_ticks": world.get_world_age(),
                "time_of_day": world.get_time_of_day(),
                "raining": world.is_raining(),
                "thundering": world.is_thundering(),
            })).collect::<Vec<_>>(),
        }))
        .map_err(|error| error.to_string())?),
        "list_players" => {
            let players = server
                .get_all_players()
                .into_iter()
                .map(|player| {
                    let (x, y, z) = player.get_position();
                    let world = player.get_world();
                    json!({
                        "name": player.get_name(),
                        "uuid": pumpkin_plugin_api::uuid::to_string(player.get_id()),
                        "world": world.get_name(),
                        "dimension": world.get_dimension(),
                        "position": { "x": x, "y": y, "z": z },
                        "game_mode": format!("{:?}", player.get_gamemode()),
                        "permission_level": format!("{:?}", player.get_permission_level()),
                        "ping_ms": player.get_ping(),
                    })
                })
                .collect::<Vec<_>>();
            serde_json::to_string(&players).map_err(|error| error.to_string())
        }
        "list_worlds" => {
            let worlds = server
                .get_all_worlds()
                .into_iter()
                .map(|world| {
                    json!({
                        "name": world.get_name(),
                        "dimension": world.get_dimension(),
                        "world_age_ticks": world.get_world_age(),
                        "time_of_day": world.get_time_of_day(),
                        "raining": world.is_raining(),
                        "thundering": world.is_thundering(),
                        "players": server.get_player_count_in_world(world),
                    })
                })
                .collect::<Vec<_>>();
            serde_json::to_string(&worlds).map_err(|error| error.to_string())
        }
        "list_entities" => {
            let world = selected_world(server, &operator, arguments)?;
            let limit = arguments
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(100)
                .clamp(1, MAX_ENTITIES as u64) as usize;
            let entities = world
                .get_entities()
                .into_iter()
                .take(limit)
                .map(|entity| {
                    let (x, y, z) = entity.get_position();
                    json!({
                        "id": entity.get_id(),
                        "uuid": pumpkin_plugin_api::uuid::to_string(entity.get_uuid()),
                        "type": format!("{:?}", entity.get_type()),
                        "position": { "x": x, "y": y, "z": z },
                        "yaw": entity.get_yaw(),
                        "pitch": entity.get_pitch(),
                        "custom_name_visible": entity.is_custom_name_visible(),
                    })
                })
                .collect::<Vec<_>>();
            serde_json::to_string(&json!({"world": world.get_name(), "entities": entities}))
                .map_err(|error| error.to_string())
        }
        "spawn_entity" => {
            let raw_type = required_string(arguments, "entity_type")?;
            let entity_type = parse_entity_type(raw_type)
                .ok_or_else(|| format!("Unsupported Pumpkin entity type: {raw_type}"))?;
            let world = selected_world(server, &operator, arguments)?;
            let fallback = operator.get_position();
            let x = coordinate(arguments, "x", fallback.0)?;
            let y = coordinate(arguments, "y", fallback.1)?;
            let z = coordinate(arguments, "z", fallback.2)?;
            let entity = world.spawn_entity(entity_type, (x, y, z));
            Ok(serde_json::to_string(&json!({
                "spawned": true,
                "type": format!("{:?}", entity.get_type()),
                "uuid": pumpkin_plugin_api::uuid::to_string(entity.get_uuid()),
                "world": world.get_name(),
                "position": { "x": x, "y": y, "z": z },
            }))
            .map_err(|error| error.to_string())?)
        }
        "run_server_command" => {
            let command = required_string(arguments, "command")?;
            if command.len() > 2048 || command.chars().any(|c| c == '\n' || c == '\r' || c == '\0')
            {
                return Err(
                    "Command must be at most 2048 bytes and contain no line breaks.".into(),
                );
            }
            let command = command.trim().trim_start_matches('/');
            if command.is_empty() {
                return Err("Command cannot be empty.".into());
            }
            server.execute_command(command, CommandSender::Player(operator));
            Ok("Dispatched immediately as the currently authorized operator.".into())
        }
        _ => Err(format!("Unknown Kurbis AI tool: {name}")),
    }
}

fn selected_world(server: &Server, operator: &Player, arguments: &Value) -> Result<World, String> {
    match arguments.get("world").and_then(Value::as_str) {
        Some(name) if !name.trim().is_empty() => server
            .get_world_by_name(name)
            .ok_or_else(|| format!("World not found: {name}")),
        _ => Ok(operator.get_world()),
    }
}

fn required_string<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Missing or empty '{key}' string"))
}

fn coordinate(arguments: &Value, key: &str, fallback: f64) -> Result<f64, String> {
    match arguments.get(key) {
        None => Ok(fallback),
        Some(value) => value
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("'{key}' must be a finite number")),
    }
}

fn parse_entity_type(input: &str) -> Option<EntityType> {
    let input = input.trim().to_ascii_lowercase();
    let input = input
        .strip_prefix("minecraft:")
        .unwrap_or(&input)
        .replace('_', "-");
    match input.as_str() {
        "acacia-boat" => Some(EntityType::AcaciaBoat),
        "acacia-chest-boat" => Some(EntityType::AcaciaChestBoat),
        "allay" => Some(EntityType::Allay),
        "area-effect-cloud" => Some(EntityType::AreaEffectCloud),
        "armadillo" => Some(EntityType::Armadillo),
        "armor-stand" => Some(EntityType::ArmorStand),
        "arrow" => Some(EntityType::Arrow),
        "axolotl" => Some(EntityType::Axolotl),
        "bamboo-chest-raft" => Some(EntityType::BambooChestRaft),
        "bamboo-raft" => Some(EntityType::BambooRaft),
        "bat" => Some(EntityType::Bat),
        "bee" => Some(EntityType::Bee),
        "birch-boat" => Some(EntityType::BirchBoat),
        "birch-chest-boat" => Some(EntityType::BirchChestBoat),
        "blaze" => Some(EntityType::Blaze),
        "block-display" => Some(EntityType::BlockDisplay),
        "bogged" => Some(EntityType::Bogged),
        "breeze" => Some(EntityType::Breeze),
        "breeze-wind-charge" => Some(EntityType::BreezeWindCharge),
        "camel" => Some(EntityType::Camel),
        "camel-husk" => Some(EntityType::CamelHusk),
        "cat" => Some(EntityType::Cat),
        "cave-spider" => Some(EntityType::CaveSpider),
        "cherry-boat" => Some(EntityType::CherryBoat),
        "cherry-chest-boat" => Some(EntityType::CherryChestBoat),
        "chest-minecart" => Some(EntityType::ChestMinecart),
        "chicken" => Some(EntityType::Chicken),
        "cod" => Some(EntityType::Cod),
        "command-block-minecart" => Some(EntityType::CommandBlockMinecart),
        "copper-golem" => Some(EntityType::CopperGolem),
        "cow" => Some(EntityType::Cow),
        "creaking" => Some(EntityType::Creaking),
        "creeper" => Some(EntityType::Creeper),
        "cushion" => Some(EntityType::Cushion),
        "dark-oak-boat" => Some(EntityType::DarkOakBoat),
        "dark-oak-chest-boat" => Some(EntityType::DarkOakChestBoat),
        "dolphin" => Some(EntityType::Dolphin),
        "donkey" => Some(EntityType::Donkey),
        "dragon-fireball" => Some(EntityType::DragonFireball),
        "drowned" => Some(EntityType::Drowned),
        "egg" => Some(EntityType::Egg),
        "elder-guardian" => Some(EntityType::ElderGuardian),
        "end-crystal" => Some(EntityType::EndCrystal),
        "ender-dragon" => Some(EntityType::EnderDragon),
        "ender-pearl" => Some(EntityType::EnderPearl),
        "enderman" => Some(EntityType::Enderman),
        "endermite" => Some(EntityType::Endermite),
        "evoker" => Some(EntityType::Evoker),
        "evoker-fangs" => Some(EntityType::EvokerFangs),
        "experience-bottle" => Some(EntityType::ExperienceBottle),
        "experience-orb" => Some(EntityType::ExperienceOrb),
        "eye-of-ender" => Some(EntityType::EyeOfEnder),
        "falling-block" => Some(EntityType::FallingBlock),
        "fireball" => Some(EntityType::Fireball),
        "firework-rocket" => Some(EntityType::FireworkRocket),
        "fishing-bobber" => Some(EntityType::FishingBobber),
        "fox" => Some(EntityType::Fox),
        "frog" => Some(EntityType::Frog),
        "furnace-minecart" => Some(EntityType::FurnaceMinecart),
        "ghast" => Some(EntityType::Ghast),
        "giant" => Some(EntityType::Giant),
        "glow-item-frame" => Some(EntityType::GlowItemFrame),
        "glow-squid" => Some(EntityType::GlowSquid),
        "goat" => Some(EntityType::Goat),
        "guardian" => Some(EntityType::Guardian),
        "happy-ghast" => Some(EntityType::HappyGhast),
        "hoglin" => Some(EntityType::Hoglin),
        "hopper-minecart" => Some(EntityType::HopperMinecart),
        "horse" => Some(EntityType::Horse),
        "husk" => Some(EntityType::Husk),
        "illusioner" => Some(EntityType::Illusioner),
        "interaction" => Some(EntityType::Interaction),
        "iron-golem" => Some(EntityType::IronGolem),
        "item" => Some(EntityType::Item),
        "item-display" => Some(EntityType::ItemDisplay),
        "item-frame" => Some(EntityType::ItemFrame),
        "jungle-boat" => Some(EntityType::JungleBoat),
        "jungle-chest-boat" => Some(EntityType::JungleChestBoat),
        "leash-knot" => Some(EntityType::LeashKnot),
        "lightning-bolt" => Some(EntityType::LightningBolt),
        "lingering-potion" => Some(EntityType::LingeringPotion),
        "llama" => Some(EntityType::Llama),
        "llama-spit" => Some(EntityType::LlamaSpit),
        "magma-cube" => Some(EntityType::MagmaCube),
        "mangrove-boat" => Some(EntityType::MangroveBoat),
        "mangrove-chest-boat" => Some(EntityType::MangroveChestBoat),
        "mannequin" => Some(EntityType::Mannequin),
        "marker" => Some(EntityType::Marker),
        "minecart" => Some(EntityType::Minecart),
        "mooshroom" => Some(EntityType::Mooshroom),
        "mule" => Some(EntityType::Mule),
        "nautilus" => Some(EntityType::Nautilus),
        "oak-boat" => Some(EntityType::OakBoat),
        "oak-chest-boat" => Some(EntityType::OakChestBoat),
        "ocelot" => Some(EntityType::Ocelot),
        "ominous-item-spawner" => Some(EntityType::OminousItemSpawner),
        "painting" => Some(EntityType::Painting),
        "pale-oak-boat" => Some(EntityType::PaleOakBoat),
        "pale-oak-chest-boat" => Some(EntityType::PaleOakChestBoat),
        "panda" => Some(EntityType::Panda),
        "parched" => Some(EntityType::Parched),
        "parrot" => Some(EntityType::Parrot),
        "phantom" => Some(EntityType::Phantom),
        "pig" => Some(EntityType::Pig),
        "piglin" => Some(EntityType::Piglin),
        "piglin-brute" => Some(EntityType::PiglinBrute),
        "pillager" => Some(EntityType::Pillager),
        "player" => Some(EntityType::Player),
        "polar-bear" => Some(EntityType::PolarBear),
        "poplar-boat" => Some(EntityType::PoplarBoat),
        "poplar-chest-boat" => Some(EntityType::PoplarChestBoat),
        "pufferfish" => Some(EntityType::Pufferfish),
        "rabbit" => Some(EntityType::Rabbit),
        "ravager" => Some(EntityType::Ravager),
        "salmon" => Some(EntityType::Salmon),
        "sheep" => Some(EntityType::Sheep),
        "shulker" => Some(EntityType::Shulker),
        "shulker-bullet" => Some(EntityType::ShulkerBullet),
        "silverfish" => Some(EntityType::Silverfish),
        "skeleton" => Some(EntityType::Skeleton),
        "skeleton-horse" => Some(EntityType::SkeletonHorse),
        "slime" => Some(EntityType::Slime),
        "small-fireball" => Some(EntityType::SmallFireball),
        "sniffer" => Some(EntityType::Sniffer),
        "snow-golem" => Some(EntityType::SnowGolem),
        "snowball" => Some(EntityType::Snowball),
        "spawner-minecart" => Some(EntityType::SpawnerMinecart),
        "spectral-arrow" => Some(EntityType::SpectralArrow),
        "spider" => Some(EntityType::Spider),
        "splash-potion" => Some(EntityType::SplashPotion),
        "spruce-boat" => Some(EntityType::SpruceBoat),
        "spruce-chest-boat" => Some(EntityType::SpruceChestBoat),
        "squid" => Some(EntityType::Squid),
        "stray" => Some(EntityType::Stray),
        "strider" => Some(EntityType::Strider),
        "sulfur-cube" => Some(EntityType::SulfurCube),
        "tadpole" => Some(EntityType::Tadpole),
        "text-display" => Some(EntityType::TextDisplay),
        "tnt" => Some(EntityType::Tnt),
        "tnt-minecart" => Some(EntityType::TntMinecart),
        "trader-llama" => Some(EntityType::TraderLlama),
        "trident" => Some(EntityType::Trident),
        "tropical-fish" => Some(EntityType::TropicalFish),
        "turtle" => Some(EntityType::Turtle),
        "vex" => Some(EntityType::Vex),
        "villager" => Some(EntityType::Villager),
        "vindicator" => Some(EntityType::Vindicator),
        "wandering-trader" => Some(EntityType::WanderingTrader),
        "warden" => Some(EntityType::Warden),
        "wind-charge" => Some(EntityType::WindCharge),
        "witch" => Some(EntityType::Witch),
        "wither" => Some(EntityType::Wither),
        "wither-skeleton" => Some(EntityType::WitherSkeleton),
        "wither-skull" => Some(EntityType::WitherSkull),
        "wolf" => Some(EntityType::Wolf),
        "zoglin" => Some(EntityType::Zoglin),
        "zombie" => Some(EntityType::Zombie),
        "zombie-horse" => Some(EntityType::ZombieHorse),
        "zombie-nautilus" => Some(EntityType::ZombieNautilus),
        "zombie-villager" => Some(EntityType::ZombieVillager),
        "zombified-piglin" => Some(EntityType::ZombifiedPiglin),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_names_accept_namespaces_and_underscores() {
        assert!(parse_entity_type("pig").is_some());
        assert!(parse_entity_type("minecraft:iron_golem").is_some());
        assert!(parse_entity_type("minecraft:zombie-villager").is_some());
        assert!(parse_entity_type("missing").is_none());
    }

    #[test]
    fn result_truncation_keeps_utf8_valid() {
        let output = format!("{}tail", "🎃".repeat(MAX_RESULT_BYTES));
        let bounded = truncate_utf8(&output, MAX_RESULT_BYTES - 16);
        assert!(bounded.is_char_boundary(bounded.len()));
    }

    #[test]
    fn coordinate_validation_rejects_non_numbers() {
        assert_eq!(coordinate(&json!({}), "x", 1.0).unwrap(), 1.0);
        assert!(coordinate(&json!({"x":"1"}), "x", 1.0).is_err());
        assert!(coordinate(&json!({"x":f64::INFINITY}), "x", 1.0).is_err());
    }
}
