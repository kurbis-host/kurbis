"""Export the film for the browser (see kurbis/export.py for what and why).

    Blender -b --factory-startup -P scripts/export_web.py

Writes web/public/landing/ (made from the texture pack while the page is a proof of
concept), the parity fixtures under web/tests/unit/landing/fixtures/ (numbers and
our own copy only), and the bundle's version, web/lib/landing/engine/version.ts.
Also the few pictures of the pack the site's own HTML pages show, web/public/gui/.
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
from kurbis import core, export, hud, sets, voxel  # noqa: E402
from kurbis import world as wld  # noqa: E402

FIXTURES = core.ROOT.parents[1] / "tests" / "unit" / "landing" / "fixtures"
VERSION = core.ROOT.parents[1] / "lib" / "landing" / "engine" / "version.ts"
VALLEY_FLOOR = -12  # faces at or above this height are the valley proper (the suns light nothing below it); the rest lies under it
KEY_SUN = tuple(np.array((-0.47, -0.57, 0.67)) / np.linalg.norm((-0.47, -0.57, 0.67)))  # toward the key suns of the open-air stops, near enough
# Constants of the film the page reads rather than repeats.
CONSTANTS = (
    "PART", "END", "WAKE_UP", "FALL", "STOMP", "DIG0", "BREAK", "LAND", "WAKE", "ARRIVAL_RATE", "BOARD", "ARRIVE", "PORTAL_LIT", "THROUGH", "WHITE", "NAME_STOP", "HOME", "SWIRL",
    "PACE", "POND_LEVEL", "SWELL", "SUN", "SUN_LIGHT", "HERO_EYE", "HERO_PUMPKIN", "LANDING_SPOT", "SLAB_RATE", "CAVE_PUMPKIN", "TORCHES", "LANTERNS",
    "SNAPSHOT", "SNAP_TILT", "LEVER", "NAMES", "BEATS", "REWIND", "HEADS", "YARD", "HOUSE",
)
# What the sets left in the film's shared state that is a place or a number.
POINTS = ("ending", "gate", "lever_side", "home_spots", "fence_posts", "toys", "rule", "nether_click", "nether_free", "nether_sock", "through", "steps", "cave_folded", "rail", "cast", "fishing", "drips", "jockey")
# Pictures of the pack the interface shows: a toast's card and the icons on it. (The page
# loads every key; "wood" is also the sides of the block a punched tree drops, and "boot"
# and "boot_overlay" are the leather boot the friends fish up now and then, film/life.ts.
# "bed", the cave asleep, and "shield", the stranger turned away at the gate, are drawn
# here from the pack's skins for them, which have no pictures of their own: kurbis/hud.py.)
HUD ={"toast": "gui/sprites/toast/advancement.png", "grass": "block/grass_block_side.png", "dirt": "block/dirt.png", "cave": "block/diamond_ore.png", "deeper": "block/obsidian.png", "pearl": "item/ender_pearl.png", "wood": "block/oak_log.png",
      "boot": "item/leather_boots.png", "boot_overlay": "item/leather_boots_overlay.png", "bed": hud.bed, "minecart": "item/minecart.png", "shield": hud.shield,
      "end": "block/end_stone.png"}  # "The End", as the credits come to rest (film/credits.ts)
# Pictures of the pack the site's own HTML pages show, as files of their own beside the
# bundle rather than in it (the bundle is written afresh and its pictures packed into one
# file, which a plain page cannot read): the dirt the game's old menu screens are tiled
# with, behind the page for an address with nothing at it (app/not-found.tsx).
GUI = {"dirt": "block/dirt.png"}
GUI_OUT = export.OUT.parent / "gui"


def _dungeon_box():
    """The dungeon's shell in either frame (the tall one is higher), as lo and hi cells."""
    d = wld.SPOTS["dungeon"]
    (x0, x1), (y0, y1), floor = d["x"], d["y"], d["floor"]
    return np.array((x0 - 1, y0 - 1, floor - 2)), np.array((x1 + 3, y1 + 2, floor + 13))


def _front(z0):
    """Daylight standing outside the cut face at each height: full down to the dig,
    then less and less toward the cave, where only the moon looks in."""
    return lambda zi: int(np.clip(15 + (zi + z0 + 8) * 0.92, 4, 15))


