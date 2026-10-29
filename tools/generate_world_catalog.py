#!/usr/bin/env python3
"""Write src/contracts/world_catalog.json from a built Pumpkin checkout.

The catalog lists what a new world can be made of, with vanilla's English
names: the world types of the Create New World screen, the superflat presets,
every biome, every structure set a superflat world may turn on (vanilla names
none, so their names come from the ids) and every block that has an item,
plus air, water and lava, which the presets layer without one. Run it against the
pinned revision after `cargo build` has downloaded the vanilla datapack:

    tools/generate_world_catalog.py output/native-pumpkin/source
"""
import argparse
import json
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("pumpkin", type=Path, help="Pumpkin source directory with assets/datapack")
args = parser.parse_args()
assets = args.pumpkin / "assets"
worldgen = assets / "datapack/data/minecraft/worldgen"
names = json.loads((assets / "en_us_java.json").read_text())


def read(path: Path):
    return json.loads(path.read_text())


def namespaced(name: str) -> str:
    return name if ":" in name else f"minecraft:{name}"


def plain(name: str) -> str:
    return name.removeprefix("minecraft:")


# Vanilla's order on the Create New World screen and in the superflat list.
world_types = [
    {"id": plain(preset), "name": names[f"generator.{preset.replace(':', '.')}"]}
    for preset in read(assets / "datapack/data/minecraft/tags/worldgen/world_preset/normal.json")["values"]
]
flat_presets = []
for preset in read(assets / "datapack/data/minecraft/tags/worldgen/flat_level_generator_preset/visible.json")["values"]:
    settings = read(worldgen / f"flat_level_generator_preset/{plain(preset)}.json")["settings"]
    overrides = settings.get("structure_overrides", [])
    flat_presets.append({
        "id": plain(preset),
        "name": names[f"flat_world_preset.{preset.replace(':', '.')}"],
        "settings": {
            "layers": settings["layers"],
            "biome": settings["biome"],
            "features": settings.get("features", False),
            "lakes": settings.get("lakes", False),
            "structure_overrides": [overrides] if isinstance(overrides, str) else overrides,
        },
    })
biomes = sorted(
    ({"id": namespaced(path.stem), "name": names[f"biome.minecraft.{path.stem}"]}
     for path in (worldgen / "biome").glob("*.json")),
    key=lambda biome: biome["name"],
)
structures = sorted(
    ({"id": namespaced(path.stem), "name": path.stem.replace("_", " ").capitalize()}
     for path in (worldgen / "structure_set").glob("*.json")),
    key=lambda structure: structure["name"],
)
blocks_data = read(assets / "blocks.json")["blocks"]
air_item = next(block["item_id"] for block in blocks_data if block["name"] == "air")
blocks = sorted(
    ({"id": namespaced(block["name"]), "name": names[block["translation_key"]]}
     for block in blocks_data
     if block["name"] in ("air", "water", "lava") or block["item_id"] != air_item),
    key=lambda block: block["name"],
)
catalog = {"world_types": world_types, "flat_presets": flat_presets, "biomes": biomes,
           "blocks": blocks, "structures": structures}
target = Path(__file__).resolve().parent.parent / "src/contracts/world_catalog.json"
target.write_text(json.dumps(catalog, indent=1, ensure_ascii=False) + "\n")
print(f"{target}: {len(world_types)} world types, {len(flat_presets)} presets, {len(biomes)} biomes, {len(blocks)} blocks, {len(structures)} structures")
