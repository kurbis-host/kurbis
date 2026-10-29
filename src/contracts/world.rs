//! What a server's world is generated from: its world type, the superflat or
//! single biome choices that go with it, structures and the seed.
//!
//! A world is generated once, so these are fixed for its lifetime. The owner
//! picks them when the world is reset; Pumpkin reads them only when it creates
//! a new world, as vanilla's `level-type`, `generator-settings`,
//! `generate-structures` and `level-seed` server properties.

use std::{collections::HashSet, sync::LazyLock};

use garde::Validate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

string_enum!(
    /// Vanilla's world presets on the Create New World screen.
    WorldType {
        Normal => "normal",
        Flat => "flat",
        LargeBiomes => "large_biomes",
        Amplified => "amplified",
        SingleBiomeSurface => "single_biome_surface",
    }
);

/// Most layers vanilla's superflat customization screen allows.
pub const MAX_FLAT_LAYERS: usize = 64;
/// The tallest world a superflat world can fill, from -64 to 320.
pub const MAX_FLAT_HEIGHT: i32 = 384;
/// Vanilla's limit on a typed seed.
pub const MAX_SEED_INPUT: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlatLayer {
    /// A block id such as `minecraft:stone`.
    pub block: String,
    pub height: i32,
}

/// A superflat world's layers, bottom first, and what grows on them, as
/// vanilla's `generator-settings` describe them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlatSettings {
    pub layers: Vec<FlatLayer>,
    /// A biome id such as `minecraft:plains`.
    pub biome: String,
    /// Place the biome's trees, flowers and ores.
    pub features: bool,
    /// Add lava lakes.
    pub lakes: bool,
    /// The structure sets this world may generate, such as
    /// `minecraft:villages`, when structures are on.
    pub structure_overrides: Vec<String>,
}

/// How a server's current world was generated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorldGeneration {
    pub world_type: WorldType,
    /// The layers of a superflat world; only with `flat`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flat: Option<FlatSettings>,
    /// The only biome of a single biome world; only with
    /// `single_biome_surface`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub biome: Option<String>,
    pub structures: bool,
    /// The world seed, as vanilla stores it. `None` for a world generated
    /// before Kurbis recorded seeds, until the worker reads it from the world.
    pub seed: Option<i64>,
}

impl Default for WorldGeneration {
    fn default() -> Self {
        Self {
            world_type: WorldType::Normal,
            flat: None,
            biome: None,
            structures: true,
            seed: None,
        }
    }
}

/// The world a reset generates. Everything but the type is optional: a
/// superflat world without layers is Classic Flat, a single biome world
/// without a biome is plains, and a missing seed is random.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct NewWorld {
    pub world_type: WorldType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flat: Option<FlatSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub biome: Option<String>,
    #[serde(default = "structures_default")]
    pub structures: bool,
    /// A number or any text, as typed into vanilla's seed field. Text that
    /// is not a number becomes its Java string hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
}

const fn structures_default() -> bool {
    true
}

impl Default for NewWorld {
    fn default() -> Self {
        Self {
            world_type: WorldType::Normal,
            flat: None,
            biome: None,
            structures: true,
            seed: None,
        }
    }
}

