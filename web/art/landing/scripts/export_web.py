"""Export the film for the browser (see kurbis/export.py for what and why).

    Blender -b --factory-startup -P scripts/export_web.py

Writes web/public/landing/ (git-ignored: it is made from the texture pack) and the
parity fixtures under web/tests/unit/landing/fixtures/ (numbers only).
"""

import hashlib
import json
import shutil
import sys
import time
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))
for _name in [k for k in sys.modules if k == "film" or k == "kurbis" or k.startswith("kurbis.")]:
    del sys.modules[_name]

import bpy  # noqa: E402
import numpy as np  # noqa: E402

import film  # noqa: E402
from kurbis import blocks as blk  # noqa: E402
from kurbis import core, export, sets, voxel  # noqa: E402
from kurbis import world as wld  # noqa: E402

FIXTURES = core.ROOT.parents[1] / "tests" / "unit" / "landing" / "fixtures"
VALLEY_FLOOR = -12  # faces at or above this height load with the hero; the rest follow
KEY_SUN = tuple(np.array((-0.47, -0.57, 0.67)) / np.linalg.norm((-0.47, -0.57, 0.67)))  # toward the key suns of the open-air stops, near enough
# Constants of the film the page reads rather than repeats.
CONSTANTS = (
    "PART", "END", "WAKE_UP", "FALL", "STOMP", "DIG0", "BREAK", "LAND", "WAKE", "ARRIVAL_RATE", "BOARD", "ARRIVE", "PORTAL_LIT", "THROUGH", "WHITE",
    "PACE", "POND_LEVEL", "SWELL", "SUN", "SUN_LIGHT", "HERO_EYE", "HERO_PUMPKIN", "LANDING_SPOT", "SLAB_RATE", "CAVE_PUMPKIN", "TORCHES", "LANTERNS",
    "SNAPSHOT", "SNAP_TILT", "LEVER", "NAMES", "ORES", "ORE_LINES", "SIGNS", "ASKED", "ORE_VIEW", "SPAWNER_VIEW",
)
# What the sets left in the film's shared state that is a place or a number.
POINTS = ("gate", "home_spot", "lever_side", "tile_spots", "sign_spots", "toys", "nether_click", "nether_free", "through", "steps", "cave_folded", "rail", "cast", "fishing", "drips")
# Pictures of the pack the interface shows: a toast's card and the icons on it.
HUD = {"toast": "gui/sprites/toast/advancement.png", "grass": "block/grass_block_side.png", "dirt": "block/dirt.png", "cave": "block/diamond_ore.png", "deeper": "block/obsidian.png", "pearl": "item/ender_pearl.png"}


def _dungeon_box():
    """The dungeon's shell in either frame (the tall one is higher), as lo and hi cells."""
    d = wld.SPOTS["dungeon"]
    (x0, x1), (y0, y1), floor = d["x"], d["y"], d["floor"]
    return np.array((x0 - 1, y0 - 1, floor - 2)), np.array((x1 + 3, y1 + 2, floor + 13))


def _front(z0):
    """Daylight standing outside the cut face at each height: full down to the dig,
    then less and less toward the cave, where only the moon looks in."""
    return lambda zi: int(np.clip(15 + (zi + z0 + 8) * 0.92, 4, 15))


def world_records(S, aspect, report):
    """The valley's faces, split into what the hero needs, what lies under it, and the
    dungeon (which differs by frame). Returns a digest of the shared part."""
    w, b = S["w"], S["b"]
    origin = np.asarray(w.origin, np.int32)
    day, lit, solid = export.levels(w.grid, b, sky=True, front=_front(w.z0))
    lo, hi = _dungeon_box()
    digest = hashlib.sha1()
    for group in ("opaque", "leaves", "water"):
        cells, which, tile = export.faces(w.grid, b, group, voxel.ALL_CLOSED_BUT_FRONT_AND_TOP)
        if not len(cells):
            continue
        shade = export.light(cells, which, day, lit, solid, sun=film.SUN_LIGHT, key=KEY_SUN, sun_floor=VALLEY_FLOOR - w.z0)
        world = cells + origin
        inside = np.all((world >= lo) & (world < hi), axis=1)
        high = world[:, 2] >= VALLEY_FLOOR
        name = {"opaque": "terrain", "leaves": "leaves", "water": "water"}[group]
        for part, pick in (("valley", high & ~inside), ("under", ~high & ~inside), (f"dungeon.{aspect}", inside)):
            if not pick.any():
                continue
            shared = not part.startswith("dungeon")
            if shared:
                digest.update(np.ascontiguousarray(world[pick]).tobytes() + which[pick].tobytes() + tile[pick].tobytes())
            if shared and aspect != "wide":
                continue  # written once, from the wide frame
            key = f"{part}.{name}" if shared else f"{part}"
            report[key] = export.write_records(export.OUT / f"{key}.bin", cells[pick], which[pick], tile[pick], shade[pick], origin)
    return digest.hexdigest()