def _parts():
    """What the valley's faces are sorted into: what the first frame needs (the valley
    proper), what lies under it, and the dungeon (which differs by frame)."""
    lo, hi = _dungeon_box()
    dungeon = [*map(int, lo), *map(int, hi)]
    above = [-32768, -32768, VALLEY_FLOOR, 32767, 32767, 32767]
    return [
        {"name": "valley.terrain", "group": "opaque", "inside": [above], "outside": [dungeon]},
        {"name": "valley.leaves", "group": "leaves", "inside": [above], "outside": [dungeon]},
        {"name": "valley.water", "group": "water", "inside": [above], "outside": [dungeon]},
        {"name": "under.terrain", "group": "opaque", "outside": [above, dungeon]},
        {"name": "under.water", "group": "water", "outside": [above, dungeon]},
        {"name": "dungeon", "group": "opaque", "inside": [dungeon]},
    ]


def _sorted(parts, cells, which, tile, shade, origin, group, only=None):
    """The digest of each part of `group` among these faces (see `_parts`)."""
    world = cells + np.asarray(origin, np.int32)
    within = lambda boxes: np.any([np.all((world >= box[:3]) & (world < box[3:]), axis=1) for box in np.asarray(boxes)], axis=0)  # noqa: E731
    out = {}
    for part in parts:
        if part["group"] != group or (only is not None and part["name"] not in only):
            continue
        pick = np.ones(len(world), bool)
        if "inside" in part:
            pick &= within(part["inside"])
        if "outside" in part:
            pick &= ~within(part["outside"])
        out[part["name"]] = export.digest(cells[pick], which[pick], tile[pick], shade[pick], origin)
    return out


def world_grids(S, aspect, report, reference, wide):
    """The valley as its blocks, for the page to build (once, from the wide frame; the
    tall frame's dungeon is a patch laid over it), and what building it comes to here:
    the digests the page's own building is held to (tests/unit/landing/world.test.ts)."""
    w, b = S["w"], S["b"]
    closed = voxel.ALL_CLOSED_BUT_FRONT_AND_TOP
    front = _front(w.z0)
    floor = VALLEY_FLOOR - w.z0
    parts = _parts()
    if aspect != "wide" and not np.array_equal(wide["grid"], w.grid):
        report["dungeon.tall"] = export.write_patch(export.OUT / "dungeon.tall.patch.gz", wide["grid"], w.grid, b, closed)
    day, lit, solid = export.levels(w.grid, b, sky=True, front=front)
    mine, shared, lit_faces = {}, hashlib.sha1(), []
    for group in ("opaque", "leaves", "water"):
        cells, which, tile = export.faces(w.grid, b, group, closed)
        world = cells + np.asarray(w.origin, np.int32)
        lo, hi = _dungeon_box()
        apart = np.all((world >= lo) & (world < hi), axis=1)
        shared.update(np.ascontiguousarray(world[~apart]).tobytes() + which[~apart].tobytes() + tile[~apart].tobytes())
        if aspect != "wide":
            # Outside the dungeon the world is the same in both frames (see "same_world"):
            # only the dungeon is worked out again.
            cells, which, tile = cells[apart], which[apart], tile[apart]
        if not len(cells):
            continue
        shade = export.light(cells, which, day, lit, solid, sun=film.SUN_LIGHT, key=KEY_SUN, sun_floor=floor)
        lit_faces.append((cells, which, shade))
        mine.update(_sorted(parts, cells, which, tile, shade, w.origin, group, only=None if aspect == "wide" else ("dungeon",)))
    reference[aspect] = {**reference.get("wide", {}), **mine}
    if aspect == "wide":
        wide["grid"] = w.grid.copy()
        # The suns' shadows go with the blocks: the page builds everything else itself.
        # (They are the same in the tall frame: its dungeon lies below anything a sun
        # lights, and nothing else differs. See "same_world".)
        suns = [{"column": 8, "toward": film.SUN_LIGHT, "floor": floor}, {"column": 12, "toward": KEY_SUN, "floor": floor}]
        shadows = export.shadows(*(np.concatenate([faces[i] for faces in lit_faces]) for i in range(3)), suns)
        report["valley"] = export.write_grid(export.OUT / "valley.grid.gz", w.grid, b, w.origin, parts, closed=closed, sky=True, front=[front(z) for z in range(w.grid.shape[2])], shadows=shadows)
        origin, slab = w.front
        report["slab"] = export.write_model(export.OUT / "slab.grid", slab, b, origin, "Voxel", S["slab"])
        # The homestead's house: block by block, for the creeper to scatter.
        cells, pieces, origin = film.home_world()
        report["home"] = export.write_model(export.OUT / "home.grid", cells, b, origin, "Voxel", pieces)
    return shared.hexdigest()