impl NewWorld {
    /// Check the choices against the world catalog and fix the seed: the
    /// generated world is then fully described before Pumpkin creates it.
    pub fn resolve(self, random_seed: i64) -> Result<WorldGeneration, String> {
        let flat = match (self.world_type, self.flat) {
            (WorldType::Flat, Some(flat)) => {
                validate_flat(&flat)?;
                Some(flat)
            }
            (WorldType::Flat, None) => Some(classic_flat()),
            (_, Some(_)) => return Err("only a superflat world has layers".to_owned()),
            (_, None) => None,
        };
        let biome = match (self.world_type, self.biome) {
            (WorldType::SingleBiomeSurface, Some(biome)) => {
                validate_biome(&biome)?;
                Some(biome)
            }
            (WorldType::SingleBiomeSurface, None) => Some("minecraft:plains".to_owned()),
            (_, Some(_)) => return Err("only a single biome world has a biome".to_owned()),
            (_, None) => None,
        };
        let seed = match self.seed.as_deref().map(str::trim) {
            None | Some("") => random_seed,
            Some(input) if input.chars().count() > MAX_SEED_INPUT => {
                return Err(format!("a seed is at most {MAX_SEED_INPUT} characters"));
            }
            Some(input) => parse_seed(input),
        };
        Ok(WorldGeneration {
            world_type: self.world_type,
            flat,
            biome,
            structures: self.structures,
            seed: Some(seed),
        })
    }
}

/// Vanilla's seed field: a number is the seed, other text its Java string
/// hash.
pub fn parse_seed(input: &str) -> i64 {
    input
        .parse::<i64>()
        .unwrap_or_else(|_| i64::from(java_string_hash(input)))
}

fn java_string_hash(input: &str) -> i32 {
    input.encode_utf16().fold(0_i32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(i32::from(unit))
    })
}

fn validate_flat(flat: &FlatSettings) -> Result<(), String> {
    if flat.layers.is_empty() || flat.layers.len() > MAX_FLAT_LAYERS {
        return Err(format!(
            "a superflat world has between 1 and {MAX_FLAT_LAYERS} layers"
        ));
    }
    let mut height = 0_i32;
    for layer in &flat.layers {
        if !CATALOG.block_ids.contains(layer.block.as_str()) {
            return Err(format!("unknown block {}", layer.block));
        }
        if layer.height < 1 {
            return Err("every layer is at least one block high".to_owned());
        }
        height = height.saturating_add(layer.height);
    }
    if height > MAX_FLAT_HEIGHT {
        return Err(format!(
            "the layers are at most {MAX_FLAT_HEIGHT} blocks high together"
        ));
    }
    validate_biome(&flat.biome)?;
    for structure in &flat.structure_overrides {
        if !CATALOG.structure_ids.contains(structure.as_str()) {
            return Err(format!("unknown structure {structure}"));
        }
    }
    Ok(())
}

fn validate_biome(biome: &str) -> Result<(), String> {
    if CATALOG.biome_ids.contains(biome) {
        Ok(())
    } else {
        Err(format!("unknown biome {biome}"))
    }
}

fn classic_flat() -> FlatSettings {
    CATALOG
        .catalog
        .flat_presets
        .iter()
        .find(|preset| preset.id == "classic_flat")
        .expect("the world catalog has Classic Flat")
        .settings
        .clone()
}