def nether_records(S, report):
    grid, b = S["nether_grid"], S["b"]
    day, lit, solid = export.levels(grid.g, b, sky=False)
    cells, which, tile = export.faces(grid.g, b, "opaque")
    shade = export.light(cells, which, day, lit, solid)
    origin = np.asarray(sets.NETHER["origin"], np.int32) + np.asarray(grid.lo, np.int32)
    report["nether.terrain"] = export.write_records(export.OUT / "nether.terrain.bin", cells, which, tile, shade, origin)


def layout(S, shots, aspect):
    scene = S["scene"]
    built = [obj for obj in scene.objects if obj.type in ("MESH", "EMPTY") and core.noted(obj).get("kind") in ("type", "rim", "scrim", "flakes")]
    return {
        "aspect": aspect,
        "shots": [export.shot(s) for s in sorted(shots, key=lambda s: s.s0)],
        "nodes": [export.node(obj) for obj in built],
        "colors": {obj.name: export._list(obj.color) for obj in scene.objects if obj.type == "MESH" and not export.skipped(obj) and tuple(obj.color) != (1.0, 1.0, 1.0, 1.0)},
        "materials": {obj.name: obj.data.materials[0].name for obj in scene.objects if obj.type == "MESH" and not export.skipped(obj) and obj.data.materials and obj.data.materials[0] is not None},
        "lights": [export.lamp(obj) for obj in scene.objects if obj.type == "LIGHT"],
        "fog": [dict(core.noted(obj), name=obj.name) for obj in scene.objects if core.noted(obj).get("kind") == "fog"],
        "points": export.plain({key: S[key] for key in POINTS if key in S and not isinstance(S[key], (bpy.types.ID,))}),
        "tracks": {"dig": export.track(S["dig"]), "ride": export.track(S["ride"])},
        "ore_wall": export.plain(film.ore_wall()),
        "bands": export.plain(film.bands()),
    }


def export_all():
    t0 = time.time()
    out = export.OUT
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    FIXTURES.mkdir(parents=True, exist_ok=True)
    report, digests, used = {}, {}, {}
    for aspect in ("wide", "tall"):
        shots = film.stage("visit", aspect)
        S = film.S
        digests[aspect] = world_records(S, aspect, report)
        if aspect == "wide":
            nether_records(S, report)
        objects = [obj for obj in S["scene"].objects if obj.type in ("MESH", "EMPTY") and not export.skipped(obj)]
        report[f"models.{aspect}"] = export.write_glb(out / f"models.{aspect}.glb", objects)
        laid = layout(S, shots, aspect)
        (out / f"layout.{aspect}.json").write_text(json.dumps(laid, separators=(",", ":")))
        used.update(export.materials())
        ref = export.reference(shots, {"pumpkin": S["a"], "nether_pumpkin": S["n"]}, film.END)
        # What the page's own timeline is built from, so the test needs nothing but this file.
        ref.update(constants=export.plain({name: getattr(film, name) for name in CONSTANTS}), points=laid["points"], tracks=laid["tracks"], shots=laid["shots"])
        ref["type"] = {node["name"]: node["note"]["lines"] for node in laid["nodes"] if node.get("note", {}).get("kind") == "type"}  # what the type says, to hold the page's copy to
        (FIXTURES / f"reference.{aspect}.json").write_text(json.dumps(ref, separators=(",", ":")))
        if aspect == "wide":
            b = S["b"]
            for name in ("atlas_color", "atlas_emit"):
                shutil.copyfile(core.EXPORT / f"{name}.png", out / f"{name}.png")
            scene = {
                "sky": core.noted(S["scene"].world),
                "atlas": {"tiles": len(b.atlas.index), "across": b.atlas.n, "tile": blk.TILE},
                "palettes": blk.TYPE_PALETTES,
                "glyphs": export.glyphs(),
                "constants": export.plain({name: getattr(film, name) for name in CONSTANTS}),
                "spots": export.plain(wld.SPOTS),
                "nether": export.plain(sets.NETHER),
                "valley_floor": VALLEY_FLOOR,
            }
    scene["materials"] = used
    scene["textures"] = export.copy_textures(used)
    (out / "hud").mkdir(exist_ok=True)
    for key, rel in HUD.items():
        shutil.copyfile(core.TEX / rel, out / "hud" / f"{key}.png")
    scene["hud"] = {key: f"hud/{key}.png" for key in HUD}
    scene["records"] = report
    scene["same_world"] = digests["wide"] == digests["tall"]
    (out / "scene.json").write_text(json.dumps(scene, separators=(",", ":")))
    total = sum(f.stat().st_size for f in out.rglob("*") if f.is_file())
    return {"seconds": round(time.time() - t0, 1), "megabytes": round(total / 1e6, 2), "same_world": scene["same_world"], "files": {k: (v.get("count", v.get("objects")), v["bytes"]) for k, v in report.items()}}


if __name__ == "__main__":
    print("EXPORT " + json.dumps(export_all()))