def nether_grid(S, report, reference):
    grid, b = S["nether_grid"], S["b"]
    origin = np.asarray(sets.NETHER["origin"], np.int32) + np.asarray(grid.lo, np.int32)
    report["nether"] = export.write_grid(export.OUT / "nether.grid.gz", grid.g, b, origin, [{"name": "nether.terrain", "group": "opaque"}], sky=False)
    day, lit, solid = export.levels(grid.g, b, sky=False)
    cells, which, tile = export.faces(grid.g, b, "opaque")
    reference["nether"] = {"nether.terrain": export.digest(cells, which, tile, export.light(cells, which, day, lit, solid), origin)}


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
        "bands": export.plain(film.bands()),
    }


def gui_pictures():
    """The pack's pictures for the site's own pages (see `GUI`), each as it is."""
    GUI_OUT.mkdir(exist_ok=True)
    for key, rel in GUI.items():
        shutil.copyfile(core.TEX / rel, GUI_OUT / f"{key}.png")


def export_all():
    t0 = time.time()
    out = export.OUT
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    FIXTURES.mkdir(parents=True, exist_ok=True)
    report, digests, used, reference, wide = {}, {}, {}, {}, {}
    for aspect in ("wide", "tall"):
        shots = film.stage("visit", aspect)
        S = film.S
        digests[aspect] = world_grids(S, aspect, report, reference, wide)
        if aspect == "wide":
            nether_grid(S, report, reference)
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
        if callable(rel):
            core.image_from_pixels(f"hud_{key}", rel(), out / "hud" / f"{key}.png")
        else:
            shutil.copyfile(core.TEX / rel, out / "hud" / f"{key}.png")
    scene["hud"] = {key: f"hud/{key}.png" for key in HUD}
    gui_pictures()
    scene["files"] = report
    scene["same_world"] = digests["wide"] == digests["tall"]
    if not scene["same_world"]:
        raise RuntimeError("the valley outside the dungeon differs between the frames: the page builds both from one grid")
    # Everything small the page reads whole (pictures, the slab's cells) goes out as one file.
    small = [f for f in out.rglob("*") if f.is_file() and (f.suffix == ".png" or f.name in ("slab.grid", "home.grid"))]
    report["small"] = export.write_pack(out / "small.pack.gz", out, small)
    (out / "scene.json").write_text(json.dumps(scene, separators=(",", ":")))
    (FIXTURES / "world.json").write_text(json.dumps(reference, separators=(",", ":")))
    files = sorted(f for f in out.rglob("*") if f.is_file())
    # The bundle's version: the page asks for every file by it, so a browser may keep them for good.
    version = hashlib.sha256()
    for f in files:
        version.update(f.relative_to(out).as_posix().encode() + b"\0" + f.read_bytes())
    VERSION.write_text(
        "// Written by art/landing/scripts/export_web.py with the bundle: do not edit.\n"
        "// A digest of every file under public/landing, which the page asks for them by.\n"
        f'export const BUNDLE = "{version.hexdigest()[:16]}";\n'
    )
    total = sum(f.stat().st_size for f in files)
    return {"seconds": round(time.time() - t0, 1), "megabytes": round(total / 1e6, 2), "version": version.hexdigest()[:16], "files": {k: (v.get("cells", v.get("objects", v.get("files"))), v["bytes"]) for k, v in report.items()}}


if __name__ == "__main__":
    print("EXPORT " + json.dumps(export_all()))