/// Pumpkin's new-world keys for `pumpkin.toml`. Pumpkin reads them only when
/// the world folder is empty, so an existing world keeps generating as it
/// was created.
pub fn render_pumpkin_world_config(world: &WorldGeneration) -> String {
    let level_type = format!("minecraft:{}", world.world_type.as_str());
    let generator_settings = match (&world.flat, &world.biome) {
        (Some(flat), _) => serde_json::to_string(flat).expect("flat settings serialize"),
        (None, Some(biome)) => serde_json::json!({ "biome": biome }).to_string(),
        (None, None) => "{}".to_owned(),
    };
    let toml_string = |value: &str| serde_json::to_string(value).expect("strings serialize");
    // An unknown seed only belongs to a world that already exists, which
    // Pumpkin never regenerates; an empty seed would make it pick one.
    let seed = world.seed.map(|seed| seed.to_string()).unwrap_or_default();
    format!(
        "seed = {}\nlevel_type = {}\ngenerator_settings = {}\ngenerate_structures = {}\n",
        toml_string(&seed),
        toml_string(&level_type),
        toml_string(&generator_settings),
        world.structures,
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CatalogEntry {
    pub id: String,
    /// Vanilla's English name.
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FlatPreset {
    pub id: String,
    pub name: String,
    pub settings: FlatSettings,
}

/// Everything a new world can be made of, generated from the pinned Pumpkin
/// by `tools/generate_world_catalog.py`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorldCatalog {
    pub world_types: Vec<CatalogEntry>,
    pub flat_presets: Vec<FlatPreset>,
    pub biomes: Vec<CatalogEntry>,
    pub blocks: Vec<CatalogEntry>,
    pub structures: Vec<CatalogEntry>,
}

pub struct IndexedCatalog {
    pub catalog: WorldCatalog,
    block_ids: HashSet<String>,
    biome_ids: HashSet<String>,
    structure_ids: HashSet<String>,
}

pub static CATALOG: LazyLock<IndexedCatalog> = LazyLock::new(|| {
    let catalog: WorldCatalog = serde_json::from_str(include_str!("world_catalog.json"))
        .expect("the world catalog is valid");
    let ids = |entries: &[CatalogEntry]| entries.iter().map(|entry| entry.id.clone()).collect();
    IndexedCatalog {
        block_ids: ids(&catalog.blocks),
        biome_ids: ids(&catalog.biomes),
        structure_ids: ids(&catalog.structures),
        catalog,
    }
});

#[cfg(test)]
mod tests {
    use super::*;

    fn new_world(world_type: WorldType) -> NewWorld {
        NewWorld {
            world_type,
            ..NewWorld::default()
        }
    }

    #[test]
    fn seeds_are_read_like_vanillas_seed_field() {
        assert_eq!(parse_seed("42"), 42);
        assert_eq!(parse_seed("-9223372036854775808"), i64::MIN);
        // Matches Pumpkin and vanilla: "kurbis".hashCode().
        assert_eq!(parse_seed("kurbis"), -1_120_102_460);
        assert_eq!(
            parse_seed("Glacier"),
            i64::from(java_string_hash("Glacier"))
        );
        // Out of range numbers are text.
        assert_eq!(
            parse_seed("9223372036854775808"),
            i64::from(java_string_hash("9223372036854775808"))
        );
    }

    #[test]
    fn a_missing_or_blank_seed_is_the_random_one() {
        let random = 7;
        assert_eq!(
            new_world(WorldType::Normal).resolve(random).unwrap().seed,
            Some(7)
        );
        let blank = NewWorld {
            seed: Some("  ".to_owned()),
            ..new_world(WorldType::Normal)
        };
        assert_eq!(blank.resolve(random).unwrap().seed, Some(7));
        let typed = NewWorld {
            seed: Some(" 12 ".to_owned()),
            ..new_world(WorldType::Normal)
        };
        assert_eq!(typed.resolve(random).unwrap().seed, Some(12));
        let long = NewWorld {
            seed: Some("x".repeat(MAX_SEED_INPUT + 1)),
            ..new_world(WorldType::Normal)
        };
        assert!(long.resolve(random).is_err());
    }

    #[test]
    fn world_types_fill_in_vanillas_defaults() {
        let flat = new_world(WorldType::Flat).resolve(1).unwrap();
        assert_eq!(flat.flat, Some(classic_flat()));
        assert_eq!(flat.biome, None);
        let single = new_world(WorldType::SingleBiomeSurface).resolve(1).unwrap();
        assert_eq!(single.biome.as_deref(), Some("minecraft:plains"));
        assert_eq!(single.flat, None);
        let amplified = new_world(WorldType::Amplified).resolve(1).unwrap();
        assert_eq!((amplified.flat, amplified.biome), (None, None));
    }

    #[test]
    fn choices_only_belong_to_their_world_type() {
        let layers_on_normal = NewWorld {
            flat: Some(classic_flat()),
            ..new_world(WorldType::Normal)
        };
        assert!(layers_on_normal.resolve(1).is_err());
        let biome_on_flat = NewWorld {
            biome: Some("minecraft:desert".to_owned()),
            ..new_world(WorldType::Flat)
        };
        assert!(biome_on_flat.resolve(1).is_err());
    }

    #[test]
    fn every_vanilla_preset_is_a_valid_superflat_world() {
        assert_eq!(CATALOG.catalog.flat_presets.len(), 9);
        for preset in &CATALOG.catalog.flat_presets {
            assert_eq!(validate_flat(&preset.settings), Ok(()), "{}", preset.id);
        }
    }

    #[test]
    fn superflat_layers_are_checked() {
        let with = |change: fn(&mut FlatSettings)| {
            let mut flat = classic_flat();
            change(&mut flat);
            NewWorld {
                flat: Some(flat),
                ..new_world(WorldType::Flat)
            }
            .resolve(1)
        };
        assert!(with(|flat| flat.layers.clear()).is_err());
        assert!(with(|flat| flat.layers[0].block = "minecraft:nonsense".to_owned()).is_err());
        assert!(with(|flat| flat.layers[0].height = 0).is_err());
        assert!(with(|flat| flat.layers[0].height = MAX_FLAT_HEIGHT).is_err());
        assert!(with(|flat| flat.layers[0].height = MAX_FLAT_HEIGHT - 3).is_ok());
        assert!(with(|flat| flat.biome = "minecraft:nonsense".to_owned()).is_err());
        assert!(
            with(|flat| flat.structure_overrides = vec!["minecraft:nonsense".to_owned()]).is_err()
        );
        assert!(
            with(|flat| flat.layers = vec![
                FlatLayer {
                    block: "minecraft:air".to_owned(),
                    height: 1
                };
                MAX_FLAT_LAYERS + 1
            ])
            .is_err()
        );
    }

    #[test]
    fn a_single_biome_world_needs_a_known_biome() {
        let with = |biome: &str| {
            NewWorld {
                biome: Some(biome.to_owned()),
                ..new_world(WorldType::SingleBiomeSurface)
            }
            .resolve(1)
        };
        assert_eq!(
            with("minecraft:cherry_grove").unwrap().biome.as_deref(),
            Some("minecraft:cherry_grove")
        );
        assert!(with("cherry_grove").is_err());
        assert!(with("minecraft:nonsense").is_err());
    }

    #[test]
    fn renders_the_keys_pumpkin_reads_for_a_new_world() {
        let normal = new_world(WorldType::Normal).resolve(-5).unwrap();
        assert_eq!(
            render_pumpkin_world_config(&normal),
            "seed = \"-5\"\nlevel_type = \"minecraft:normal\"\ngenerator_settings = \"{}\"\ngenerate_structures = true\n"
        );
        let void = CATALOG
            .catalog
            .flat_presets
            .iter()
            .find(|preset| preset.id == "the_void")
            .unwrap();
        let flat = NewWorld {
            flat: Some(void.settings.clone()),
            structures: false,
            ..new_world(WorldType::Flat)
        }
        .resolve(3)
        .unwrap();
        let config = render_pumpkin_world_config(&flat);
        assert!(config.contains("level_type = \"minecraft:flat\"\n"));
        assert!(config.contains(r#"generator_settings = "{\"layers\":[{\"block\":\"minecraft:air\",\"height\":1}],\"biome\":\"minecraft:the_void\",\"features\":true,\"lakes\":false,\"structure_overrides\":[]}""#));
        assert!(config.contains("generate_structures = false\n"));
        let single = NewWorld {
            biome: Some("minecraft:desert".to_owned()),
            ..new_world(WorldType::SingleBiomeSurface)
        }
        .resolve(3)
        .unwrap();
        assert!(
            render_pumpkin_world_config(&single)
                .contains(r#"generator_settings = "{\"biome\":\"minecraft:desert\"}""#)
        );
        assert!(
            render_pumpkin_world_config(&WorldGeneration::default()).starts_with("seed = \"\"\n")
        );
    }
}
