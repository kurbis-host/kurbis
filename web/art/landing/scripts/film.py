"""The film: one take from the sky to the Nether, written as a function of scroll.

Run inside Blender (Blender MCP, or `blender -b -P film.py`). `build(session)` makes
every set, light, actor and curve for one way of scrolling through the page and
saves blend/film_<session>.blend, which `render_range.py` renders headless.

Positions on the timeline are scroll positions in screens (one screen is one
viewport height of scroll). The camera and the story are scrubbed by the scroll;
life runs in real time on top (see kurbis/anim.py).

Every part has its own camera move and every transition its own in-world device.
Where the camera changes set, the frame is fully covered (the portal swirl), so on
screen it stays one continuous move.
"""

import json
import math
import sys
import time
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))
for _name in [k for k in sys.modules if k == "kurbis" or k.startswith("kurbis.")]:
    del sys.modules[_name]

import bpy  # noqa: E402
import numpy as np  # noqa: E402
from mathutils import Euler, Matrix, Vector  # noqa: E402

from kurbis import anim, boxmodel, core, font, hud, life, look, mascot, scenery, sets, voxel  # noqa: E402
from kurbis import blocks as blk  # noqa: E402
from kurbis import world as wld  # noqa: E402
from kurbis.anim import EVER, FPS, Shot, after, lerp, light_ramp, light_window, live, pop, pose, ramp, scrub, show, smooth, switch, unpop  # noqa: E402

# The sun sits on the horizon straight down the valley. The lamp is a little
# higher than the disc so the forest keeps some direct light (a standard cheat).
SUN = Vector((-0.035, 1.0, 0.062)).normalized()
SUN_LIGHT = Vector((-0.035, 1.0, 0.135)).normalized()

# Where each part of the journey begins, in screens of scroll. A stop is a part
# where the camera only drifts; the others are the moves between them.
PART = {
    "sky": 0.0,  # stop
    "dive": 0.25,
    "surface": 1.15,  # stop
    "slice": 1.45,
    "dig": 1.75,
    "arrival": 2.75,
    "cave": 3.25,  # stop
    "cart": 3.55,
    "dungeon": 4.05,  # stop
    "portal": 4.75,
    "nether": 5.55,  # stop
    "end": 6.2,
}
END = PART["end"]

# Moments of the story, as scroll positions.
WAKE_UP = 0.25  # the pumpkin wakes on its cloud
FALL = (0.38, 0.86)  # it falls from the cloud to the meadow
STOMP = 1.52  # its stomp breaks the ground open
DIG0 = 1.60  # it starts to dig
BREAK = 2.58  # it comes through the roof of the cave
LAND = 2.80  # and lands on the cave floor
WAKE = 3.02  # it reaches the snapshot block, and the cave wakes
ARRIVAL_RATE = 9.0  # seconds of the waking sequence that one screen of scroll plays on arrival
BOARD = 3.64  # it is in the cart
ARRIVE = 4.03  # the cart stops in the dungeon
PORTAL_LIT = 4.90  # it jumps into the frame, and the portal lights
THROUGH = 5.26  # the camera is in the portal: the set changes here, under the swirl
WHITE = (0.645, 0.66, 0.705, 0.76)  # the cloud the dive goes through: closing, white, white, clear

PACE = 2.0  # seconds per screen in the previs
FRAME = {"tall": False}  # which way up the frame is: wide (16:9) or tall (9:16)


def P(wide, tall):
    """This for the wide frame, that for the tall one: the page picks a layout by aspect."""
    return tall if FRAME["tall"] else wide


POND_LEVEL, SWELL = 1.0, 0.045  # the pond's surface, and how high its swell runs
# Help for type that stands on a ground as bright as itself (the gold on the sunset):
# a dark rim round the letters, in cells, and a soft dark card behind them.
HELP = {"rim": 0.2, "scrim": 0.0}
S = {}  # scene state shared by the builders


# What the visitor does while resting at each stop: seconds into the rest.
AT_SKY = {"sky_hover": 1.0, "sky_click": 2.6, "cloud_click": 4.6}
AT_SURFACE = {"pond_click": 1.8, "friend_click": 4.2, "tree_click": 6.4, "pumpkin_click": 8.4}
AT_CAVE = {"lever_hover": 1.4, "lever_off": 2.6, "lever_on": 8.0}
AT_ORES = {"tile_2": 1.4, "tile_5": 3.9}  # points at one tile of the ore wall, then at the chest
AT_SPAWNER = {"sign_1": 2.0}  # clicks the second sign
AT_NETHER = {"name_focus": 1.2, "name_type": 2.0, "name_enter": 5.6}


def sessions():
    """Ways of scrolling through the page. `scroll` never stops; `visit` rests at every
    stop, which is where life and the visitor's clicks show. The others scroll to one
    stop and stay there, to look at its life on its own."""
    return {
        "scroll": anim.Session("scroll", END, PACE),
        "visit": anim.Session("visit", END, PACE, lead=1.0, tail=1.5, stops=[(0.12, 7.0, AT_SKY), (1.30, 10.5, AT_SURFACE), (3.40, 12.5, AT_CAVE), (4.22, 7.2, AT_ORES), (4.62, 11.0, AT_SPAWNER), (END, 8.0, AT_NETHER)]),
        # The end: the visitor types a name, it is free, the frame lights, Enter.
        "nether": anim.Session("nether", END, PACE, lead=0.0, tail=1.5, stops=[(END, 9.0, AT_NETHER)]),
        # The dungeon: left alone, its two chambers show themselves.
        "dungeon": anim.Session("dungeon", 4.62, PACE, lead=0.0, tail=1.0, stops=[(4.22, 14.0), (4.62, 21.0)]),
        # The first two stops only: left alone, and played with.
        "idle": anim.Session("idle", 1.30, PACE, lead=0.0, tail=12.0, stops=[(0.12, 6.0)]),
        "play": anim.Session("play", 1.30, PACE, lead=0.0, tail=1.5, stops=[(0.12, 7.0, AT_SKY), (1.30, 10.5, AT_SURFACE)]),
        # The cave: the visitor pulls the lever twice; or nobody does, and the pumpkin shows how.
        # The dig is no stop, but a visitor who lingers finds things in the dirt.
        "dig": anim.Session("dig", 2.25, PACE, lead=0.0, tail=1.0, stops=[(2.25, 8.5, {"fossil_hover": 1.2, "chest_hover": 3.0, "sign_hover": 4.8, "sock_hover": 6.6})]),
        "cave": anim.Session("cave", 3.40, PACE, lead=0.0, tail=1.5, stops=[(3.40, 12.5, AT_CAVE)]),
        "cave_demo": anim.Session("cave_demo", 3.40, PACE, lead=0.0, tail=1.5, stops=[(3.40, 13.5, {"demo_off": 7.0, "demo_on": 10.8})]),
    }


def rad(*deg):
    return tuple(math.radians(d) for d in deg)


def V(*xyz):
    return Vector(xyz)


def hidden(obj):
    """Nothing to see until some later piece shows it."""
    anim.TL.add(obj, "scale", EVER, lambda s, dt: (0.0, 0.0, 0.0), "real")


def type_block(name, lines, at, palettes="gold", cell=0.1, depth=4, leading=3, yaw=0.0, tilt=0.0, material="type", scrim=0.0, feather=0.9, parent=None):
    """Lines of extruded type as one small tree: a root at the bottom centre of the front
    face, one object per line (so each line can have its own colours) and, if asked
    for, a soft dark card behind the letters. Returns the root."""
    b, mats = S["b"], S["mats"]
    lines = [lines] if isinstance(lines, str) else list(lines)
    palettes = [palettes] * len(lines) if isinstance(palettes, str) else list(palettes)
    root = core.empty(name, "Type", at)
    root.rotation_euler = rad(tilt, 0, yaw)
    pitch = (font.ROWS + leading) * cell
    width = max(font.line_mask(line).shape[1] for line in lines) * cell
    height = (len(lines) * font.ROWS + (len(lines) - 1) * leading) * cell
    for i, (line, palette) in enumerate(zip(lines, palettes)):
        obj = font.text_object(f"{name}_{i}", [line], b, mats[material], "Type", palette=palette, cell=cell, depth=depth)
        obj.parent = root
        obj.location = (0, 0, (len(lines) - 1 - i) * pitch)
    if scrim:
        card = scenery.scrim(f"{name}_Scrim", width, height, strength=scrim, feather=feather)
        card.parent = root
        card.location = (0, depth * cell - 0.02, height / 2)
    if parent is not None:
        root.parent = parent
    S.setdefault("type", {})[name] = (root, width, height)
    return root


# -- base --------------------------------------------------------------------
def ore_wall():
    """The dungeon's ore wall, by aspect: the room's height, the five ore tiles in
    reading order as (south edge y, feet above the floor), and the backups chest as
    (y, feet above the floor, standing on a ledge of the wall or not).

    Wide: five in a row, the chest on the floor under the middle one. Tall: two across
    and three up, the chest on a ledge as the sixth tile, in a room high enough for them."""
    d = wld.SPOTS["dungeon"]
    mid = (d["ores"][0] + d["ores"][-1]) / 2 + 1.0
    return P(
        {"height": d["height"], "tiles": [(y, 3) for y in d["ores"]], "chest": (mid, 0, False)},
        {"height": 12, "tiles": [(20, 7), (16, 7), (20, 4), (16, 4), (20, 1)], "chest": (17.0, 2, True)},
    )


# Facts embossed on the cut face beside the dig: (lines, palettes, cell, (centre x, bottom z))
def bands(tall=None):
    """Wide: two lines each, beside the dig. Tall: four short lines each, close beside
    the leg of the dig they belong to, so the type can be large in a narrow frame.
    Without an argument, the ones for the frame being built."""
    return (
        (
            (("COST: NOTHING.", "NO CARD. NO ADS."), ("gold", "cream"), 0.1, (6.6, -3.9)),
            (("2 GB RAM. 5 GB WORLD.", "ONE SERVER EACH."), ("gold", "cream"), 0.085, (2.0, -12.9)),
        ),
        (
            (("COST:", "NOTHING.", "NO CARD.", "NO ADS."), ("gold", "gold", "cream", "cream"), 0.09, (2.75, -5.4)),
            (("2 GB RAM.", "5 GB WORLD.", "ONE SERVER", "EACH."), ("gold", "gold", "cream", "cream"), 0.08, (-0.75, -14.5)),
        ),
    )[FRAME["tall"] if tall is None else tall]



def build_base():
    scene = core.reset_scene("KurbisFilm")
    anim.reset()
    S.clear()
    b = blk.build_registry()
    mascot.register(b)
    b.atlas.bake()
    mats = {
        "voxel": blk.voxel_material("Voxel", b.atlas, emit_strength=4.0),
        "nether": blk.voxel_material("VoxelNether", b.atlas, emit_strength=1.5),  # so the lava stays orange, not white
        "type": blk.voxel_material("Type", b.atlas, emit_strength=0.0, self_illum=0.55, roughness=0.7),
        "type_dim": blk.voxel_material("TypeDim", b.atlas, emit_strength=0.0, self_illum=0.2, roughness=0.7),
        "mini": blk.voxel_material("VoxelMini", b.atlas, emit_strength=2.4, self_illum=0.34),
        "glass": blk.flat_material("SnapshotGlass", (1.0, 0.86, 0.5), alpha=0.06, emit=0.25, roughness=0.05),
        "glow_edge": blk.flat_material("SnapshotEdge", (1.0, 0.78, 0.3), emit=5.0),
        "mascot": blk.voxel_material("Kurbis", b.atlas, emit_strength=3.2),
        "lamp": blk.voxel_material("VoxelLamp", b.atlas, emit_strength=1.1),  # the lever's lamp is close to the lens
        "wave": blk.flat_material("SnapshotWave", (1.0, 0.78, 0.3), alpha=0.99, emit=5.0),
        "water": blk.flat_material("Water", (0.03, 0.22, 0.80), alpha=0.74, roughness=0.3),
        "cloud": blk.flat_material("Cloud", (1.0, 0.80, 0.74), alpha=0.8, emit=0.16, emit_rgb=(1.0, 0.50, 0.46), roughness=1.0, dither=True),
        "cloud_near": blk.flat_material("CloudNear", (0.90, 0.80, 0.82), emit=0.05, emit_rgb=(1.0, 0.6, 0.6), roughness=1.0),
        "leaf": blk.flat_material("Leaf", (1.0, 0.42, 0.08), emit=0.5, roughness=0.8),
        "petal": blk.flat_material("Petal", (1.0, 0.62, 0.78), emit=0.45, roughness=0.8),
        "line": blk.flat_material("Line", (0.92, 0.92, 0.95), emit=0.3),
        "bobber": blk.flat_material("Bobber", (0.9, 0.12, 0.1), emit=0.25),
        "spark": blk.flat_material("PortalSpark", (0.72, 0.3, 1.0), emit=3.0),
        "ember": blk.flat_material("Ember", (1.0, 0.45, 0.1), emit=4.0),
        "black": blk.flat_material("Black", (0.0, 0.0, 0.0), roughness=1.0),
        "rim": blk.flat_material("Rim", (0.13, 0.04, 0.07), roughness=1.0),
    }
    for flat in ("black", "rim"):
        mats[flat].node_tree.nodes["Principled BSDF"].inputs["Specular IOR Level"].default_value = 0.0

    flat = []  # the cut face is kept flat behind the bands of both frames, so one world serves both
    for lines, _palettes, cell, (x, z) in (*bands(False), *bands(True)):
        cols = max(font.line_mask(line).shape[1] for line in lines)
        rows = len(lines) * font.ROWS + (len(lines) - 1) * 3
        flat.append((x - cols * cell / 2 - 0.8, x + cols * cell / 2 + 0.8, z - 0.8, z + rows * cell + 0.8))
    ow = ore_wall()
    cy, cup, shelf = ow["chest"]
    w = wld.World(b, flat=flat, dungeon={"height": ow["height"], "ores": ow["tiles"], "ledge": (int(cy) - 1, cup - 1) if shelf else None}).generate()
    closed = voxel.ALL_CLOSED_BUT_FRONT_AND_TOP
    voxel.grid_object("Terrain", w.grid, b, mats["voxel"], "World", origin=w.origin, closed=closed)
    # Everything that grows moves in the wind, and the water swells (see kurbis/life.py).
    life.wind(voxel.grid_object("Leaves", w.grid, b, mats["voxel"], "World", group="leaves", origin=w.origin, closed=closed), 0.075)
    water = voxel.grid_object("Water", w.grid, b, mats["water"], "World", group="water", origin=w.origin, closed=closed)
    life.shimmer(mats["water"])
    life.waves(water, POND_LEVEL, SWELL)
    life.waves(water, float(wld.SPOTS["cave"]["floor"]), 0.02)  # the pool in the cave
    life.wind(boxmodel.cross_sprites("GrassTufts", "block/short_grass.png", w.sprites["grass"], col="World", tint=(0.62, 0.86, 0.34)), 0.16, rooted=True)
    life.wind(boxmodel.cross_sprites("SugarCane", "block/sugar_cane.png", w.sprites["cane"], col="World"), 0.11, rooted=True)
    life.waves(boxmodel.flat_sprites("LilyPads", "block/lily_pad.png", w.sprites["lily"], col="World", tint=(0.22, 0.58, 0.16)), POND_LEVEL, SWELL)
    boxmodel.flat_sprites("Petals", "block/pink_petals.png", w.sprites["petals"], col="World")
    for kind, flower in enumerate(("poppy", "dandelion", "cornflower", "oxeye_daisy")):
        spots = [(x, y, z) for x, y, z, k in w.sprites["flowers"] if k == kind]
        if spots:
            life.wind(boxmodel.cross_sprites(f"Flowers_{flower}", f"block/{flower}.png", spots, col="World"), 0.14, rooted=True)

    look.sky(
        scene,
        SUN,
        stops=(  # linear values: these read much lighter on screen
            (-0.30, (0.28, 0.11, 0.08)),
            (-0.03, (0.80, 0.33, 0.13)),
            (0.00, (1.00, 0.58, 0.21)),
            (0.03, (1.00, 0.44, 0.16)),
            (0.09, (0.72, 0.24, 0.17)),
            (0.20, (0.30, 0.10, 0.20)),
            (0.38, (0.10, 0.05, 0.17)),
            (0.90, (0.02, 0.02, 0.09)),
        ),
        glow=(1.0, 0.50, 0.18),
        glow_power=42.0,
        glow_gain=0.85,
        disc=5.0,
        disc_power=2600.0,
        stars=1.2,
    )
    sun = look.sun("Sun", SUN_LIGHT, energy=7.0, color=(1.0, 0.62, 0.34), angle_deg=1.6)
    look.fog("Haze", (-300, -120, -4), (300, 420, 72), density=0.0014, fade=(0, 54), color=(1.0, 0.72, 0.5), anisotropy=0.74)
    comp = look.compositor(scene, bloom=0.38, threshold=1.1, size=0.55, vignette=0.32, saturation=1.05, wobble=True)

    cam = look.camera("Cam_Film", (0.0, 4.0, 31.0), (0.0, 60.0, 33.9), lens=28)
    scene.camera = cam
    S.update(
        scene=scene, b=b, mats=mats, w=w, cam=cam, sun=sun, comp=comp,
        white=look.overlay("Cover_Cloud", cam, rgb=(1.0, 0.84, 0.80), strength=1.3),
        swirl=look.overlay("Cover_Portal", cam, image_rel="block/nether_portal.png", strength=2.4),
    )


def ground_at(x, y):
    """Top of the valley terrain under a point."""
    return S["w"].top(int(math.floor(x)), int(math.floor(y))) + 1.0


def floor_z(x, y, start=-21):
    """Height of the floor under a point underground: first solid cell below `start`."""
    w, z = S["w"], start
    while z > w.z0 and w.get(int(math.floor(x)), int(math.floor(y)), z - 1) == 0:
        z -= 1
    return float(z)


def on_floor(x, y, start=-21):
    return (x, y, floor_z(x, y, start))


def new_actor(name):
    root, spin, bodies, stem = mascot.build_actor(name, S["b"], S["mats"]["mascot"])
    actor = anim.Actor(root, spin, bodies, extras=[stem])
    candle = look.point(f"{name}_Candle", (0, 0, 0), 0.0, (1.0, 0.6, 0.2), radius=0.3, shadow=False)
    candle.parent = root
    candle.location = (0.0, -1.1, 0.4)
    actor.candle = candle
    return actor


# -- sky and valley -----------------------------------------------------------
HERO_EYE = V(0.0, 4.0, 31.0)
HERO_PUMPKIN = V(0.425, 11.4, 30.5)
LANDING_SPOT = V(-0.9, 2.2, 1.0)
LANDING_EYE = V(-2.9, -4.2, 2.2)
SLAB_RATE = 6.0  # seconds of falling that one screen of scroll plays, once the ground breaks


def toward(obj, k=0.5, eye=HERO_EYE):
    """Pull an object toward the eye and shrink it by the same factor: the picture
    from the eye is unchanged, but the thing is nearer and smaller."""
    obj.location = Vector(obj.location) * k + eye * (1 - k)
    obj.scale = Vector(obj.scale) * k


def set_valley():
    """Hero and landing dressing, the pumpkin's cloud, and the slab of ground that
    hides the cut face until the slice."""
    b, mats, w = S["b"], S["mats"], S["w"]

    clouds = (
        ("near_left", (-33, 44, 45.0), (7, 3), (3.0, 3.0, 1.2), 6),
        ("near_right", (35, 52, 47.5), (8, 3), (3.0, 3.0, 1.2), 4),
        ("mid_right", (66, 118, 40.0), (7, 3), (4.0, 4.0, 1.6), 8),
        ("mid_left", (-70, 126, 42.0), (7, 3), (4.0, 4.0, 1.6), 5),
        ("high_a", (-16, 70, 52.0), (7, 3), (3.5, 3.5, 1.4), 6),
        ("high_b", (14, 96, 78.0), (8, 3), (4.0, 4.0, 1.6), 4),
        ("high_c", (-4, 130, 124.0), (9, 3), (5.0, 5.0, 2.0), 8),
        ("high_d", (46, 84, 46.0), (8, 3), (3.5, 3.5, 1.4), 2),
    ) + P((), (("top_left", (-14, 64, 68.0), (5, 3), (3.0, 3.0, 1.2), 6), ("top_right", (17, 104, 92.0), (6, 3), (3.5, 3.5, 1.4), 4)))  # the tall frame sees more sky
    for name, center, cells, cell, seed in clouds:
        far = scenery.cloud(f"Cloud_{name}", b, mats["cloud"], center, cells, cell, seed, "World", profile=(1, 0))
        live(far, "location", lambda t: (0.22 * t, 0.04 * t, 0.0))  # they drift with the wind
    # The layer the dive goes through: out of the hero frame, right under the camera.
    scenery.cloud("Cloud_dive", b, mats["cloud"], (-1, 3, 19.8), (7, 5), (3.0, 3.0, 1.4), 12, "World", profile=(1, 0))
    # Leaves on the wind, each travelling for ever inside its box. The boxes keep clear of
    # the lens and of the air in front of the address.
    for name, material, lo, hi, count, size, seed, velocity in (
        ("Leaves_high", "leaf", (-16, 13, 18), (16, 46, 50), 110, (0.05, 0.12), 5, (1.1, 0.35, -0.3)),
        ("Petals_air", "petal", (1, 0.5, 1.2), (13, 11, 12), 46, (0.03, 0.07), 3, (0.35, 0.1, -0.55)),
        ("Leaves_low", "leaf", (-20, 6.6, 1.4), (12, 30, 18), 90, (0.04, 0.09), 4, (0.9, 0.3, -0.25)),
    ):
        life.drift(scenery.leaves(name, mats[material], "FX", lo=lo, hi=hi, count=count, size=size, seed=seed), velocity, lo, hi, sway=0.22)

    # Hero: the title over the valley, and the pumpkin's cloud with the caption on it.
    title_z = P(40.0, 42.9)
    title = font.text_object("Hero_Title", P(["YOUR WORLD", "IS WAITING BELOW"], ["YOUR WORLD", "IS WAITING", "BELOW"]), b, mats["type"], "Type", palette="gold", cell=0.5, depth=6, leading=1)
    title.location = (0, 59.5, title_z)
    title_lines = P(["YOUR WORLD", "IS WAITING BELOW"], ["YOUR WORLD", "IS WAITING", "BELOW"])
    if HELP["rim"]:
        font.keyline("Hero_Title_Rim", title_lines, mats["rim"], cell=0.5, width=HELP["rim"], leading=1).parent = title
    if HELP["scrim"]:
        rows, cols = font.text_mask(title_lines, leading=1).shape
        card = scenery.scrim("Hero_Title_Scrim", cols * 0.5, rows * 0.5, strength=HELP["scrim"], feather=5.0)
        card.parent = title
        card.location = (0, 6 * 0.5 + 0.5, rows * 0.5 / 2)

    # The perch: a slab with a riser on it, the caption on the riser's face. Narrower and
    # with a taller riser in the tall frame, where the caption takes three lines.
    footprint, risers = P((12, 8), (9, 8)), P(3, 4)
    step, perch = (0.7, 0.7, 0.35), V(0.5, 18.6, 30.0 - 0.35 * (2 + risers))
    mask = np.ones(footprint, bool)
    mask[:1, 1:] = mask[-1:, 2:] = False
    mask[:3, -2:] = mask[-3:, -1:] = False
    near = [scenery.cloud("Hero_Perch", b, mats["cloud_near"], perch, footprint, step, col="World", profile=(1, 0) + (1,) * risers, mask=mask)]
    ledge = perch.z + step[2] * 2
    cap_cell, depth = 0.042, 3
    cap_lines = P(["FREE MINECRAFT SERVERS", "FOR YOU AND YOUR FRIENDS"], ["FREE MINECRAFT", "SERVERS FOR YOU", "AND YOUR FRIENDS"])
    cap_rows = len(cap_lines) * font.ROWS + (len(cap_lines) - 1) * 3
    caption = font.text_object("Hero_Caption", cap_lines, b, mats["type"], "Type", palette="plum", cell=cap_cell, depth=depth, leading=3)
    caption.location = (perch.x, perch.y - step[1] * 3 - depth * cap_cell, ledge + (step[2] * risers - cap_rows * cap_cell) / 2)
    cue = font.text_object("Hero_Cue", ["DIG DOWN ↓"], b, mats["type"], "Type", palette="cream", cell=P(0.022, 0.03), depth=3)
    cue.location = (0.5, 15.0, P(28.27, 26.0))  # (its nudge stays inside the safe area)
    near += [caption, cue]
    zs = []
    for i, ((dx, dz), cell) in enumerate(zip(((1.75, 1.3), (2.5, 1.6), (3.35, 1.95)), (0.034, 0.046, 0.062))):
        z = font.text_object(f"Hero_Zzz_{i}", ["Z"], b, mats["type"], "Type", palette="plum", cell=cell, depth=2)
        z.location = (0.85 + dx, 18.8 - 0.4, 30.0 + dz)
        z.rotation_euler = rad(0, 8 + i * 9, -6)
        zs.append(z)
    for obj in near + zs:
        toward(obj)  # the pumpkin is one block everywhere, so its cloud sits closer than in the still
    for obj in [title] + near:  # gone under the white-out, so nothing of the hero hangs over the landing
        switch(obj, "scale", [(0.0, tuple(obj.scale)), (WHITE[1] + 0.01, (0.0, 0.0, 0.0))])

    # Life in the sky, in real time. The Zs rise from the sleeper one after another
    # (until it wakes), the title floats, and the cue nudges downward.
    def z_at(u):
        point = P(V(2.2, 18.4, 31.05), V(1.9, 18.4, 31.4)).lerp(P(V(4.6, 18.4, 32.2), V(3.3, 18.4, 33.3)), u) + V(0, 0, 0.1 * math.sin(u * 9.0))
        return tuple(point * 0.5 + HERO_EYE * 0.5)  # as `toward` places it

    for i, (z, cell) in enumerate(zip(zs, (0.034, 0.046, 0.062))):

        def risen(t, i=i):
            return (t / 2.7 + i / 3.0) % 1.0

        def z_size(t, cell=cell, risen=risen):
            u = risen(t)
            return (0.5 * lerp(0.03, 0.066, u) / cell * smooth(u / 0.12) * smooth((1.0 - u) / 0.18),) * 3

        live(z, "location", lambda t, risen=risen: z_at(risen(t)))
        live(z, "scale", z_size)
        anim.TL.add(z, "scale", WAKE_UP - 0.01, lambda s, dt, z_size=z_size: tuple(c * max(1.0 - dt / 0.15, 0.0) for c in z_size(anim.now())))
    live(title, "location", lambda t: (0.0, 59.5, title_z + 0.28 * math.sin(t * 2 * math.pi / 6.5)))
    live(cue, "location", lambda t, home=Vector(cue.location): tuple(home + V(0, 0, 0.012 * math.sin(t * 2 * math.pi / 1.1))))

    hero_key = look.sun("Hero_Key", (-0.5, -0.62, 0.6), energy=2.1, color=(1.0, 0.60, 0.40), angle_deg=8.0, volume=0.0)
    light_ramp(hero_key, [(0.0, 2.1), (0.78, 2.1), (0.95, 0.0)])
    fill = look.area("Hero_Fill", HERO_EYE + V(-1.75, 0, 1.0), HERO_PUMPKIN + V(0, 0, 0.5), energy=125, color=(1.0, 0.76, 0.68), size=3)
    light_ramp(fill, [(0.0, 125.0), (0.42, 125.0), (0.52, 0.0)])

    # The slab in front of the cut face, in columns that fall away in a wave from the stomp.
    # The fall is scrubbed, so the camera, which drops through where the slab was, can
    # never catch up with it however fast the page is scrolled.
    origin, grid = w.front
    nx, ny, nz = grid.shape
    rng = np.random.default_rng(3)
    for i0 in range(0, nx, 2):
        for row, (j0, j1) in enumerate(((0, ny // 2), (ny // 2, ny))):
            sub = grid[i0 : i0 + 2, j0:j1]
            if not sub.any():
                continue
            lo = Vector((origin[0] + i0, origin[1] + j0, origin[2]))
            centre = lo + Vector((1.0, (j1 - j0) / 2, nz / 2))
            chunk = voxel.grid_object(f"Slab_{i0:02d}_{row}", sub, b, mats["voxel"], "World", origin=tuple(lo - centre))
            chunk.location = centre
            S.setdefault("slab", []).append({"name": chunk.name, "at": (i0, j0, 0), "shape": sub.shape, "centre": tuple(centre)})  # the page builds each from the slab's cells
            delay = 0.04 + 0.03 * abs(centre.x + 1.0) + (0.0 if row == 1 else 0.16)
            tip_x, twist, drift = rng.uniform(0.7, 1.5), rng.uniform(-0.25, 0.25), rng.uniform(0.6, 1.6)

            def fallen(s, delay=delay):
                return max((s - STOMP) * SLAB_RATE - delay, 0.0)

            scrub(chunk, "location", STOMP, lambda s, c=centre, d=drift, f=fallen: tuple(c + Vector((0.0, -d * f(s), -13.0 * f(s) ** 2))))
            scrub(chunk, "rotation_euler", STOMP, lambda s, a=tip_x, t=twist, f=fallen: (-a * f(s), 0.0, t * f(s)))
            scrub(chunk, "scale", STOMP, lambda s, f=fallen: (0.0, 0.0, 0.0) if f(s) > 1.55 else (1.0, 1.0, 1.0))


# -- the surface -----------------------------------------------------------------
NAMES = ("STEVE", "LENA", "BIGFARM")  # the address retypes itself through these


def set_surface():
    """The meadow where the pumpkin lands: the address, whose name retypes itself, and
    two friends on different editions fishing the same pond."""
    b, mats, session = S["b"], S["mats"], S["session"]
    shown = 1.05

    # The address. Its line re-centres as the name grows and shrinks. In the tall frame
    # the name stands over the host, each centred.
    stacked = FRAME["tall"]
    cell, at = P(0.115, 0.11), P(V(-3.05, 4.6, 4.8), V(-3.9, 6.2, 5.3))
    over = (font.ROWS + 3) * cell if stacked else 0.0  # how far the name stands above the host
    # The tall frame looks across the meadow on a diagonal, so there the type turns to face it.
    block = core.empty("Landing_Type", "Type", at)
    block.rotation_euler = rad(0, 0, P(0.0, 29.0))
    leaving = lambda s: (1.0 - smooth((s - STOMP) / 0.07),) * 3  # the type belongs to this stop: it goes as the ground breaks
    scrub(block, "scale", STOMP, leaving)
    root = core.empty("Landing_Address", "Type", (0, 0, 0), parent=block)
    pop(root, shown)
    host = font.text_object("Landing_Host", [".KURBIS.HOST"], b, mats["type"], "Type", palette="gold", cell=cell, depth=4)
    host.parent = root
    host_w = font.line_mask(".KURBIS.HOST").shape[1]
    if HELP["rim"]:
        font.keyline("Landing_Host_Rim", [".KURBIS.HOST"], mats["rim"], cell=cell, width=HELP["rim"]).parent = host
    if HELP["scrim"]:
        card = scenery.scrim("Landing_Address_Scrim", P(host_w * 1.75, host_w) * cell, (font.ROWS + P(8, 22)) * cell, strength=HELP["scrim"], feather=1.4)
        card.parent = root
        card.location = (0, 4 * cell + 0.1, (font.ROWS + P(8, 22)) * cell / 2 - 2 * cell)
    rows = []
    for name in NAMES:
        row, x = [], 0
        for i, ch in enumerate(name):
            width = font.GLYPHS[ch].shape[1]
            letter = font.text_object(f"Landing_{name}_{i}", [ch], b, mats["type"], "Type", palette="cream", cell=cell, depth=4)
            letter.parent = root
            if HELP["rim"]:
                font.keyline(f"Landing_{name}_{i}_Rim", [ch], mats["rim"], cell=cell, width=HELP["rim"]).parent = letter
            row.append((letter, x, width))
            x += width + 1
        rows.append(row)
    # What stands there at each moment of the loop, (seconds, which name, how many letters),
    # and when each letter is typed.
    held, typing, wiping, pause = 2.4, 0.11, 0.05, 0.25
    steps, typed, t, k = [(0.0, 0, len(NAMES[0]))], {}, 0.0, 0
    for following in (1, 2, 0):
        t += held
        for n in range(len(NAMES[k]) - 1, -1, -1):
            steps.append((t, k, n))
            t += wiping
        t += pause
        for n in range(1, len(NAMES[following]) + 1):
            steps.append((t, following, n))
            typed[(following, n - 1)] = t
            t += typing
        k = following
    period, born = t, session.when(shown)
    where = {letter: ([], []) for row in rows for letter, _, _ in row}
    host_at, width = [], None
    for f in range(session.frames):
        tau = f / FPS
        u = (tau - born) % period if tau > born else 0.0
        which, n = [(w, count) for moment, w, count in steps if moment <= u + 1e-9][-1]
        want = float(rows[which][n - 1][1] + rows[which][n - 1][2]) if n else 0.0
        width = want if width is None else width + (want - width) * (1.0 - math.exp(-22.0 / FPS))
        space = min(width / 3.0, 1.0)
        left = -width / 2.0 if stacked else -(width + space + host_w) / 2.0
        for j, row in enumerate(rows):
            for i, (letter, offset, w) in enumerate(row):
                age = (u - typed[(j, i)]) % period
                size = 1.0 + 0.18 * math.sin(math.pi * min(age / 0.12, 1.0)) if (j == which and i < n) else 0.0
                where[letter][0].append(((left + offset + w / 2.0) * cell, 0.0, over))
                where[letter][1].append((size, size, size))
        host_at.append((0.0 if stacked else (left + width + space + host_w / 2.0) * cell, 0.0, 0.0))
    for letter, (places, sizes) in where.items():
        anim.write(letter, "location", places)
        anim.write(letter, "scale", sizes)
    anim.write(host, "location", host_at)
    caption = font.text_object("Landing_Caption", ["YOU CHOOSE THE NAME."], b, mats["type"], "Type", palette="cream", cell=0.05, depth=3)
    caption.parent = block
    caption.location = V(0, 0, over + 8 * cell + 5 * 0.05)
    if HELP["rim"]:
        font.keyline("Landing_Caption_Rim", ["YOU CHOOSE THE NAME."], mats["rim"], cell=0.05, width=HELP["rim"] * 1.4).parent = caption
    pop(caption, shown + 0.07)
    # Under the address: to the right in the wide frame, where Steve's rod owns the air
    # to the left; centred and on two lines in the tall one.
    pop(type_block("Landing_Editions", P(["DIFFERENT EDITIONS. SAME POND."], ["DIFFERENT EDITIONS.", "SAME POND."]), P(V(3.2, 0, -0.44), V(0, 0, -0.92)), "cream", cell=P(0.03, 0.036), depth=2, scrim=0.62, feather=0.5, parent=block), shown + 0.1)

    # Two friends fish the same pond: Steve on Java from the near bank, with a barrel for
    # his catch, and Alex on Bedrock from the far bank, who lets hers go.
    ripple = scenery.fading_material("Ripple", (0.85, 0.94, 1.0), strength=1.2)
    S["fishers"], S["friends"] = [], {}
    for tag, skin, slim, (x, y), yaw, head, arm, label, float_at, away, keeps, phase in (
        ("Steve", "steve", False, (-4.3, 3.2), -58, (4, 0, 38), -72, "JAVA", (-6.6, 2.4), (-2.5, 3.5, ground_at(-3, 3) + 1.0), True, 0.0),
        ("Alex", "alex", True, (-8.2, 5.8), -3, (2, 0, 8), -46, "BEDROCK", (-8.6, 3.6), (-9.0, 2.7, POND_LEVEL), False, 4.8),  # she holds her rod low, clear of her name
    ):
        rig = boxmodel.player(tag, skin, slim=slim)
        S["friends"][tag] = rig
        rig.place((x, y, ground_at(x, y)), yaw=yaw)
        rig.pose(body=(-4, 0, 0), head=head, arm_r=(arm, 0, 6), hand_r=(-24, 0, 0), arm_l=(-22, 0, 0), hand_l=(-26, 0, 0), leg_r=(-8, 0, 0), foot_r=(4, 0, 0), leg_l=(7, 0, 0), foot_l=(2, 0, 0))
        rod = boxmodel.item(f"{tag}_Rod", "item/fishing_rod_cast.png")
        tip = boxmodel.hold(rig, rod, scale=1.3)
        bobber = scenery.boxes_object(f"{tag}_Bobber", [((-0.07, -0.07, -0.02), (0.14, 0.14, 0.16))], mats["bobber"], "Props")
        line = scenery.boxes_object(f"{tag}_Line", [((-0.0125, 0.0, -0.0125), (0.025, 1.0, 0.025))], mats["line"], "Props")
        rings = [scenery.ring(f"{tag}_Ring_{i}", ripple) for i in range(3)]
        S["fishers"].append(life.Fisher(rig, rod, tip, bobber, line, boxmodel.fish(f"{tag}_Fish"), rings, (*float_at, 0.0), away, POND_LEVEL, phase, SWELL, keeps, size=0.45))
        S.setdefault("fishing", []).append({"tag": tag, "tip": tip, "float": (*float_at, 0.0), "away": away, "keeps": keeps, "phase": phase, "size": 0.45})  # for the page, which plays the same loop
        name_tag = font.text_object(f"Tag_{tag}", [label], b, mats["type"], "Type", palette="cream", cell=0.042, depth=2)
        name_tag.location = (x + (0.0 if keeps else P(0.3, -0.5)), y, rig.root.location.z + (2.2 if keeps else P(3.5, 3.3)))  # hers stands clear above both rods, and the cane
        name_tag.rotation_euler = rad(0, 0, P(0.0, 29.0))
        pop(name_tag, shown + 0.05)
        scrub(name_tag, "scale", STOMP, leaving)
    # Butterflies over the meadow, each in its own livery.
    rows = ("kkkk..", "kaaakk", "kabaak", "kaawak", "kaaaak", ".kkaak", ".kaakk", ".kkkk.")
    liveries = (((1.0, 0.55, 0.10), (1.0, 0.80, 0.25)), ((1.0, 0.93, 0.45), (1.0, 1.0, 0.85)), ((0.45, 0.72, 1.0), (0.80, 0.92, 1.0)), ((1.0, 0.62, 0.78), (1.0, 0.86, 0.92)))
    for i, ((x, y, lift), (a, c)) in enumerate(zip(((1.9, 4.6, 1.3), (-1.4, 7.6, 1.7), (4.3, 6.4, 2.2), (-5.6, 8.2, 1.6)), liveries)):
        wing = core.image_from_pixels(f"butterfly_{i}", life.picture(rows, {"k": (0.10, 0.06, 0.05), "a": a, "b": c, "w": (1.0, 1.0, 1.0)}), core.EXPORT / f"butterfly_{i}.png")
        life.flier(f"Butterfly_{i}", wing, (0.27, 0.36), (x, y, ground_at(x, y) + lift), (1.3, 1.1, 0.45), speed=0.9 + 0.17 * i, beat=4.6 + 0.7 * i, seed=2.3 * i + 0.4, emit=0.5)
    rings = [scenery.ring(f"Leaper_Ring_{i}", ripple) for i in range(2)]
    S["fishers"].append(life.Jumper(boxmodel.fish("Leaper"), rings, (((-7.9, 2.0), (-7.3, 2.8)), ((-6.0, 1.3), (-6.8, 1.6))), POND_LEVEL, period=5.2, phase=2.0, height=SWELL))

    landing_key = look.sun("Landing_Key", (-0.38, -0.5, 0.78), energy=2.3, color=(1.0, 0.62, 0.42), angle_deg=6.0, volume=0.0)
    light_ramp(landing_key, [(0.66, 0.0), (0.86, 2.3), (1.56, 2.3), (1.74, 0.0)])
    mist = look.fog("Landing_Mist", (-80, -14, 0.6), (80, 140, 30), density=0.005, color=(1.0, 0.74, 0.5), anisotropy=0.76)
    ramp(mist.data.materials[0].node_tree.nodes["Volume Scatter"].inputs["Density"], "default_value", [(0.62, 0.0), (0.9, 0.005), (1.50, 0.005), (1.74, 0.0)])


# -- the dig ---------------------------------------------------------------------
def set_dig():
    """The cut face: two bands of facts, the blocks the pumpkin has yet to dig, the
    crumbs it throws, and the daylight that falls on the face."""
    b, mats, w = S["b"], S["mats"], S["w"]
    steps = wld.tunnel_steps()
    # It comes through the roof at the first step whose whole height is inside the cave.
    through = next(i for i in range(8, len(steps)) if all(w.get(steps[i][0] - dx, 2, steps[i][1] + 1) == 0 for dx in (0, 1)))
    assert through >= 20, through
    S["steps"], S["through"] = steps, through
    # How far along the dig (in blocks) it is at each scroll position: slower past the bands.
    S["dig"] = anim.Track([(DIG0, 0.0), (1.95, 6.0), (2.12, 10.0), (2.33, 15.8), (2.47, 18.3), (BREAK, float(through))])

    for i, (lines, palettes, cell, (x, z)) in enumerate(bands()):
        band = type_block(f"Band_{i}", lines, (x, -4 * cell, z), palettes, cell=cell, depth=4, scrim=0.42, feather=0.8)
        if FRAME["tall"]:  # the tall frame turns away from each band before it is out of sight: it steps aside before the frame's edge would cut it
            gone = (2.04, 2.52)[i]
            scrub(band, "scale", gone, lambda s, gone=gone: (1.0 - smooth((s - gone) / 0.05),) * 3)
    cx, cz = wld.SPOTS["chest"]
    chest, _ = boxmodel.chest("Dig_Chest", open_deg=0)
    chest.location = (cx + 1.0, 0.5, cz)
    chest.rotation_euler = rad(0, 0, 6)
    # Things buried in the cut face, to find on the way down: a fossil (in the world's
    # blocks), a chest, an old sign, a lost sock.
    board = boxmodel.sign("Dig_Sign", post=False)
    board.location, board.rotation_euler = P((7.0, -0.07, -10.9), (0.5, -0.07, -9.1)), rad(0, 6, 0)
    note = type_block("Dig_Sign_Text", ["BACK IN", "5 MINUTES"], (0, 0, 0), "ink", cell=0.022, depth=1, leading=3, parent=board)
    note.location = (0, -1 / 16 - 0.022, (0.75 - 19 * 0.022) / 2)
    sock = ("................", "....wwwww.......", "....rrrrr.......", "....wwwww.......", "....rrrrr.......", "....wwwww.......", "....wwwww.......", "....wwwww.......",
            "....wwwwww......", "....wwwwwwww....", "....wwwwwwwwww..", "....gwwwwwwwwww.", ".....gwwwwwwwww.", "......ggggggggg.", "................", "................")
    ink = {".": (0, 0, 0, 0), "w": (0.93, 0.92, 0.88, 1), "r": (0.82, 0.16, 0.14, 1), "g": (0.62, 0.61, 0.6, 1)}
    px = np.array([[ink[c] for c in row] for row in sock[::-1]], np.float32)
    boxmodel.image_tiles("Dig_Sock", core.image_from_pixels("sock", px, core.EXPORT / "sock.png"), [[(-3.3, -0.02, -9.3), (-2.4, -0.02, -9.3), (-2.4, -0.02, -8.4), (-3.3, -0.02, -8.4)]])
    S["toys"] = {"fossil": V(8.4, 0.0, -6.6), "chest": V(cx + 1.0, 0.0, cz + 0.5), "sign": Vector(board.location) + V(0, 0.07, 0.4), "sock": V(-2.85, 0.0, -8.85)}

    # What the pumpkin has not dug yet stays in place until it gets there.
    rng = np.random.default_rng(11)
    crumbs = [voxel.grid_object(f"Crumb_{k}", np.full((1, 1, 1), b["dirt" if k % 2 else "stone"], np.uint8), b, mats["voxel"], "FX", scale=0.3, origin=(-0.15, -0.15, -0.15)) for k in range(12)]
    for crumb in crumbs:
        hidden(crumb)
    for i, step in enumerate(w.tunnel[: through + 1]):
        at = Vector(step["at"])
        when = S["dig"].when(float(i)) if i else DIG0
        if step["cells"]:
            cells = np.array([c[:3] for c in step["cells"]])
            lo = cells.min(axis=0)
            sub = np.zeros(tuple(cells.max(axis=0) - lo + 1), np.uint8)
            for x, y, z, block in step["cells"]:
                sub[x - lo[0], y - lo[1], z - lo[2]] = block
            centre = Vector(lo) + Vector(sub.shape) / 2
            plug = voxel.grid_object(f"Plug_{i:02d}", sub, b, mats["voxel"], "World", origin=tuple(Vector(lo) - centre))
            plug.location = centre
            switch(plug, "scale", [(0.0, (1.0, 1.0, 1.0)), (when - 0.004, (0.0, 0.0, 0.0))])
            # Its face cracks as the pumpkin comes: ten stages by the scroll, then it is gone.
            solid = {c[:3] for c in step["cells"]}
            fronts = [(x, y, z) for x, y, z in solid if y == 0 or (x, 0, z) not in solid]
            quads = [[(x, y - 0.004, z), (x + 1, y - 0.004, z), (x + 1, y - 0.004, z + 1), (x, y - 0.004, z + 1)] for x, y, z in fronts]
            cracks = bpy.data.objects.new(f"Crack_{i:02d}", core.mesh_from_quads(f"Crack_{i:02d}", np.asarray(quads, np.float32), np.asarray([[(0, 0), (1, 0), (1, 1), (0, 1)]] * len(quads), np.float32)))
            cracks.data.materials.append(scenery.crack_material())
            cracks.visible_shadow = False
            core.link(cracks, "FX")
            begun = S["dig"].when(float(i - 1)) if i > 1 else DIG0
            scrub(cracks, "color", begun, lambda s, i=i: (min(max(S["dig"](s) - (i - 1), 0.0), 0.999), 0.0, 0.0, 1.0))
            switch(cracks, "scale", [(0.0, (0.0, 0.0, 0.0)), (begun, (1.0, 1.0, 1.0)), (when - 0.004, (0.0, 0.0, 0.0))])
        # A crumb thrown toward the camera at every step, so the digging has some bite.
        crumb = crumbs[i % len(crumbs)]
        fling = Vector((rng.uniform(-1.6, 1.6), rng.uniform(-3.4, -1.4), rng.uniform(1.0, 3.2)))
        spin_rate = Vector(rng.uniform(-9, 9, 3))
        start = at + Vector((0, -0.2, 0.5))
        after(crumb, "location", when, lambda dt, p=start, v=fling: tuple(p + v * dt + Vector((0, 0, -9.0 * dt * dt))))
        after(crumb, "rotation_euler", when, lambda dt, r=spin_rate: tuple(r * dt))
        after(crumb, "scale", when, lambda dt: (max(1.0 - dt / 0.42, 0.0),) * 3)

    dig_key = look.sun("Dig_Key", (-0.52, -0.56, 0.64), energy=0.9, color=(1.0, 0.86, 0.72), angle_deg=5.0, volume=0.0)
    light_ramp(dig_key, [(1.54, 0.0), (1.72, 0.9), (2.42, 0.9), (2.62, 0.0)])
    daylight = look.area("Dig_Daylight", (-3.0, -7.0, 7.0), (-3.0, 0.0, -9.0), energy=2600, color=(1.0, 0.80, 0.62), size=22, shadow=True)
    light_ramp(daylight, [(1.54, 0.0), (1.72, 2600.0), (2.5, 2600.0), (2.8, 0.0)])
    haze = look.fog("Dig_Haze", (-34, -5, -32), (34, 7, 2), density=0.011, color=(1.0, 0.9, 0.8), anisotropy=0.4)
    ramp(haze.data.materials[0].node_tree.nodes["Volume Scatter"].inputs["Density"], "default_value", [(1.5, 0.0), (1.75, 0.011)])


def dig_position(s):
    steps = S["steps"]
    i = S["dig"](s)
    k = min(int(math.floor(i)), len(steps) - 2)
    (x0, z0), (x1, z1) = steps[k], steps[k + 1]
    return Vector((lerp(x0, x1, i - k), 1.0, lerp(z0, z1, i - k)))


# -- the cave ----------------------------------------------------------------------
CAVE_EYE = V(0.2, -2.2, -23.6)
CAVE_AIM = V(0.8, 6.0, -22.55)
CAVE_PUMPKIN = (-0.4, 3.6)
TORCHES = ((-8.6, 1.6), (7.6, 7.6), (-1.8, 9.2), (12.6, 1.4), (2.8, 9.2), (-4.5, 5.5))
LANTERNS = ((4.4, 1.6), (-2.4, 1.2))
SNAPSHOT = (1.2, 3.0)
SNAP_TILT = (10, 0, 24)
LEVER = (3.0, 2.9)


def flicker(tau, phase):
    """A flame is never quite steady: a factor near 1 that wanders."""
    return 1.0 + 0.09 * math.sin(tau * 13.1 + phase) + 0.06 * math.sin(tau * 29.3 + 2.3 * phase) + 0.04 * math.sin(tau * 5.7 + 0.7 * phase)


def set_cave():
    """The cave the cut face opens. It has two states, asleep and awake, and one sequence
    from each to the other. Three things play those sequences: the pumpkin arriving
    (scrubbed by the scroll), the visitor's hand on the lever, and the pumpkin showing
    how when nobody touches it.

    Asleep: dark and blue, torches out, nobody there, the world kept as one small
    glowing block. Awake: torches lit, friends in, the block unfolded into the world."""
    b, mats, w, session = S["b"], S["mats"], S["w"], S["session"]
    one, zero = (1.0, 1.0, 1.0), (0.0, 0.0, 0.0)
    near = [(0.0, True), (2.3, False), (4.2, True)]  # the cave's lights exist only while it can be seen
    wakes, sleeps = [], []  # what each sequence does, as fn(start, clock)
    rewakes = []  # and what waking does only after a sleep: putting away what sleep brought

    by_length = {}
    for x, y, z, length in w.sprites["vines"]:
        by_length.setdefault(length, []).append((x, y, z))
    for length, spots in by_length.items():
        boxmodel.cross_sprites(f"Cave_Vines_{length}", "block/cave_vines_lit.png", spots, col="Props", emit=1.4, emit_rel="block/cave_vines_lit_e.png", rows=length)
    look.fog("Cave_Haze", (-16, -9, -29), (22, 15, -13), density=0.007, color=(1.0, 0.9, 0.8), anisotropy=0.4)
    dust_lo, dust_hi = (-7.0, 0.5, -24.8), (8.0, 9.0, -19.5)
    dust = scenery.leaves("Cave_Dust", scenery.fading_material("Dust", (1.0, 0.8, 0.5), strength=0.9), "FX", lo=dust_lo, hi=dust_hi, count=55, size=(0.008, 0.018), seed=13)
    dust.color = (1.0, 1.0, 1.0, 0.45)  # a few motes in the torchlight, no more
    life.drift(dust, (0.07, 0.03, 0.05), dust_lo, dust_hi, sway=0.06)
    chest, _ = boxmodel.chest("Cave_Chest", open_deg=-38)
    chest.location = on_floor(-3.3, 6.6)
    chest.rotation_euler = rad(0, 0, 20)

    def switched(obj, on_at=None, off_at=None):
        """It is there while the cave is awake (`on_at` and `off_at` are how long into each
        sequence it changes), or, with the two swapped by the caller, while it sleeps."""
        wakes.append(lambda start, clock: after(obj, "scale", start, lambda dt: one if dt >= on_at else zero, clock))
        sleeps.append(lambda start, clock: after(obj, "scale", start, lambda dt: zero if dt >= off_at else one, clock))

    def lit(lamp, energy, on_at, off_at, rise=0.1, fall=0.15, flame=None):
        """A light that burns while the cave is awake."""
        waver = (lambda: flicker(anim.now(), flame)) if flame is not None else (lambda: 1.0)
        switch(lamp, "hide_render", near)
        wakes.append(lambda start, clock: anim.TL.add(lamp.data, "energy", start, lambda s, dt: energy * anim.clamp01((dt - on_at) / rise) * waver(), clock))
        sleeps.append(lambda start, clock: anim.TL.add(lamp.data, "energy", start, lambda s, dt: energy * (1.0 - anim.clamp01((dt - off_at) / fall)) * waver(), clock))

    # Torches and lanterns: they fire outward from the block, and die from the far end back.
    warm = (1.0, 0.54, 0.18)
    heart = Vector(SNAPSHOT)
    flames = [("Torch", boxmodel.torch, at, 110, 0.85, 0.15) for at in TORCHES] + [("Lantern", boxmodel.lantern, at, 70, 0.45, 0.12) for at in LANTERNS]
    order = sorted(range(len(flames)), key=lambda i: (Vector(flames[i][2]) - heart).length)
    for i, (kind, make, (x, y), energy, lift, radius) in enumerate(flames):
        rank = order.index(i)
        on_at, off_at = 0.1 + 0.07 * rank, 0.45 + 0.09 * (len(flames) - 1 - rank)
        burning, dead = make(f"{kind}_{i}_lit"), make(f"{kind}_{i}_out", lit=False)
        burning.location = dead.location = on_floor(x, y)
        switched(burning, on_at, off_at)
        wakes.append(lambda start, clock, dead=dead, on_at=on_at: after(dead, "scale", start, lambda dt: zero if dt >= on_at else one, clock))
        sleeps.append(lambda start, clock, dead=dead, off_at=off_at: after(dead, "scale", start, lambda dt: one if dt >= off_at else zero, clock))
        lit(look.point(f"{kind}Light_{i}", (x, y, burning.location.z + lift), energy, warm, radius=radius), energy, on_at, off_at, flame=1.7 * i)
    glows = (
        ("Furnace_a", (-3.5, 7.4, floor_z(-3.5, 7.0) + 0.5), 90, (1.0, 0.45, 0.12), 0.3, True),
        ("Furnace_b", (7.5, 4.3, floor_z(7.5, 4.0) + 0.5), 70, (1.0, 0.45, 0.12), 0.3, True),
        ("Berries_a", (10.5, 5.5, -19.8), 45, (1.0, 0.70, 0.28), 0.5, False),
        ("Berries_b", (13.0, 2.5, -20.6), 45, (1.0, 0.70, 0.28), 0.5, False),
        ("Berries_c", (9.0, 8.0, -19.2), 45, (1.0, 0.70, 0.28), 0.5, False),
        ("Diamonds", (-7.0, 3.0, -23.6), 26, (0.35, 0.95, 1.0), 0.4, False),
    )
    for name, at, energy, color, radius, shadow in glows:
        lit(look.point(f"Cave_{name}", at, energy, color, radius=radius, shadow=shadow), energy, 0.3, 0.3, rise=0.5, fall=0.8)
    kx, ky = CAVE_PUMPKIN
    lit(look.point("Cave_CastKey", (kx + 0.4, ky - 2.2, floor_z(kx, ky) + 1.9), 150, (1.0, 0.66, 0.34), radius=0.9), 150, 0.3, 0.2, rise=0.6, fall=0.8)
    lit(look.area("Cave_Front", (3, -12, -17), (3, 4, -23), energy=260, color=(0.42, 0.52, 1.0), size=18), 260, 0.1, 0.3, rise=0.6, fall=1.0)
    # Moonlight through the open front while it sleeps, so the sleeper and the block can be seen.
    moon = look.area("Cave_Moon", (3, -12, -17), (2, 4, -24), energy=0.0, color=(0.30, 0.42, 1.0), size=18)
    switch(moon, "hide_render", near)
    arriving = anim.linear([(2.45, 0.0), (2.7, 150.0)])
    anim.TL.add(moon.data, "energy", EVER, lambda s, dt: arriving(s), "real")
    wakes.append(lambda start, clock: after(moon.data, "energy", start, lambda dt: 150.0 * (1.0 - anim.clamp01(dt / 0.5)), clock))
    sleeps.append(lambda start, clock: after(moon.data, "energy", start, lambda dt: 150.0 * anim.clamp01((dt - 0.6) / 1.0), clock))

    # The two friends: they join once the lights are on, and leave before they go out.
    steve = boxmodel.player("Cave_Steve", "steve")
    steve.place(on_floor(*P((-2.5, 4.2), (-1.7, 5.4))), yaw=-66)  # (the tall frame is narrower: he works nearer the middle)
    steve.pose(body=(12, 0, 0), head=(2, 0, 50), arm_r=(-150, 0, 0), hand_r=(-28, 0, 0), arm_l=(34, 0, 0), hand_l=(-18, 0, 0), leg_r=(-30, 0, 0), foot_r=(26, 0, 0), leg_l=(22, 0, 0), foot_l=(8, 0, 0))
    boxmodel.hold(steve, boxmodel.item("Cave_Pickaxe", "item/iron_pickaxe.png"), grip=(-5, -5))
    swing = anim.cycle(1.5, [(0.0, (-150, 0, 0)), (0.28, (-58, 0, 0)), (0.5, (-70, 0, 0)), (1.2, (-150, 0, 0))])  # he mines the diamonds, for ever
    live(steve.parts["arm_r"], "rotation_euler", lambda t: rad(*swing(t)))
    bow = anim.cycle(1.5, [(0.0, (12, 0, 0)), (0.28, (22, 0, 0)), (1.2, (12, 0, 0))])
    live(steve.parts["body"], "rotation_euler", lambda t: rad(*bow(t)))
    alex = boxmodel.player("Cave_Alex", "alex", slim=True)
    home = V(1.9, 4.6, floor_z(1.9, 4.6))
    alex.place(home, yaw=-24)
    alex.pose(body=(-6, 0, 0), head=(-12, 6, -16), arm_r=(0, 152, 0), hand_r=(0, 24, 0), arm_l=(0, -140, 0), hand_l=(0, -30, 0), leg_r=(-38, 0, 0), foot_r=(62, 0, 0), leg_l=(14, 0, 0), foot_l=(34, 0, 0))
    live(alex.root, "location", lambda t: tuple(home + V(0, 0, 0.42 * abs(math.sin(t * math.pi / 0.62)))))  # she cannot keep still
    for rig, label, lift, joins, leaves in ((steve, "JAVA", 2.62, 1.0, 0.25), (alex, "BEDROCK", 2.75, 1.35, 0.55)):
        tag = font.text_object(f"Cave_Tag_{label}", [label], b, mats["type"], "Type", palette="cream", cell=0.034, depth=2)
        tag.location = (rig.root.location.x, rig.root.location.y, rig.root.location.z + lift)
        tag.rotation_euler = rad(-6, 0, 0)
        for obj, late in ((rig.root, 0.0), (tag, 0.12)):
            wakes.append(lambda start, clock, obj=obj, at=joins + late: anim.TL.add(obj, "scale", start, lambda s, dt: tuple(Vector(one) * anim._POP((dt - at) / 0.32)), clock))
            sleeps.append(lambda start, clock, obj=obj, at=leaves: anim.TL.add(obj, "scale", start, lambda s, dt: tuple(Vector(one) * anim._UNPOP((dt - at) / 0.22)), clock))

    # Now and then a diamond flies from his pick into the chest behind him.
    gem_at = core.empty("Cave_Diamond_At", "Props")
    gem = boxmodel.item("Cave_Diamond", "item/diamond.png")
    gem.parent = gem_at
    switched(gem_at, 1.4, 0.25)
    struck, kept = Vector(steve.root.location) + V(-1.2, -0.25, 1.05), Vector(chest.location) + V(0.0, 0.0, 0.7)  # from the rock in front of him, over his head

    def flight(t):
        return anim.clamp01((t % 4.5 - 0.3) / 0.75)  # one in three swings, from the moment the pick lands

    live(gem, "location", lambda t: tuple(struck.lerp(kept, flight(t)) + V(0, 0, 4 * 1.5 * flight(t) * (1 - flight(t)))))
    live(gem, "rotation_euler", lambda t: (0.0, 0.0, t * 7.0))
    live(gem, "scale", lambda t: (0.36 * smooth(flight(t) / 0.08) * (1.0 - smooth((flight(t) - 0.82) / 0.18)),) * 3)
    # Bats under the roof: either side of the headline in the wide frame, above it in the tall one.
    bat_wing = core.image_from_pixels("bat_wing", life.picture(("kkkkkkk.", "kmmkmmkk", "kmkkmkmk", ".kk.kk.k"), {"k": (0.20, 0.15, 0.15), "m": (0.36, 0.28, 0.26)}), core.EXPORT / "bat_wing.png")
    bat_body = blk.flat_material("Bat", (0.25, 0.19, 0.18), emit=0.5, roughness=1.0)  # (a little light of their own: the roof is dark)
    for i, (haunt, reach) in enumerate(P((((6.4, 5.8, -20.0), (1.3, 1.6, 0.4)), ((-4.9, 5.4, -20.1), (1.0, 1.4, 0.4))), (((1.6, 4.4, -16.1), (3.2, 2.0, 0.3)), ((0.4, 5.2, -15.9), (2.8, 1.8, 0.25))))):
        life.flier(f"Cave_Bat_{i}", bat_wing, (0.4, 0.3), haunt, reach, speed=2.3 - 0.4 * i, beat=7.5, seed=5.1 * i + 1.0, body=(0.12, bat_body), col="Props", emit=0.5)
    # Drops from the roof, each with its ring on the floor.
    ripple = scenery.fading_material("Ripple", (0.85, 0.94, 1.0), strength=1.2)
    for i, (x, y, period) in enumerate(((4.6, 5.8, 3.7), (-1.4, 6.6, 5.3), (3.9, 4.4, 4.4))):
        roof = floor_z(x, y)
        while roof < -12 and w.get(int(math.floor(x)), int(math.floor(y)), int(roof)) == 0:
            roof += 1.0
        drop = scenery.boxes_object(f"Cave_Drop_{i}", [((-0.02, -0.02, 0.0), (0.04, 0.04, 0.2))], mats["line"], "FX")
        drop.visible_shadow = False
        life.drip(f"Cave_Drip_{i}", (x, y, roof - 0.2), floor_z(x, y), period, 1.3 * i, drop, scenery.ring(f"Cave_Drip_Ring_{i}", ripple, thick=0.12))
        S.setdefault("drips", []).append((x, y, roof - 0.2, floor_z(x, y), period, 1.3 * i))

    # One headline for each state, in the same place.
    for key, lines, palettes, material, table_in, table_out, at_in, at_out in (
        ("Awake", P(["SLEEPS WHEN YOU LEAVE.", "WAKES WHEN YOU JOIN."], ["SLEEPS WHEN", "YOU LEAVE.", "WAKES WHEN", "YOU JOIN."]), P(("cream", "gold"), ("cream", "cream", "gold", "gold")), "type", wakes, sleeps, 1.6, 0.5),
        ("Asleep", P(["NOBODY HOME. IT SLEEPS.", "EVERY BLOCK SAVED."], ["NOBODY HOME.", "IT SLEEPS.", "EVERY BLOCK", "SAVED."]), P(("cream", "gold"), ("cream", "cream", "gold", "gold")), "type_dim", sleeps, rewakes, 1.7, 0.0),
    ):
        headline = type_block(f"Cave_{key}", lines, P((0.5, 6.5, -20.7), (1.1, 6.5, -20.4)), palettes, cell=P(0.065, 0.09), depth=4, leading=3, tilt=-8, material=material, scrim=0.5)
        hidden(headline)
        table_in.append(lambda start, clock, obj=headline, at=at_in: anim.TL.add(obj, "scale", start, lambda s, dt: tuple(Vector(one) * anim._POP((dt - at) / 0.28)), clock))
        table_out.append(lambda start, clock, obj=headline, at=at_out: anim.TL.add(obj, "scale", start, lambda s, dt: tuple(Vector(one) * anim._UNPOP((dt - at) / 0.2)), clock))

    # The snapshot: the world as one small block that keeps its own light. Waking, it
    # swells and a fine frame races outward from it: the world unfolding. Going to
    # sleep, the world folds back into it with a flash.
    sx, sy = SNAPSHOT
    sz = floor_z(sx, sy) + 1.25
    snap = core.empty("Snapshot", "Props", (sx, sy, sz))
    snap.rotation_euler = rad(*SNAP_TILT)
    for part in (scenery.snapshot_block("Snapshot_World", b, mats["mini"], "Props", size=0.86), scenery.frame_cube("Snapshot_Edge", 1.0, 0.022, mats["glow_edge"], "Props"), scenery.boxes_object("Snapshot_Glass", [((-0.5,) * 3, (1.0,) * 3)], mats["glass"], "Props")):
        part.parent = snap
    wave = scenery.frame_cube("Snapshot_Wave", 1.0, 0.012, mats["wave"], "Props")
    wave.location, wave.rotation_euler = snap.location, snap.rotation_euler
    hidden(wave)
    fade = mats["wave"].node_tree.nodes["Principled BSDF"].inputs["Alpha"]
    gold = (1.0, 0.72, 0.32)
    flash = look.point("Cave_Unfold", (sx, sy - 0.2, sz + 0.2), 0.0, gold, radius=0.6, shadow=False)
    switch(flash, "hide_render", near)
    folded = 1.5  # how long into going to sleep the block is back

    def burst(peak, at=0.0):
        return lambda dt: peak * max(1.0 - abs(dt - at - 0.12) / (0.12 if dt - at < 0.12 else 0.36), 0.0)

    wakes.append(lambda start, clock: after(snap, "scale", start, lambda dt: (lerp(1.0, 1.9, smooth(dt / 0.16)),) * 3 if dt < 0.16 else zero, clock))
    wakes.append(lambda start, clock: after(wave, "scale", start, lambda dt: (1.0 + 13.0 * (dt / 0.5) ** 1.5,) * 3 if dt < 0.5 else zero, clock))
    wakes.append(lambda start, clock: after(fade, "default_value", start, lambda dt: max(1.0 - dt / 0.5, 0.0) ** 1.5, clock))
    wakes.append(lambda start, clock: after(flash.data, "energy", start, burst(1000.0), clock))
    sleeps.append(lambda start, clock: anim.TL.add(snap, "scale", start, lambda s, dt: tuple(Vector(one) * anim._POP((dt - folded) / 0.3)), clock))
    sleeps.append(lambda start, clock: after(flash.data, "energy", start, burst(420.0, folded), clock))
    for name, at, energy in (("SnapLight", (sx - 0.8, sy - 0.6, sz + 0.1), 40.0), ("SnapFloor", (sx, sy - 0.3, sz - 0.9), 60.0)):
        glow = look.point(f"Cave_{name}", at, energy, gold, radius=0.4, shadow=False)
        switch(glow, "hide_render", near)
        coming = anim.linear([(2.3, 0.0), (2.45, energy)])
        anim.TL.add(glow.data, "energy", EVER, lambda s, dt, coming=coming: coming(s), "real")
        wakes.append(lambda start, clock, glow=glow, energy=energy: after(glow.data, "energy", start, lambda dt: energy * (1.0 - anim.clamp01(dt / 0.1)), clock))
        sleeps.append(lambda start, clock, glow=glow, energy=energy: after(glow.data, "energy", start, lambda dt: energy * anim.clamp01((dt - folded) / 0.3), clock))

    # The sleeper's Zs, which rise beside the pumpkin while the cave is asleep. In the
    # session where it works the lever itself, it dozes off right beside it.
    lx, ly = LEVER
    S["lever_side"] = V(lx - 1.15, ly - 0.9, floor_z(lx - 1.15, ly - 0.9))  # in front of it, clear of the block
    bed = S["lever_side"] if session.at("demo_off") is not None else V(kx, ky, floor_z(kx, ky))

    def z_at(u):
        return tuple((bed + V(-0.5, -0.2, 1.1)).lerp(bed + V(-1.5, -0.2, 2.1), u) + V(0, 0, 0.07 * math.sin(u * 9.0)))

    for i, cell in enumerate((0.024, 0.032, 0.042)):
        z = font.text_object(f"Cave_Zzz_{i}", ["Z"], b, mats["type_dim"], "Type", palette="cream", cell=cell, depth=2)
        z.rotation_euler = rad(0, -10 - i * 8, 8)

        def risen(t, i=i):
            return (t / 2.7 + i / 3.0) % 1.0

        def z_size(cell=cell, risen=risen):
            u = risen(anim.now())
            return lerp(0.022, 0.046, u) / cell * smooth(u / 0.12) * smooth((1.0 - u) / 0.18)

        live(z, "location", lambda t, risen=risen: z_at(risen(t)))
        hidden(z)
        sleeps.append(lambda start, clock, z=z, z_size=z_size: after(z, "scale", start, lambda dt: (z_size() * anim.clamp01((dt - 1.5) / 0.3),) * 3, clock))
        rewakes.append(lambda start, clock, z=z, z_size=z_size: after(z, "scale", start, lambda dt: (z_size() * (1.0 - anim.clamp01(dt / 0.15)),) * 3, clock))

    # The lever: on a lamp in the right foreground, far bigger than life, at the height
    # of the lens and clear of the headline.
    lz = floor_z(lx, ly)
    for state, block in (("off", "lamp_off"), ("on", "lamp")):
        lamp = voxel.grid_object(f"Lever_Lamp_{state}", np.full((1, 1, 1), b[block], np.uint8), b, mats["lamp"], "Props", origin=(-0.5, -0.5, 0.0))
        lamp.location = (lx, ly, lz)
        wakes.append(lambda start, clock, lamp=lamp, on=state == "on": after(lamp, "scale", start, lambda dt: one if on == (dt > 0.0) else zero, clock))
        sleeps.append(lambda start, clock, lamp=lamp, on=state == "on": after(lamp, "scale", start, lambda dt: zero if on == (dt > 0.0) else one, clock))
    base, handle = boxmodel.lever("Lever", on=False)
    base.location = (lx, ly, lz + 1.0)
    base.scale = (2.4,) * 3
    wakes.append(lambda start, clock: after(handle, "rotation_euler", start, lambda dt: (0.0, math.radians(lerp(-40.0, 40.0, smooth(dt / 0.12))), 0.0), clock))
    sleeps.append(lambda start, clock: after(handle, "rotation_euler", start, lambda dt: (0.0, math.radians(lerp(40.0, -40.0, smooth(dt / 0.12))), 0.0), clock))
    lit(look.point("Lever_Glow", (lx, ly - 0.9, lz + 0.5), 22.0, (1.0, 0.5, 0.2), radius=0.3, shadow=False), 22.0, 0.0, 0.0, rise=0.3, fall=0.2)
    # A frame that breathes round the lever: this is the thing to touch here.
    hint = scenery.frame_cube("Lever_Hint", 1.0, 0.03, scenery.fading_material("Hint", (1.0, 0.82, 0.3), strength=2.2), "FX")
    hint.location, hint.scale = (lx, ly, lz + 1.65), (0.95, 0.75, 1.5)
    hint.visible_shadow = False
    live(hint, "color", lambda t: (1.0, 1.0, 1.0, 0.18 + 0.4 * (0.5 + 0.5 * math.sin(t * 2 * math.pi / 1.5))))
    show(hint, 3.12, 3.6)

    # Who plays the sequences. Arriving wakes it; then whatever this session's visitor,
    # or the pumpkin itself, does with the lever.
    toggles = [("wake", ("scrub", ARRIVAL_RATE), WAKE)]  # arriving, the scroll plays it: done before the stop
    for key, what in (("lever_off", "sleep"), ("lever_on", "wake"), ("demo_off", "sleep"), ("demo_on", "wake")):
        if session.at(key) is not None:
            toggles.append((what, "real", session.at(key)))
    toggles.sort(key=lambda toggle: toggle[2] if toggle[1] == "real" else session.when(toggle[2]))
    for i, (what, clock, start) in enumerate(toggles):
        for step in (wakes + (rewakes if i else [])) if what == "wake" else sleeps:
            step(start, clock)
    S.update(snap=snap, lever=(base, handle), cave_toggles=toggles, cave_folded=folded)


# -- the cart ride -------------------------------------------------------------------
def rail_path():
    """The cart's path: points along the rails, with the turn taken as a cart takes it
    (half a block of radius), and the distance along the path at each point."""
    (x0, y0, z0), (x1, y1, _), (x2, _, _), (x3, _, z3), (x4, _, _) = wld.SPOTS["rail"]
    pts = [V(x0, y0, z0), V(x1, y1 - 0.5, z0)]
    for angle in (22.5, 45.0, 67.5):
        pts.append(V(x1 + 0.5 - 0.5 * math.cos(math.radians(angle)), y1 - 0.5 + 0.5 * math.sin(math.radians(angle)), z0))
    pts += [V(x1 + 0.5, y1, z0), V(x2, y1, z0), V(x3, y1, z3), V(x4, y1, z3)]
    far = [0.0]
    for p, q in zip(pts, pts[1:]):
        far.append(far[-1] + (q - p).length)
    return pts, far


def rail_at(d):
    """Position on the rails at a distance along them."""
    pts, far = S["rail"]
    d = min(max(d, 0.0), far[-1])
    i = min(int(np.searchsorted(far, d, side="right")) - 1, len(pts) - 2)
    return pts[i].lerp(pts[i + 1], (d - far[i]) / (far[i + 1] - far[i]))


def rail_heading(d, reach=0.35):
    return (rail_at(d + reach) - rail_at(d - reach)).normalized()


def set_ride():
    """Rails from the cave to the dungeon, the cart, and a few torches in the tunnel."""
    b, mats = S["b"], S["mats"]
    S["rail"] = rail_path()
    length = S["rail"][1][-1]
    # How far along the rails the cart is at each scroll position.
    S["ride"] = anim.Track([(BOARD, 0.0), (BOARD + 0.06, 2.5), (ARRIVE - 0.07, length - 3.0), (ARRIVE, length)])

    (x0, y0, z0), (x1, y1, _), (x2, _, _), (x3, _, z3), (x4, _, _) = wld.SPOTS["rail"]
    cx, cy, lift = int(math.floor(x1)), int(math.floor(y1)), 0.03
    along_y = [(0, 0), (1, 0), (1, 1), (0, 1)]
    along_x = [(0, 1), (0, 0), (1, 0), (1, 1)]
    quads, uvs = [], []
    for y in range(int(math.floor(y0)) - 1, cy):  # north out of the cave
        quads.append([(cx, y, z0 + lift), (cx + 1, y, z0 + lift), (cx + 1, y + 1, z0 + lift), (cx, y + 1, z0 + lift)])
        uvs.append(along_y)
    for x in range(cx + 1, int(x2)):  # east, level
        quads.append([(x, cy, z0 + lift), (x + 1, cy, z0 + lift), (x + 1, cy + 1, z0 + lift), (x, cy + 1, z0 + lift)])
        uvs.append(along_x)
    for x in range(int(x2), int(x3)):  # the slope
        za, zb = z0 - (x - x2) + lift, z0 - (x + 1 - x2) + lift
        quads.append([(x, cy, za), (x + 1, cy, zb), (x + 1, cy + 1, zb), (x, cy + 1, za)])
        uvs.append(along_x)
    for x in range(int(x3), int(math.floor(x4)) + 1):  # the run in
        quads.append([(x, cy, z3 + lift), (x + 1, cy, z3 + lift), (x + 1, cy + 1, z3 + lift), (x, cy + 1, z3 + lift)])
        uvs.append(along_x)
    boxmodel.tiles("Rails", "block/rail.png", quads, uvs)
    boxmodel.tiles("Rail_Turn", "block/rail_corner.png", [[(cx, cy, z0 + lift), (cx + 1, cy, z0 + lift), (cx + 1, cy + 1, z0 + lift), (cx, cy + 1, z0 + lift)]])
    stop = np.full((1, 1, 1), b["log"], np.uint8)
    voxel.grid_object("Rail_Stop", stop, b, mats["voxel"], "Props", origin=(math.floor(x4) + 1.0, cy, z3))

    cart = boxmodel.minecart("Cart")
    cart.scale = (1.3,) * 3

    def cart_at(s):
        return rail_at(S["ride"](s))

    def cart_turn(s):
        h = rail_heading(S["ride"](s))
        return (math.asin(max(min(h.z, 1.0), -1.0)), 0.0, math.atan2(-h.x, h.y))

    scrub(cart, "location", BOARD, lambda s: tuple(cart_at(s)))
    scrub(cart, "rotation_euler", BOARD, cart_turn)
    S["cart"] = cart

    warm = (1.0, 0.54, 0.18)
    for i, (x, y) in enumerate(((1.6, 12.4), (-0.6, 18.6), (10.6, 18.6))):
        torch = boxmodel.torch(f"Ride_Torch_{i}")
        torch.location = on_floor(x, y, start=-22 if x < 5 else -27)
        lamp = look.point(f"Ride_TorchLight_{i}", (x, y, torch.location.z + 0.85), 120, warm, radius=0.15)
        light_ramp(lamp, [(3.2, 0.0), (3.3, 120.0), (4.2, 120.0), (4.3, 0.0)])


# -- the dungeon -----------------------------------------------------------------------
ORES = ("CONSOLE", "LOGS", "PLAYERS", "SETTINGS", "ACTIVITY")  # redstone, coal, gold, lapis, copper
ORE_LINES = (["Type a command.", "Things happen."], ["Read what happened", "while you weren't looking."], ["Friends in.", "Everyone else out."], ["Survival today.", "Creative tomorrow."], ["Who did what, and when."])
SIGNS = (("/AI ASK", "GIVE EVERYONE", "NIGHT VISION"), ("/AI CODE HOME", "ADD A /HOME", "COMMAND"), ("/AI REVERT", "HOME"))
ASKED = (("/ai ask give everyone night vision", "Done now."), ("/ai code home add a /home command", "Becomes a real command, no restart."), ("/ai revert home", "Oops, undone."))
ORE_VIEW, SPAWNER_VIEW = (4.14, 4.31), (4.46, 4.78)  # the scroll over which the camera rests on each chamber


def set_dungeon():
    """What stands in the dungeon. In the south bay, the ore wall with its labels and
    the backups chest. In the north bay, the spawner under the three things you can
    ask it, and the portal that waits unlit in the wall."""
    b, mats = S["b"], S["mats"]
    d, ow = wld.SPOTS["dungeon"], ore_wall()
    (x0, x1), (y0, y1), floor, height = d["x"], d["y"], d["floor"], d["height"]  # (its lamps hang at this height, however high the room)
    wall = x1 + 1.0  # the east wall's face
    proud = float(x1)  # the face of the ores, which stand a block out from it
    look.fog("Dungeon_Haze", (x0 - 1, y0 - 1, floor - 1), (x1 + 2, y1 + 2, floor + ow["height"] + 1), density=0.01, color=(1.0, 0.9, 0.8), anisotropy=0.4)
    lit = [(3.85, 0.0), (3.95, 1.0), (THROUGH, 1.0), (THROUGH + 0.05, 0.0)]

    def lamp(name, at, energy, color=(1.0, 0.54, 0.18), radius=0.15, shadow=True, flame=None, breath=None):
        """A light that is on while the dungeon can be seen. A flame wavers (`flame` is
        its phase); an ore's glow breathes slowly."""
        light = look.point(name, at, energy, color, radius=radius, shadow=shadow)
        if flame is None and breath is None:
            light_ramp(light, [(s, energy * k) for s, k in lit])
            return
        shown = anim.linear(lit)
        waver = (lambda: flicker(anim.now(), flame)) if flame is not None else (lambda: 1.0 + 0.16 * math.sin(anim.now() * 1.3 + breath))
        switch(light, "hide_render", [(lit[0][0] - 1.0, True), (lit[0][0], False), (lit[-1][0], True)])
        anim.TL.add(light.data, "energy", EVER, lambda s, dt: energy * shown(s) * waver(), "real")

    # South bay: the ore wall, read left to right as you face it.
    ore_mid = (d["ores"][0] + d["ores"][-1]) / 2 + 1.0
    tiles = ow["tiles"]
    type_block("Dungeon_Headline", P(["YOUR SERVER. YOUR RULES."], ["YOUR SERVER.", "YOUR RULES."]), (wall - 4 * 0.075, ore_mid, floor + P(5.55, 9.5)), "gold", cell=0.075, depth=4, yaw=-90, scrim=0.45, feather=0.6)
    for i, (label, (y, up)) in enumerate(zip(ORES, tiles)):
        type_block(f"Dungeon_Ore_{i}", [label], (proud - 2 * 0.045, y + 1.0, floor + up - 0.54), "cream", cell=0.045, depth=2, yaw=-90)
        lamp(f"Dungeon_OreLight_{i}", (proud - 1.3, y + 1.0, floor + up + 1.6), 55, (1.0, 0.8, 0.6), radius=0.3, shadow=False, breath=1.9 * i)
    # The backups chest: on the floor under the row, or two side by side on their ledge as the sixth tile.
    cy, cup, shelf = ow["chest"]
    lids = []
    for k, dy in enumerate((-0.5, 0.5) if shelf else (0.0,)):
        chest, lid = boxmodel.chest(f"Dungeon_Chest_{k}", open_deg=-8)
        chest.location = (wall - 0.55, cy + dy, floor + cup)
        chest.rotation_euler = rad(0, 0, -90)
        lids.append(lid)
    if shelf:
        type_block("Dungeon_Backups", ["BACKUPS"], (proud - 2 * 0.045, cy, floor + cup - 1 - 0.54), "cream", cell=0.045, depth=2, yaw=-90)
    else:
        type_block("Dungeon_Backups", ["BACKUPS"], (proud - 2 * 0.05, cy, floor + 1.0), "cream", cell=0.05, depth=2, yaw=-90)  # (low, clear of what the ores say)
    # Torches at the foot of the wall: between each pair of ores in the row, or either side of the two columns.
    for i, y in enumerate(P([y - 0.5 for y, _ in tiles[:-1]], [22.7, 15.3])):
        torch = boxmodel.torch(f"Dungeon_Torch_{i}")
        torch.location = (wall - 0.4, y, floor)
        lamp(f"Dungeon_TorchLight_{i}", (wall - 0.7, y, floor + 0.9), 80, shadow=False, flame=2.3 * i)

    # North bay: the spawner on a mossy block, under a wall of three signs.
    board_w, board_h, gap = 3.0, 1.05, 0.1
    sign_y = y1 - 0.5
    for i, lines in enumerate(SIGNS):
        board = boxmodel.sign(f"Sign_{i}", post=False)
        board.location = (wall - 0.02, sign_y, floor + 1.75 + (2 - i) * (board_h + gap))
        board.rotation_euler = rad(0, 0, -90)
        board.scale = (board_w / 1.5, 1.0, board_h / 0.75)
        rows = len(lines) * font.ROWS + (len(lines) - 1) * 3
        cell = 0.031
        type_block(f"Sign_{i}_Text", lines, (wall - 0.03 - 1 / 16 - cell, sign_y, floor + 1.75 + (2 - i) * (board_h + gap) + (board_h - rows * cell) / 2), "ink", cell=cell, depth=1, leading=3, yaw=-90)
    cell = P(0.042, 0.06)
    type_block("Spawner_Headline", P(["ASK THE SPAWNER ANYTHING."], ["ASK THE SPAWNER", "ANYTHING."]), (wall - 4 * cell, sign_y - 2.0, floor + P(5.62, 5.78)), "gold", cell=cell, depth=4, yaw=-90, scrim=0.45, feather=0.5)
    sx, sy = wall - 2.5, sign_y
    voxel.grid_object("Spawner_Stand", np.full((1, 1, 1), b["mossy_cobblestone"], np.uint8), b, mats["voxel"], "Props", origin=(sx - 0.5, sy - 0.5, floor))
    boxmodel.cage("Spawner").location = (sx, sy, floor + 1.0)
    flame = voxel.grid_object("Spawner_Core", np.full((1, 1, 1), b["jack_o_lantern"], np.uint8), b, mats["lamp"], "Props", scale=0.36, origin=(-0.18, -0.18, -0.18))
    # Inside the cage the little lantern turns and bobs, as whatever a spawner holds does, and sparks rise round it.
    live(flame, "location", lambda t: (sx, sy, floor + 1.46 + 0.035 * math.sin(t * 2.1)))
    live(flame, "rotation_euler", lambda t: (0.0, 0.0, math.radians(-62.0) + t * 1.7))
    lo, hi = (sx - 0.4, sy - 0.4, floor + 1.06), (sx + 0.4, sy + 0.4, floor + 1.94)
    life.drift(scenery.leaves("Spawner_Sparks", mats["ember"], "FX", lo=lo, hi=hi, count=12, size=(0.02, 0.04), seed=29), (0.0, 0.0, 0.42), lo, hi, sway=0.08)
    lamp("Spawner_Glow", (sx - 0.9, sy, floor + 1.7), 16, (1.0, 0.6, 0.2), shadow=False, flame=4.4)
    lamp("Sign_Light", (wall - 2.2, sign_y - 0.2, floor + 4.4), 120, (1.0, 0.82, 0.62), radius=0.5, shadow=False)
    type_block("Spawner_Small", ["OPERATORS ONLY."], (wall - 2 * 0.03, sign_y - 2.0, floor + P(5.24, 5.34)), "cream", cell=P(0.03, 0.036), depth=2, yaw=-90)
    for i, (x, y) in enumerate(((wall - 0.4, y1 + 0.6), (x0 + 0.5, y0 + 0.5), (x0 + 0.5, y1 + 0.5), (x0 + 0.5, d["parting"][0] + 0.5))):
        torch = boxmodel.torch(f"Dungeon_Torch_n{i}")
        torch.location = (x, y, floor)
        lamp(f"Dungeon_TorchLight_n{i}", (x - 0.3 if i == 0 else x + 0.3, y, floor + 0.9), 110, shadow=i > 0, flame=3.1 * i + 0.7)
    for i, y in enumerate((ore_mid, sign_y - 2.0)):  # just enough to read each bay
        lamp(f"Dungeon_Base_{i}", (x0 + 4.5, y, floor + height - 0.8), 170, (1.0, 0.74, 0.46), radius=0.8, shadow=False)
    dust_lo, dust_hi = (x0 + 2.0, y0 + 0.5, floor + 0.3), (wall - 0.3, y1 + 0.5, floor + 5.6)
    dust = scenery.leaves("Dungeon_Dust", scenery.fading_material("Dust", (1.0, 0.8, 0.5), strength=0.9), "FX", lo=dust_lo, hi=dust_hi, count=110, size=(0.008, 0.018), seed=17)
    dust.color = (1.0, 1.0, 1.0, 0.45)
    life.drift(dust, (0.05, 0.04, 0.04), dust_lo, dust_hi, sway=0.06)

    # The portal in the east wall: dark until the pumpkin jumps into the frame.
    p0, p1 = d["portal"]
    gate = V(wall + 0.5, (p0 + p1 + 1) / 2, floor)
    surface = portal_surface("Dungeon_Portal", gate, along=(0, -1, 0))
    pop(surface, PORTAL_LIT, 0.2)
    for name, at, energy in (("a", gate + V(-1.5, 0, 2.5), 700.0), ("b", gate + V(-3.5, 0, 0.8), 240.0)):
        light_window(look.point(f"Dungeon_PortalGlow_{name}", at, energy, (0.62, 0.22, 1.0), radius=0.7, shadow=name == "a"), PORTAL_LIT, THROUGH + 0.02, fade_in=0.1, fade_out=0.0)
    sparks = scenery.leaves("Dungeon_Sparks", mats["spark"], "FX", lo=tuple(gate + V(-5.5, -3, 0.4)), hi=tuple(gate + V(-0.5, 3, 5.5)), count=60, size=(0.025, 0.06), seed=9)
    show(sparks, PORTAL_LIT)
    S["gate"] = gate

    # -- what the dungeon does while you look at it, in real time ------------------------
    session = S["session"]
    hint = scenery.fading_material("Hint", (1.0, 0.82, 0.3), strength=2.2)

    def outline(name, at, size):
        frame = scenery.frame_cube(name, 1.0, 0.035, hint, "FX")
        frame.location, frame.scale, frame.visible_shadow = at, size, False
        return frame

    def rounds(first, each, count, chosen):
        """Whose turn it is and since when: [(moment, which)]. Left alone, they take turns
        of `each` seconds from `first`. What a visitor points at or clicks (`chosen`) has
        its turn at once, and the round goes on from there."""
        turns, moment, which = [], first, 0
        for at, k in sorted(chosen) + [(session.duration, None)]:
            while moment < at - 0.3:
                turns.append((moment, which))
                moment, which = moment + each, (which + 1) % count
            if k is not None:
                turns.append((at, k))
                moment, which = at + each, (k + 1) % count
        return turns

    def turn_of(spans, which, each, view):
        """How much `which` shows at scroll s and moment tau: it fades in for its turn and
        out before the next, and only while the camera is on its chamber."""
        def fn(s, tau):
            if not view[0] <= s < view[1]:
                return 0.0
            live_ = [(moment, k) for moment, k in spans if moment <= tau]
            if not live_ or live_[-1][1] != which:
                return 0.0
            age = tau - live_[-1][0]
            return anim.clamp01(age / 0.15) * (1.0 - anim.clamp01((age - each + 0.25) / 0.2))

        return fn

    # The ore wall: each ore says what it is for, one after another; the chest opens for backups.
    each = 2.3
    # (title, lines, middle, size of its frame, where its lines hang from). An ore has its name
    # under it already. In the row its lines hang under that name; in the grid, where the next
    # tile (or the pumpkin) is right below, they lie across the lower half of the tile itself.
    hang = P(-0.58, 1.35)  # from the tile's foot
    topics = [(None, lines, V(proud, y + 1.0, floor + up + 1.0), (0.25, 2.2, 2.2), floor + up + hang) for lines, (y, up) in zip(ORE_LINES, tiles)]
    backups = ["Creeper took the house?", "Ctrl+Z for the whole world."]
    topics.append((None, backups, V(proud, cy, floor + cup), (0.25, 2.2, 2.2), floor + cup - 1 + hang) if shelf else ("Backups", backups, V(wall - 0.55, cy, floor + 0.5), (1.1, 1.1, 1.1), None))
    pointed = [(session.at(f"tile_{k}"), k) for k in range(len(topics)) if session.at(f"tile_{k}") is not None]
    spans = rounds(session.when(ORE_VIEW[0]), each, len(topics), pointed)
    S.update(tile_pointed=pointed, tile_spots=[at for _, _, at, _, _ in topics])
    for k, (title, lines, at, size, hangs) in enumerate(topics):
        showing = turn_of(spans, k, each, ORE_VIEW)
        frame = outline(f"Dungeon_Pick_{k}", at, size)
        anim.TL.add(frame, "color", EVER, lambda s, dt, showing=showing: (1.0, 1.0, 1.0, 0.9 * showing(s, anim.now())), "real")
        under = hangs is not None  # what a tile says hangs under its label; the chest on the floor has a tooltip beside it
        S.setdefault("tips", []).append({"name": f"topic_{k}", "title": title, "lines": lines, "at": V(at.x, at.y, hangs) if under else at + V(0, -2.0, 0.3), "s": 4.22, "showing": showing, "under": under})
        if k == len(topics) - 1:  # the chest opens for its turn
            for lid in lids:
                anim.TL.add(lid, "rotation_euler", EVER, lambda s, dt, showing=showing: (math.radians(lerp(-8.0, -64.0, showing(s, anim.now()))), 0.0, 0.0), "real")

    # The spawner: the three things on the signs are asked in turn, and the room does them.
    # Night vision lights the room; /home is written and the pumpkin uses it; revert takes it back.
    each = 3.4
    picked = [(session.at(f"sign_{k}"), k) for k in range(3) if session.at(f"sign_{k}") is not None]  # the visitor clicks a sign: that one is asked now
    begun, leave = session.when(SPAWNER_VIEW[0]), session.when(SPAWNER_VIEW[1])
    asked = [(moment, k) for moment, k in rounds(begun, each, 3, picked) if moment < leave - 1.0]
    S.update(sign_picked=picked, sign_spots=[V(wall - 0.2, sign_y, floor + 1.75 + (2 - k) * (board_h + gap) + board_h / 2) for k in range(3)])
    for k in range(3):
        showing = turn_of(asked, k, each, SPAWNER_VIEW)
        frame = outline(f"Sign_Pick_{k}", (wall - 0.2, sign_y, floor + 1.75 + (2 - k) * (board_h + gap) + board_h / 2), (0.3, board_w + 0.12, board_h + 0.1))
        anim.TL.add(frame, "color", EVER, lambda s, dt, showing=showing: (1.0, 1.0, 1.0, 0.95 * showing(s, anim.now())), "real")
    vision = look.area("Dungeon_NightVision", (x0 + 5.0, sign_y - 2.5, floor + height - 0.4), (wall - 2.0, sign_y - 2.5, floor + 1.0), energy=0.0, color=(0.72, 0.9, 1.0), size=9)
    switch(vision, "hide_render", [(0.0, True), (4.3, False), (THROUGH, True)])
    home_at = V(22.35, gate.y - P(1.5, 0.9), floor)  # by the portal's right post, where the camera sees the floor
    home = boxmodel.sign("Home_Sign")
    home.location, home.rotation_euler, home.scale = home_at, rad(0, 0, -72), (0.6,) * 3
    word = type_block("Home_Sign_Text", ["/HOME"], (0, 0, 0), "ink", cell=0.05, depth=1, parent=home)
    word.location = (0, -1 / 16 - 0.05, (14 + 6) / 16 - 4 * 0.05)
    hidden(home)
    trips, away = [], None
    for moment, k in asked:
        S.setdefault("says", []).append((moment + 0.15, "> " + ASKED[k][0], hud.WHITE))
        S["says"].append((moment + 1.0, "[AI] " + ASKED[k][1], hud.GREEN))
        if k == 0:
            live(vision.data, "energy", lambda dt: 1250.0 * anim.clamp01(dt / 0.4) * (1.0 - anim.clamp01((dt - 1.9) / 0.5)), start=moment + 1.0)
        elif k == 1:
            pop(home, moment + 1.0, 0.3, base=(0.6,) * 3, clock="real")
            S["says"].append((moment + 1.9, "<Kurbis> /home", hud.WHITE))
            away = moment + 2.1
        elif away is not None:
            unpop(home, moment + 1.0, 0.25, base=(0.6,) * 3, clock="real")
            trips.append((away, moment + 1.3))
            away = None
    if away is not None:
        trips.append((away, None))
    S["home_trips"], S["home_spot"] = trips, home_at + V(-0.75, 1.7, 0)  # beside its sign, not in front of it


def portal_surface(name, foot, along=(1, 0, 0)):
    """The swirling surface of a portal, two wide and three tall. `foot` is the middle of
    its bottom edge and `along` the direction it is wide in."""
    if "Portal" not in bpy.data.materials:
        boxmodel.tex_material("Portal", "block/nether_portal.png", clip=False, emit=2.6)
    side, up = Vector(along), V(0, 0, 1)
    quads = [[side * (i - 1) + up * k, side * i + up * k, side * i + up * (k + 1), side * (i - 1) + up * (k + 1)] for i in range(2) for k in range(3)]
    quads = np.array([[tuple(v) for v in q] for q in quads], np.float32)
    uvs = np.array([[(0, 31 / 32), (1, 31 / 32), (1, 1), (0, 1)]] * len(quads), np.float32)
    obj = bpy.data.objects.new(name, core.mesh_from_quads(name, quads, uvs))
    obj.data.materials.append(bpy.data.materials["Portal"])
    obj.location = foot
    return core.link(obj, "Props")


# -- the Nether -------------------------------------------------------------------
def N(x, y, z):
    """Nether coordinates to world coordinates."""
    return Vector(sets.NETHER["origin"]) + Vector((x, y, z))


def set_nether():
    """Out of the portal on its ledge, a short bridge, and a second frame that waits
    unlit under the name field: the end of the page."""
    b, mats = S["b"], S["mats"]
    nether = sets.NETHER
    grid = sets.nether(b)
    grid.object("Nether", mats["nether"], "World", nether["origin"])
    S["nether_grid"] = grid
    ledge, row, gate = nether["ledge"], nether["portal_row"], nether["gate_row"]
    s_in = THROUGH - 0.03

    look.fog("Nether_Haze", N(-70, 2, -4), N(70, 130, 50), density=0.0045, color=(1.0, 0.32, 0.16), anisotropy=0.3)
    portal_surface("Nether_Portal", N(0, row + 0.5, ledge))
    for name, at, energy, color, radius, shadow in (
        ("Portal", N(0, row + 1.6, ledge + 1.5), 800, (0.62, 0.22, 1.0), 0.8, True),
        ("PortalLow", N(0, row + 3.4, ledge + 0.5), 260, (0.62, 0.22, 1.0), 0.6, False),
        ("Sea_a", N(4, 44, 6), 14000, (1.0, 0.34, 0.07), 6.0, False),
        ("Sea_b", N(34, 72, 6), 18000, (1.0, 0.34, 0.07), 6.0, False),
        ("Sea_c", N(-32, 74, 6), 18000, (1.0, 0.34, 0.07), 6.0, False),
        ("Sea_d", N(2, 102, 6), 18000, (1.0, 0.34, 0.07), 6.0, False),
        ("Fall_w", N(-12.5, 17, 12), 1500, (1.0, 0.4, 0.08), 1.5, False),
        ("Fall_e", N(18.5, 17, 12), 1500, (1.0, 0.4, 0.08), 1.5, False),
        ("Ledge", N(2.5, 24, ledge + 5), 900, (1.0, 0.62, 0.42), 2.0, False),
        ("Gate", N(0.5, gate - 3.5, ledge + 4.5), 700, (1.0, 0.66, 0.44), 1.5, True),
    ):
        lamp = look.point(f"Nether_Lamp_{name}", at, energy, color, radius=radius, shadow=shadow)  # (not the names of the two portals' surfaces)
        switch(lamp, "hide_render", [(0.0, True), (s_in, False)])
    # Life, in real time: sparks hang round the portal, embers rise off the sea for ever,
    # the lava's glow breathes, and a strider walks the sea far out.
    lo, hi = tuple(N(-3.5, row + 0.6, ledge + 0.4)), tuple(N(3.5, row + 6, ledge + 6))
    life.drift(scenery.leaves("Nether_Sparks", mats["spark"], "FX", lo=lo, hi=hi, count=70, size=(0.025, 0.06), seed=19), (0.05, 0.12, 0.3), lo, hi, sway=0.12)
    lo, hi = tuple(N(-30, 16, 2)), tuple(N(40, 80, 36))
    life.drift(scenery.leaves("Nether_Embers", mats["ember"], "FX", lo=lo, hi=hi, count=420, size=(0.05, 0.14), seed=21), (0.25, 0.1, 1.3), lo, hi, sway=0.35)
    live(life.crawl(mats["nether"]), "default_value", lambda t: 1.5 * (1.0 + 0.08 * math.sin(t * 1.3) + 0.04 * math.sin(t * 3.7)))
    walker = boxmodel.strider("Strider")
    walker.root.rotation_euler = rad(0, 0, 90)  # walking east
    walker.root.scale = (1.5,) * 3
    live(walker.root, "location", lambda t: tuple(N(-34.0 + 1.1 * t, 78.0, 0.8 + 0.12 * abs(math.sin(t * 5.0)))))
    for key, sign in (("leg_r", 1.0), ("leg_l", -1.0)):
        live(walker.parts[key], "rotation_euler", lambda t, sign=sign: (math.radians(26.0 * sign * math.sin(t * 5.0)), 0.0, 0.0))

    # The name field over the second frame, facing back along the bridge. Until a name is
    # typed it shows a placeholder; a name that is free lights the frame under it.
    session = S["session"]
    y = gate - 0.45
    field = core.empty("Nether_Field", "Type", N(0, y, ledge + 4.45))
    # Wide: the name and the host on one line. Tall: the name over the host, and every
    # line of the block broken short, so the type can be large in the narrow frame.
    scenery_card = scenery.scrim("Nether_Field_Scrim", *P((8.6, 2.6), (7.3, 5.4)), strength=0.78, feather=1.3)
    scenery_card.parent = field
    scenery_card.location = (0, 0.42, P(1.25, 2.3))
    caption = type_block("Nether_Caption", P(["STEP THROUGH TO YOUR WORLD."], ["STEP THROUGH", "TO YOUR WORLD."]), (0, 0, 0), "cream", cell=P(0.042, 0.06), depth=3, parent=field)
    cell, name_w, host_w = P(0.06, 0.09), 58, 71
    total = name_w + 1 + host_w
    left = -(P(total, name_w) / 2) * cell  # where the name begins
    line = P(0.72, 2.55)  # the name's line, above the foot of the block
    name = type_block("Nether_Name", ["YOURNAME"], (left + name_w / 2 * cell, 0, line), "cream", cell=cell, depth=4, material="type_dim", parent=field)
    host = type_block("Nether_Host", [".KURBIS.HOST"], P(((total / 2 - host_w / 2) * cell, 0, 0.72), (0, 0, 1.5)), "gold", cell=cell, depth=4, parent=field)
    bar = voxel.grid_object("Nether_Underline", np.full((name_w, 4, 1), b["type_cream"], np.uint8), b, mats["type"], "Type", group="type", scale=cell, origin=(-name_w * cell / 2, 0, 0))
    bar.parent = field
    bar.location = (left + name_w / 2 * cell, 0, line - 2.2 * cell)
    headline = type_block("Nether_Headline", ["PICK A NAME."], (0, 0, P(1.62, 3.75)), "gold", cell=P(0.1, 0.09), depth=4, parent=field)
    for obj, when in ((headline, 5.66), (name, 5.70), (host, 5.72), (bar, 5.70), (caption, 5.76)):
        pop(obj, when)
    switch(scenery_card, "scale", [(0.0, (0.0, 0.0, 0.0)), (5.6, (1.0, 1.0, 1.0))])

    # The second frame's own portal, dark until then.
    surface = portal_surface("Nether_Gate", N(0, gate + 0.5, ledge))
    hidden(surface)
    glows = [look.point(f"Nether_GateGlow_{k}", at, 0.0, (0.62, 0.22, 1.0), radius=0.7, shadow=k == 0) for k, at in enumerate((N(0, gate - 1.2, ledge + 2.4), N(0, gate - 3.2, ledge + 0.8)))]
    for glow in glows:
        switch(glow, "hide_render", [(0.0, True), (s_in, False)])
    lo, hi = tuple(N(-3, gate - 4.5, ledge + 0.3)), tuple(N(3, gate + 0.3, ledge + 5.2))
    sparks = scenery.leaves("Nether_GateSparks", mats["spark"], "FX", lo=lo, hi=hi, count=60, size=(0.025, 0.06), seed=23)
    life.drift(sparks, (0.04, -0.1, 0.35), lo, hi, sway=0.12)
    hidden(sparks)

    # The visitor types a name (where this session has one who does).
    focus, typing, enter = session.at("name_focus"), session.at("name_type"), session.at("name_enter")
    S["nether_enter"] = enter
    if typing is not None:
        typed, x = "STEVE", 0
        unpop(name, focus if focus is not None else typing - 0.3, 0.15, clock="real")
        for i, ch in enumerate(typed):
            width = font.GLYPHS[ch].shape[1]
            letter = font.text_object(f"Nether_Typed_{i}", [ch], b, mats["type"], "Type", palette="cream", cell=cell, depth=4)
            letter.parent = field
            letter.location = (left + (x + width / 2) * cell, 0, line)
            anim.TL.add(letter, "scale", typing + 0.2 * i, lambda s, dt: (1.0 + 0.2 * math.sin(math.pi * min(dt / 0.12, 1.0)),) * 3 if dt > 0 else (0.0, 0.0, 0.0), "real")
            x += width + 1
        free = typing + 0.2 * len(typed) + 0.7  # the check comes back: it is free
        good = type_block("Nether_Free", P([f"{typed}.KURBIS.HOST IS FREE."], [f"{typed}.KURBIS.HOST", "IS FREE."]), (0, 0, 0), "lime", cell=P(0.042, 0.06), depth=3, parent=field)
        hidden(good)
        unpop(caption, free - 0.1, 0.15, clock="real")
        pop(good, free + 0.05, clock="real")
        pop(surface, free + 0.25, 0.3, clock="real")
        show_sparks = free + 0.3
        live(sparks, "scale", lambda dt: (min(dt / 0.2, 1.0),) * 3, start=show_sparks)
        for glow, energy in zip(glows, (800.0, 260.0)):
            live(glow.data, "energy", lambda dt, energy=energy: energy * anim.clamp01(dt / 0.25), start=free + 0.25)
        S.update(nether_free=free, nether_click=Vector(field.location) + V(left + 1.5, 0, line + 0.23))  # where a visitor clicks to type


# -- the cast -----------------------------------------------------------------
def cast():
    """The pumpkin, as two actors: one for the world it digs through, one for the Nether."""
    session = S["session"]
    a = new_actor("Kurbis")
    steps, through = S["steps"], S["through"]
    fall_from = V(0.4, 9.5, 30.7)

    a.rest(0.0, HERO_PUMPKIN, yaw=-5)
    a.hop(WAKE_UP, 0.31, HERO_PUMPKIN, HERO_PUMPKIN, 0.3, yaw=(-5, 0), antic=0.1, land=0.25)  # startled awake
    a.hop(0.31, FALL[0], HERO_PUMPKIN, fall_from, 0.5, land=0.0)  # shuffles off the edge of the cloud

    def fall(u):
        pos = fall_from.lerp(LANDING_SPOT, u)
        pos.z = lerp(fall_from.z, LANDING_SPOT.z, u**1.8)
        return pose(pos, tilt=(-720 * u, 0, 0), squash=1.0 + 0.16 * smooth(u * 4))

    a.move(*FALL, fall)
    a.move(FALL[1], 0.93, lambda u: pose(LANDING_SPOT, yaw=6, squash=1.0 - 0.5 * (1 - u) ** 2 * math.cos(u * 9.0)))  # impact
    a.rest(0.93, LANDING_SPOT, yaw=6)
    dig_start = V(steps[0][0], 1.0, steps[0][1])
    a.hop(1.46, STOMP, LANDING_SPOT, dig_start, 2.6, yaw=(6, 0), flips=1.0, antic=0.1, land=0.0)
    a.move(STOMP, DIG0, lambda u: pose(dig_start, squash=1.0 - 0.52 * (1 - u) ** 2 * abs(math.cos(u * 10.0))))

    turns = 5.0  # whole turns while it digs
    a.move(DIG0, BREAK, lambda u: pose(dig_position(lerp(DIG0, BREAK, u)), tilt=(0, 360 * turns * S["dig"](lerp(DIG0, BREAK, u)) / through, 0)))
    hole = V(steps[through][0], 1.0, steps[through][1])
    landing = V(hole.x, 1.0, floor_z(hole.x, 1.0))

    def drop(u):
        pos = hole.lerp(landing, u**1.6)
        return pose(pos, tilt=(-540 * u, 0, 0), squash=1.0 + 0.14 * smooth(u * 3))

    a.move(BREAK, LAND, drop)
    a.move(LAND, LAND + 0.04, lambda u: pose(landing, yaw=14, squash=1.0 - 0.5 * (1 - u) ** 2 * math.cos(u * 9.0)))
    kx, ky = CAVE_PUMPKIN
    mid, home = V(-2.9, 2.1, floor_z(-2.9, 2.1)), V(kx, ky, floor_z(kx, ky))
    a.hop(LAND + 0.04, 2.93, landing, mid, 1.2, yaw=(14, 20))
    a.hop(2.93, WAKE, mid, home, 1.3, yaw=(20, 370), land=0.12)
    a.rest(WAKE, home, yaw=10)
    # While the cave sleeps, so does it. And where nobody touches the lever, it goes
    # over and works it itself: a leap to knock it off, a doze, a start, a leap to knock
    # it back on, and home.
    toggles = S["cave_toggles"]
    slept = [(start, next((later for what_, _, later in toggles[i + 1 :] if what_ == "wake"), None)) for i, (what, clock, start) in enumerate(toggles) if what == "sleep"]
    off, on, side = session.at("demo_off"), session.at("demo_on"), S["lever_side"]
    for start, woken in slept:
        a.mask(start + 1.2, None if woken is None else woken - (0.6 if off is not None else -0.15), "asleep")
    if off is not None:

        def knock(u):
            """Straight up beside the lever, turning over once, to hit the handle at the top."""
            if u < 0.14:
                return pose(side, 10, squash=1.0 - 0.3 * math.sin(math.pi * 0.5 * u / 0.14))
            if u > 0.88:
                v = (u - 0.88) / 0.12
                return pose(side, 10, squash=1.0 - 0.36 * (1 - v) * math.cos(math.pi * 0.5 * v))
            v = (u - 0.14) / 0.74
            return pose(side + V(0.3 * math.sin(math.pi * v), 0, 4 * 1.5 * v * (1 - v)), 10, tilt=(0, 360 * smooth(v), 0), squash=1.0 + 0.16 * abs(math.cos(math.pi * v)))

        def between(p0, p1, height, yaw):
            return lambda u: pose(p0.lerp(p1, smooth(u)) + V(0, 0, 4 * height * u * (1 - u)), lerp(yaw[0], yaw[1], smooth(u)), squash=1.0 + 0.12 * abs(math.cos(math.pi * u)))

        a.act(off - 1.35, off - 0.5, between(home, side, 1.1, (10, 10)))
        a.act(off - 0.5, off + 0.5, knock)  # at the top of the leap, half way, the lever goes over
        a.act(off + 0.5, on - 0.5, lambda u: pose(side, 10, calm=True))
        a.act(on - 0.5, on + 0.5, knock)
        a.act(on + 0.5, on + 1.3, between(side, home, 1.1, (10, 10)))

    # Into the cart, through the tunnel, and out in front of the ore wall.
    seat = V(0.0, 0.0, 0.2)
    board = rail_at(0.0) + seat
    a.hop(3.57, BOARD, home, board, 1.5, yaw=(10, 180), flips=1.0, land=0.1)

    def ride(u):
        d = S["ride"](lerp(BOARD, ARRIVE, u))
        h = rail_heading(d)
        return pose(rail_at(d) + seat, yaw=math.degrees(math.atan2(h.x, -h.y)), tilt=(math.degrees(math.asin(max(min(-h.z, 1.0), -1.0))), 0, 0))

    a.move(BOARD, ARRIVE, ride)
    alight = rail_at(1e9) + seat
    d = wld.SPOTS["dungeon"]
    floor, gate = d["floor"], S["gate"]
    wall_spot, opening, sign_spot = P(V(19.8, 15.6, floor), V(21.1, 19.0, floor)), V(14.4, d["parting"][0] + 0.4, floor), V(21.9, gate.y + 1.55, floor)  # (tall: under the middle of the wall)
    facing = P(-75, -90)
    a.hop(ARRIVE, 4.11, alight, wall_spot, 1.5, yaw=(90, facing), flips=1.0)
    a.rest(4.11, wall_spot, yaw=facing)
    a.hop(4.24, 4.31, wall_spot, opening, 1.6, yaw=(facing, -180), land=0.1)  # well ahead of the camera, through the opening
    a.hop(4.31, 4.38, opening, sign_spot, 1.5, yaw=(-180, -100), flips=1.0)
    a.rest(4.38, sign_spot, yaw=-100)
    for go, back in S["home_trips"]:  # /home: gone in a blink, and there
        spot = S["home_spot"]
        a.act(go, go + 0.36, lambda u, spot=spot: pose(sign_spot if u < 0.5 else spot, -100, grow=abs(1.0 - 2.0 * u), calm=False))
        a.act(go + 0.36, back if back is not None else 1e9, lambda u, spot=spot: pose(spot, -100, calm=True))
        if back is not None:
            a.act(back, back + 0.55, lambda u, spot=spot: pose(spot.lerp(sign_spot, smooth(u)) + V(0, 0, 4 * 0.9 * u * (1 - u)), -100, squash=1.0 + 0.12 * abs(math.cos(math.pi * u))))
    before, inside = gate + V(-2.3, 0.2, 0), gate + V(0, 0, 0.6)
    into_portal = (before, inside)
    a.hop(4.77, 4.84, sign_spot, before, 0.9, yaw=(-100, 90))
    a.hop(4.84, PORTAL_LIT, before, inside, 1.1, yaw=(90, 90), flips=1.0, land=0.0)  # into the frame, and it lights
    a.appear(-1.0, PORTAL_LIT + 0.004)
    for s, face in ((0.0, "asleep"), (WAKE_UP, "awake"), (FALL[0], "whee"), (FALL[1], "awake"), (DIG0, "whee"), (LAND, "awake"), (BOARD - 0.04, "whee"), (ARRIVE + 0.05, "awake"), (4.84, "whee")):
        a.face(s, face)
    candle = anim.linear([(WAKE_UP, 0.0), (WAKE_UP + 0.01, 16.0), (FALL[1], 16.0), (FALL[1] + 0.02, 40.0), (STOMP, 40.0), (1.66, 70.0), (BREAK, 70.0), (BREAK + 0.02, 150.0), (WAKE, 150.0), (WAKE + 0.1, 34.0), (3.57, 34.0), (BOARD, 110.0), (ARRIVE, 110.0), (4.12, 30.0), (PORTAL_LIT, 30.0), (PORTAL_LIT + 0.01, 0.0)])
    anim.TL.add(a.candle.data, "energy", EVER, lambda s, dt: candle(s), "real")
    for start, woken in slept:  # its candle goes out while it sleeps
        anim.TL.add(a.candle.data, "energy", start, lambda s, dt: candle(s) * (1.0 - anim.clamp01((dt - 1.0) / 0.4)), "real")
        if woken is not None:
            anim.TL.add(a.candle.data, "energy", woken, lambda s, dt: candle(s) * anim.clamp01(dt / 0.3), "real")
    back = look.point("Kurbis_Back", (0, 0, 0), 0.0, (1.0, 0.6, 0.2), radius=0.3, shadow=False)
    back.parent = a.root
    back.location = (0.0, 2.2, 1.7)  # a glimmer on the tunnel behind the cart, for the camera that follows
    light_ramp(back, [(3.58, 0.0), (BOARD, 16.0), (ARRIVE, 16.0), (ARRIVE + 0.06, 0.0)])

    # The Nether: already through, hopping ahead along the bridge to the second frame.
    n = new_actor("Kurbis_Nether")
    ledge, row, gate = sets.NETHER["ledge"], sets.NETHER["portal_row"], sets.NETHER["gate_row"]  # (the Nether's own gate)
    spots = [N(0.0, row + 5.5, ledge), N(0.0, row + 10.0, ledge), N(0.2, row + 14.5, ledge), N(-0.2, gate - 4.2, ledge), N(*P((-2.4, gate - 1.0), (-2.2, gate - 0.4)), ledge)]
    n.appear(THROUGH - 0.02)
    n.rest(THROUGH - 0.02, spots[0], yaw=180)
    for i, (s0, s1) in enumerate(((5.34, 5.46), (5.46, 5.60), (5.60, 5.74), (5.74, 5.90))):  # always well ahead of the camera
        last = i == 3
        n.hop(s0, s1, spots[i], spots[i + 1], 1.2 if last else 1.5, yaw=(180, 372) if last else (180, 180), flips=0.0 if last else 1.0)
    n.rest(5.90, spots[-1], yaw=12)
    enter = S.get("nether_enter")
    if enter is not None:  # Enter: it leaps through the lit frame, and is gone
        inside = N(0.0, gate + 0.5, ledge + 0.7)
        n.act(enter + 0.15, enter + 0.8, lambda u: pose(spots[-1].lerp(inside, smooth(u)) + V(0, 0, 4 * 1.1 * u * (1 - u)), lerp(12, 180, smooth(u)), tilt=(360 * smooth(u), 0, 0), grow=1.0 - smooth((u - 0.8) / 0.2)))
        n.act(enter + 0.8, 1e9, lambda u: pose(inside, 180, grow=0.0))
    n.face(0.0, "whee").face(5.90, "awake")
    ramp(n.candle.data, "energy", [(THROUGH - 0.03, 0.0), (THROUGH - 0.02, 26.0)])
    S.update(a=a, n=n)
    # The places of its journey, for the page, which plays the same moves.
    S["cast"] = {
        "fall_from": fall_from, "dig_start": dig_start, "hole": hole, "landing": landing, "mid": mid, "home": home, "seat": seat, "board": board, "alight": alight,
        "wall_spot": wall_spot, "opening": opening, "sign_spot": sign_spot, "facing": facing, "before": into_portal[0], "inside": into_portal[1],
        "nether_spots": spots, "nether_inside": N(0.0, gate + 0.5, ledge + 0.7),
    }


# -- the camera ---------------------------------------------------------------
def camera_shots():
    a = S["a"]
    pumpkin = a.position  # where it is at a scroll position
    hero_aim = V(0.0, 60.0, P(33.3, 31.9))
    # The landing frame. The tall one stands further left and back, with a wider lens,
    # to hold the pond, both friends and the pumpkin in a narrow picture.
    landing_eye = P(V(-3.0, -4.22, 2.22), V(0.6, -4.6, 2.3))
    landing_aim = P(V(-3.1, 10.3, 3.98), V(-5.0, 5.5, 4.3))
    landing_lens = P(20.0, 20.0)
    off = landing_eye - V(-3.0, -4.22, 2.22)
    spot = LANDING_SPOT

    def side(s, x, z, drop=0.5):
        """Side on to the cut face, level with a point of it."""
        return (s, V(x, -12.5, z), V(x, 0.5, z - drop))

    # The dig: (scroll, eye, aim). Wide, the text stands to the right of the pumpkin and
    # the camera sinks with it. The tall frame cannot hold a band and the jog at once: it
    # follows the pumpkin to the left, and the first band steps aside as it goes.
    dig = P(
        [side(1.78, 3.0, -1.6, 0.4), side(1.97, 3.0, -3.7), side(2.15, 2.6, -3.9), side(2.33, 1.4, -9.5), side(2.47, 1.4, -12.0), (BREAK, V(0.8, -12.5, -17.8), V(-1.5, 0.5, -18.8))],
        [side(1.78, 1.6, -1.4, 0.4), side(1.97, 1.6, -2.7), side(2.045, 0.75, -3.0), side(2.12, -1.3, -3.8), side(2.2, -1.8, -5.6), side(2.33, -1.8, -9.3), side(2.47, -1.8, -12.4), (BREAK, V(-1.8, -12.5, -17.8), V(-2.2, 0.5, -18.8))],
    )
    dig_lens = P(22.0, 28.0)
    # The cave. The tall frame stands a little further out and more to the right, between
    # the pumpkin and the lever, and looks up at the headline.
    cave_eye = P(CAVE_EYE, V(0.9, -3.0, -23.4))
    cave_aim = P(CAVE_AIM, V(0.95, 6.0, -21.6))

    # A: sky, dive, surface, slice, dig, arrival, cave, cart, dungeon, into the portal.
    # The dive never rolls and never looks back: the camera stays south of the falling
    # pumpkin and above it, sinks with it through the cloud, and levels out low behind
    # the spot where it lands.
    eye = [
        (0.0, HERO_EYE), (0.22, V(0, 5.0, 31.02)), (FALL[0], V(0, 5.4, 31.0)),
        (0.50, V(0.0, 5.0, 30.6)), (0.62, V(-0.6, 2.6, 26.5)), (0.71, V(-1.2, 0.6, 19.0)), (0.78, V(-1.9, -1.6, 11.5)),
        (FALL[1], V(-2.5, -3.4, 4.6) + off), (0.94, V(-2.9, -4.0, 2.9) + off), (1.03, V(-3.15, -4.2, 2.3) + off),
        (PART["surface"], landing_eye), (PART["slice"], landing_eye + V(0.5, -0.18, -0.02)), (STOMP, landing_eye + V(0.55, -0.18, 0.03)),
        (1.60, V(-2.0, -7.5, 0.95)), (1.69, V(-0.2, -11.0, -0.9)), *[(s, at) for s, at, _ in dig],
        # down with it into the cave, then in through the open front
        (2.70, V(0.6, -12.0, -21.4)), (LAND, V(0.5, -10.5, -22.8)), (2.95, P(V(0.4, -7.0, -23.3), V(0.6, -7.6, -23.3))), (3.10, P(V(0.3, -4.0, -23.5), V(0.8, -4.9, -23.4))),
        (PART["cave"], cave_eye), (PART["cart"], cave_eye + V(0.5, 0.1, 0.06)),
    ]
    target = [
        (0.0, hero_aim), (0.225, hero_aim), (0.30, HERO_PUMPKIN + V(0, 0, 0.5)), (FALL[0], pumpkin(FALL[0]) + V(0, 0, 0.4)),
        (0.50, pumpkin(0.50)), (0.62, pumpkin(0.62)), (0.71, pumpkin(0.71)), (0.78, pumpkin(0.78)),
        (FALL[1], spot + V(0, 0.8, 0.7)), (0.94, spot.lerp(landing_aim, 0.35)), (1.03, landing_aim + V(0.1, -1.3, -0.3)),
        (PART["surface"], landing_aim), (PART["slice"], landing_aim + V(0.2, 0, -0.08)), (STOMP, V(-1.6, 3.0, 1.6)),
        (1.60, V(-1.0, 0.5, 0.0)), (1.69, V(0.8, 0.0, -1.5)), *[(s, aim) for s, _, aim in dig],
        (2.70, V(-2.6, 1.0, -22.4)), (LAND, V(-2.2, 2.0, -23.8)), (2.95, V(-0.6, 4.0, -23.1)), (3.10, V(0.5, 5.5, -22.7)),
        (PART["cave"], cave_aim), (PART["cart"], cave_aim + V(0.1, 0, 0)),
    ]
    lens = [(0.0, 28.0), (0.50, 28.0), (0.71, 22.0), (FALL[1], landing_lens), (STOMP, landing_lens), (1.78, dig_lens), (BREAK, dig_lens), (PART["cave"], 20.0), (ARRIVE - 0.05, 20.0), *P([(4.18, 18.0)], [(4.18, 23.0), (4.30, 23.0), (4.46, 18.0)]), (PORTAL_LIT, 18.0), (THROUGH, 16.0)]
    roll = [(0.0, 0.0), (1.90, 0.0), (1.99, -3.0), (2.10, 3.0), (2.22, 0.0)]

    # The cart: the camera chases it along its own, wider line (it cuts the turn), keeps
    # its distance, and looks a little way ahead of the cart.
    chase = anim.Track(
        [
            (0.0, V(0.5, 1.2, -23.3)), (4.0, V(0.5, 6.4, -23.5)), (8.0, V(0.5, 10.6, -23.6)), (11.3, V(0.5, 13.6, -23.6)),
            (14.3, V(1.0, 16.2, -23.6)), (17.8, V(3.2, 17.4, -23.9)), (21.4, V(5.6, 17.5, -26.0)), (25.0, V(8.8, 17.5, -28.2)),
            (S["rail"][1][-1], V(12.4, 17.6, -28.1)),
        ],
        ends="free",
    )
    for k in range(0, 21):
        s = lerp(BOARD, ARRIVE, k / 20)
        far = S["ride"](s)
        eye.append((s, chase(far)))
        if k < 20:
            target.append((s, rail_at(far + 1.4) + V(0, 0, 0.75)))
    roll += [(3.76, 0.0), (3.815, -11.0), (3.87, 0.0)]

    # The dungeon: in on the ore wall, along the room past the column to the spawner's
    # bay, then into the portal. The camera faces the east wall the whole way.
    d = wld.SPOTS["dungeon"]
    floor, wall, gate = d["floor"], d["x"][1] + 1.0, S["gate"]
    ore_mid = (d["ores"][0] + d["ores"][-1]) / 2 + 1.0
    # Wide, it stands at head height before the row. Tall, it rises with the wall, which
    # is twice as high, and stands a little further off from the spawner's bay.
    wall_aim = V(wall, ore_mid, floor + P(2.75, 5.1))
    bay_aim = V(wall, gate.y + 1.6, floor + P(2.85, 3.5))
    eye += [(4.18, P(V(13.6, ore_mid - 0.35, floor + 2.4), V(13.4, ore_mid - 0.15, floor + 4.5))), (4.30, P(V(13.9, ore_mid + 0.25, floor + 2.4), V(13.6, ore_mid + 0.1, floor + 4.9))),
            (4.46, P(V(16.3, gate.y + 1.4, floor + 2.1), V(15.2, gate.y + 1.5, floor + 3.0))), (PART["portal"], P(V(16.8, gate.y + 1.7, floor + 2.05), V(15.7, gate.y + 1.7, floor + 2.9))),
            (4.92, V(17.6, gate.y + 0.3, floor + 1.8)), (5.10, V(gate.x - 3.2, gate.y, floor + 1.55)), (THROUGH, V(gate.x - 0.2, gate.y, floor + 1.5))]
    target += [(ARRIVE, wall_aim), (4.18, wall_aim + V(0, -0.2, 0)), (4.30, wall_aim + V(0, 0.2, 0)), (4.46, bay_aim), (PART["portal"], bay_aim),
               (4.92, gate + V(0, 0, 1.5)), (5.10, gate + V(1.0, 0, 1.5)), (THROUGH, gate + V(2.0, 0, 1.5))]
    shot_a = Shot(eye, target, roll=roll, lens=lens)

    # B: the Nether. Out of the portal and forward along the bridge to the second frame.
    ledge, row, far_gate = sets.NETHER["ledge"], sets.NETHER["portal_row"], sets.NETHER["gate_row"]
    aim = (0.0, far_gate, ledge + P(3.0, 4.8))  # (the tall frame looks up at a taller block of type)
    back = P(8.1, 7.7)  # how far in front of the second frame it comes to rest
    path = (
        (THROUGH, (0.0, row + 0.9, ledge + 1.5), (0.0, row + 20, ledge + 2.6)),
        (5.42, (0.2, row + 4.6, ledge + 2.0), (0.0, far_gate, ledge + 2.8)),
        (5.75, (0.5, row + 10.5, ledge + 2.4), aim),
        (6.05, (0.45, far_gate - back - 0.4, ledge + 2.5), aim),
        (END, (0.4, far_gate - back, ledge + 2.5), aim),
    )
    shot_b = Shot([(s, N(*e)) for s, e, _ in path], [(s, N(*at)) for s, _, at in path], lens=[(THROUGH, 16.0), (5.75, 17.5), (END, 18.0)])
    return [shot_a, shot_b]


# -- the interface and the visitor ---------------------------------------------------
def set_hud(shots):
    """The interface (chat, a toast, the shortcut to the end) and, where this session's
    visitor does something, the pointer and what the world does in answer."""
    b, mats, session, a = S["b"], S["mats"], S["session"], S["a"]
    hud.shape(FRAME["tall"])
    panel = hud.Hud(S["cam"])
    S["hud"] = panel
    shot = sorted(shots, key=lambda sh: sh.s0)[0]

    # Chat: a friend asks for the address. It starts as the pumpkin lands and runs in real time.
    landed = session.when(1.12)
    for delay, line in ((0.5, "<Steve> what's the ip"), (1.9, "<You> steve.kurbis.host"), (3.3, "<Steve> that's it?"), (4.4, "<You> that's it")):
        panel.say(landed + delay, line, clock="real", stay=6.5, until=1.62)
    shortcut = panel.card("shortcut", hud.button(core.pixels("item/ender_pearl.png"), "Jump to portal"), at=(hud.WIDTH - 5, 6), anchor=(1.0, 0.0))
    ramp(shortcut, "color", [(5.45, (1.0, 1.0, 1.0, 1.0)), (5.6, (1.0, 1.0, 1.0, 0.0))])  # not needed once you are there
    back = core.pixels("gui/sprites/toast/advancement.png")
    for key, s, icon, body, until in (("grass", 1.0, "block/grass_block_side.png", "Touched grass", None), ("dirt", 2.3, "block/dirt.png", "Dirt enthusiast", None), ("cave", WAKE + 0.03, "block/diamond_ore.png", "Found the cave", 3.62), ("deeper", PORTAL_LIT + 0.02, "block/obsidian.png", "We need to go deeper", 5.5)):
        panel.announce(s, key, hud.toast(core.pixels(icon), "Advancement made!", body, back), stay=2.6, until=until)  # (each is gone before the next place's headline wants its corner)

    # The cave's comings and goings, in chat. Arriving, it is the pumpkin that joins first.
    folded = S["cave_folded"]
    for i, (what, clock, start) in enumerate(S["cave_toggles"]):
        if what == "wake":
            lines = ([(0.15, "Kurbis joined the game")] if i == 0 else []) + [(1.0, "Steve joined the game"), (1.35, "Alex joined the game")]
        else:
            lines = [(0.25, "Steve left the game"), (0.55, "Alex left the game")]
        for delay, line in lines:
            panel.say(start, line, hud.YELLOW, clock=clock, stay=6.0, delay=delay, until=3.62)
        if what == "sleep":
            panel.say(start, "World saved.", hud.GREY, clock=clock, stay=6.0, delay=folded + 0.15, until=3.62)

    # The visitor. Each thing they do is a named moment of the session; a session
    # without it simply has no pointer there and nothing answers.
    def seen(point, s):
        return hud.project(shot, s, Vector(point))[0]

    # Tooltips and chat the sets asked for (the dungeon's ores and what the spawner is asked).
    for tip in S.get("tips", []):
        spot = seen(tip["at"], tip["s"])
        right = spot[0] > hud.WIDTH * 0.62
        if tip["under"]:  # centred under the thing
            card = panel.card(f"tip_{tip['name']}", hud.tooltip(tip["title"], tip["lines"]), at=spot, anchor=(0.5, 1.0), shown=False, inside=4)
        else:
            card = panel.card(f"tip_{tip['name']}", hud.tooltip(tip["title"], tip["lines"]), at=(spot[0] + (20 if right else -20), spot[1]), anchor=(1.0 if right else 0.0, 0.0), shown=False, inside=4)
        anim.TL.add(card, "color", EVER, lambda s, dt, showing=tip["showing"]: (1.0, 1.0, 1.0, showing(s, anim.now())), "real")
    for moment, line, colour in S.get("says", []):
        panel.say(moment, line, colour, clock="real", stay=5.0, until=SPAWNER_VIEW[1] + 0.02)

    keys, clicks = [], []

    def go(moment, where, lead=0.7):
        """Be at `where` at `moment`, having set out a little before."""
        if keys and moment - lead > keys[-1][0] + 0.05:
            keys.append((moment - lead, keys[-1][1]))
        keys.append((moment, where))

    # Sky: hover the sleeper (it opens an eye and follows), click it (it jumps), click a cloud (rain).
    hover, poke, cloud = session.at("sky_hover"), session.at("sky_click"), session.at("cloud_click")
    if hover is not None:
        on_pumpkin = seen(HERO_PUMPKIN + V(0.1, 0, 0.45), 0.12)
        keys.append((hover - 0.8, (on_pumpkin[0] + 150, on_pumpkin[1] - 90)))
        go(hover, on_pumpkin)
        go(hover + 0.9, (on_pumpkin[0] - 34, on_pumpkin[1] + 14), 0.5)
        go(poke, (on_pumpkin[0] + 6, on_pumpkin[1] + 2), 0.6)
        clicks.append(poke)
        a.mask(hover, poke + 1.4, "peek")
        centre = on_pumpkin[0]
        a.glance(hover, poke, lambda tau: 0.45 * (panel.pointer[1](tau)[0] - centre))
        a.jump(poke, 0.42)  # (a small one: the title is right above its head)
        if cloud is not None:
            high = V(-16, 70, 52)
            go(cloud, seen(high + V(0, 0, 1.4), 0.12), 0.9)
            clicks.append(cloud)
            rain = scenery.streaks("Rain", scenery.fading_material("RainLight", (0.7, 0.85, 1.0), strength=1.4), "FX", lo=(-27, 66, 30), hi=(-5, 74, 52), count=150, size=(0.12, 1.5), seed=7)
            life.drift(rain, (0.0, 0.0, -26.0), (-27, 66, 30), (-5, 74, 52), sway=0.0)
            rain.color = (1.0, 1.0, 1.0, 0.75)
            hidden(rain)
            live(rain, "scale", lambda dt: (1.0, 1.0, 1.0) if dt < 3.2 else (0.0, 0.0, 0.0), start=cloud + 0.1)
            go(cloud + 1.4, (on_pumpkin[0] + 120, on_pumpkin[1] - 60), 0.9)

    # Surface: click the pond (a fish leaps), a friend (she waves), the tree (petals), the pumpkin (it jumps).
    pond, friend, tree, pumpkin = (session.at(key) for key in ("pond_click", "friend_click", "tree_click", "pumpkin_click"))
    if pond is not None:
        spot = ((-7.4, 2.9), (-6.7, 3.5))
        keys.append((pond - 1.0, (hud.WIDTH * 0.62, 40.0)))
        go(pond, seen((*spot[0], POND_LEVEL), 1.30))
        clicks.append(pond)
        rings = [scenery.ring(f"Clicked_Ring_{i}", bpy.data.materials["Ripple"]) for i in range(2)]
        S["fishers"].append(life.Jumper(boxmodel.fish("Clicked_Fish"), rings, (spot,), POND_LEVEL, height=SWELL, once=pond + 0.12))
    if friend is not None:
        alex = S["friends"]["Alex"]
        go(friend, seen(Vector(alex.root.location) + V(0, 0, 1.5), 1.30))
        clicks.append(friend)
        arm = alex.parts["arm_l"]
        rest_arm = tuple(arm.rotation_euler)

        def wave(dt):
            up = smooth(dt / 0.25) * (1.0 - smooth((dt - 1.5) / 0.3))
            return (rest_arm[0] * (1.0 - up), math.radians(-150.0 + 22.0 * math.sin(dt * 14.0)) * up, rest_arm[2])

        live(arm, "rotation_euler", wave, start=friend + 0.1)
    if tree is not None:
        go(tree, seen((4.4, 5.6, 7.0), 1.30))
        clicks.append(tree)
        lo, hi = (1.5, 2.5, 1.2), (10.5, 11.0, 9.5)
        burst = scenery.leaves("Petal_Burst", mats["petal"], "FX", lo=lo, hi=hi, count=170, size=(0.05, 0.11), seed=31)
        life.drift(burst, (0.5, -0.25, -2.4), lo, hi, sway=0.3)
        hidden(burst)
        live(burst, "scale", lambda dt: (min(dt / 0.15, 1.0) * (1.0 - smooth((dt - 2.6) / 0.6)),) * 3, start=tree + 0.1)
    if pumpkin is not None:
        go(pumpkin, seen(LANDING_SPOT + V(0, -0.5, 0.5), 1.30))
        clicks.append(pumpkin)
        a.jump(pumpkin + 0.05)
        go(pumpkin + 1.3, (hud.WIDTH * 0.69, 60.0), 0.9)
    # Dig: things buried in the cut face say what they are when the pointer finds them.
    begun = False
    for key, title, lines in (("fossil", "Fossil", ["Older than your last world."]), ("chest", "Chest", ["Someone's coal. Finders keepers."]), ("sign", "Old sign", ["It says: Back in 5 minutes."]), ("sock", "Lost sock", ["The other one is in the Nether."])):
        found = session.at(f"{key}_hover")
        if found is None:
            continue
        spot = seen(S["toys"][key], 2.25)
        if not begun:
            keys.append((found - 1.0, (hud.WIDTH * 0.15, hud.HEIGHT * 0.78)))
            begun = True
        go(found, spot)
        right = spot[0] > hud.WIDTH * 0.62
        tip = panel.card(f"tip_{key}", hud.tooltip(title, lines), at=(spot[0] + (-10 if right else 12), spot[1] + 8), anchor=(1.0 if right else 0.0, 0.0), shown=False, inside=4)
        live(tip, "color", lambda dt: (1.0, 1.0, 1.0, anim.clamp01(dt / 0.12) * (1.0 - anim.clamp01((dt - 1.5) / 0.15))), start=found + 0.15)

    # Cave: hover the lever (a tooltip says what it does), pull it, and pull it back.
    hover, off, on = session.at("lever_hover"), session.at("lever_off"), session.at("lever_on")
    if off is not None:
        lx, ly = LEVER
        on_lever = seen((lx, ly, floor_z(lx, ly) + 1.7), 3.40)
        keys.append((hover - 1.0, (hud.WIDTH * 0.31, 60.0)))
        go(hover, on_lever)
        go(off, (on_lever[0] + 2, on_lever[1] - 2), 0.3)
        clicks.append(off)
        if on is not None:
            go(on, (on_lever[0] + 1, on_lever[1] + 1), 0.4)
            clicks.append(on)
            go(on + 1.6, (on_lever[0] - 120, on_lever[1] - 70), 1.0)
        for key, lines, t_in, t_out in (("awake", ["Pull it: everyone leaves,", "and the server sleeps."], hover + 0.25, off + 0.1), ("asleep", ["Pull it: someone joins,", "and the server wakes."], off + 2.2, (on or off + 6.0) + 0.1)):
            tip = panel.card(f"tip_lever_{key}", hud.tooltip("Lever", lines), at=P((on_lever[0] - 16, on_lever[1] + 26), (on_lever[0] + 24, on_lever[1] + 34)), anchor=(1.0, 0.0), shown=False, inside=4)  # between the headline and the block
            live(tip, "color", lambda dt, stay=t_out - t_in: (1.0, 1.0, 1.0, anim.clamp01(dt / 0.12) * (1.0 - anim.clamp01((dt - stay) / 0.15))), start=t_in)
    # Dungeon: point at a tile of the ore wall (it says its piece), and click one of the
    # spawner's signs (that one is asked).
    for moment, k in S["tile_pointed"]:
        go(moment, seen(S["tile_spots"][k] + V(0, -0.3, -0.2), 4.22), 0.6)
    for moment, k in S["sign_picked"]:
        go(moment, seen(S["sign_spots"][k] + V(0, -0.6, -0.1), 4.62), 0.7)
        clicks.append(moment)
    # Nether: click the name field, type, and press Enter once the name is free.
    focus, enter = session.at("name_focus"), session.at("name_enter")
    if focus is not None:
        shot_b = sorted(shots, key=lambda sh: sh.s0)[-1]
        on_field = hud.project(shot_b, END, S["nether_click"])[0]
        keys.append((focus - 1.0, (on_field[0] + 130, on_field[1] - 120)))
        go(focus, on_field)
        clicks.append(focus)
        go(focus + 1.2, (on_field[0] + 40, on_field[1] - 46), 0.8)
        if enter is not None:
            key = panel.card("key_enter", hud.button(None, "Enter"), at=(hud.WIDTH / 2, 22), anchor=(0.5, 0.0), shown=False)
            live(key, "color", lambda dt, stay=enter - S["nether_free"] - 0.5: (1.0, 1.0, 1.0, anim.clamp01(dt / 0.15) * (1.0 - anim.clamp01((dt - stay) / 0.15))), start=S["nether_free"] + 0.5)
            live(key, "scale", lambda dt: (1.0 - 0.14 * max(1.0 - abs(dt - 0.06) / 0.08, 0.0),) * 2 + (1.0,), start=enter - 0.02)
    panel.point(keys, clicks)


def grade():
    """Everything that changes with the place rather than with an object: the sun,
    the sky's light, the covers, the lens wobble."""
    scene = S["scene"]
    ramp(S["sun"].data, "energy", [(2.3, 7.0), (2.55, 0.0)])
    switch(S["sun"], "hide_render", [(0.0, False), (2.56, True)])
    sky = scene.world.node_tree.nodes["Background"].inputs["Strength"]
    ramp(sky, "default_value", [(1.5, 1.0), (1.78, 0.55), (2.45, 0.55), (2.75, 0.1), (THROUGH - 0.01, 0.1), (THROUGH, 0.05)])

    one, zero = (1.0, 1.0, 1.0), (0.0, 0.0, 0.0)
    for cover, points in (
        (S["white"], [(WHITE[0], 0.0), (WHITE[1], 1.0), (WHITE[2], 1.0), (WHITE[3], 0.0)]),
        (S["swirl"], [(5.13, 0.0), (5.22, 1.0), (5.30, 1.0), (5.46, 0.0)]),
    ):
        ramp(cover, "color", [(s, (1.0, 1.0, 1.0, alpha)) for s, alpha in points])
        switch(cover, "scale", [(0.0, zero), (points[0][0] - 0.01, one), (points[-1][0] + 0.01, zero)])
    wobble = scene.compositing_node_group.nodes.get("Wobble") if scene.compositing_node_group else None
    if wobble:
        ramp(wobble.inputs["Distortion"], "default_value", [(4.95, 0.0), (5.14, 0.09), (5.28, -0.07), (5.40, 0.045), (5.52, 0.0)])
        ramp(wobble.inputs["Dispersion"], "default_value", [(4.95, 0.0), (5.2, 0.06), (5.52, 0.0)])


# -- build, preview, save -------------------------------------------------------
def preview_settings(scale=0.5, samples=8, blur=True):
    scene = S["scene"]
    look.render_settings(scene, int(P(1920, 1080) * scale), int(P(1080, 1920) * scale), samples)
    scene.render.use_motion_blur = blur
    scene.render.motion_blur_shutter = 0.5
    scene.eevee.motion_blur_steps = 1
    scene.eevee.volumetric_samples = 48
    scene.eevee.volumetric_tile_size = "8"
    scene.eevee.shadow_pool_size = "1024"  # the cave has many small lights
    scene.frame_start, scene.frame_end = 1, S["session"].frames
    scene.render.fps = FPS


def frame_at(s):
    """The frame on which this session's scroll first gets to s."""
    return anim.frame(S["session"].when(s))


def still(s, name=None, scale=0.5, samples=8, blur=False):
    scene = S["scene"]
    preview_settings(scale, samples, blur)
    scene.frame_set(frame_at(s))
    path = core.RENDERS / "film" / f"{name or f's{s:04.2f}'}.png"
    path.parent.mkdir(parents=True, exist_ok=True)
    scene.render.filepath = str(path)
    bpy.ops.render.render(write_still=True)
    return str(path)


def save():
    path = core.BLEND / f"film_{S['session'].name}{P('', '_tall')}.blend"
    bpy.ops.wm.save_as_mainfile(filepath=str(path))
    return str(path)


def check(contrast=0):
    """The legibility audit (audit.py) at three moments of every rest of this session.
    Returns its findings as short lines; an empty problem list is the goal. Contrast
    takes two renders a moment and is left out unless asked for (percent of the preview
    frame to measure it on); `audit_run.py` does the whole audit headless."""
    for name in [k for k in sys.modules if k == "audit"]:
        del sys.modules[name]
    import audit

    session, stops = S["session"], []
    for t0, t1, s in session.spans:
        if t1 - t0 < 1.5:
            continue
        part = [name for name, start in sorted(PART.items(), key=lambda kv: kv[1]) if start <= s + 1e-6][-1]
        stops += [(f"{part} at {s:.2f} +{round(dt, 1)}s", anim.frame(t0 + dt)) for dt in (0.9, (t1 - t0) / 2, t1 - t0 - 0.3)]  # (two rests can share a part)
    lines = audit.summary(audit.run(S["scene"], stops, contrast_scale=contrast))
    S["scene"].frame_set(1)
    return lines


def budget(shots):
    """The journey's cost in scroll, and any stretch where scrolling moves nothing."""
    moves = anim.survey(shots, END)
    still_at = [s for s, blocks, degrees in moves if blocks < 0.15 and degrees < 0.8]
    slowest = min(moves, key=lambda m: m[1] + m[2] / 6)
    return {"screens": END, "within_budget": END <= 6.5, "camera_still_at": still_at, "slowest": slowest}


def stage(session="scroll", aspect="wide"):
    """Every set, light and actor standing where it starts, with the timeline written but
    nothing baked. Returns the camera's shots. (The web exporter stops here.)"""
    FRAME["tall"] = aspect == "tall"
    build_base()
    S["session"] = sessions()[session]
    set_valley()
    set_surface()
    set_dig()
    set_cave()
    set_ride()
    set_dungeon()
    set_nether()
    cast()
    shots = camera_shots()
    grade()
    set_hud(shots)
    S["shots"] = shots
    return shots


def build(session="scroll", aspect="wide"):
    t0 = time.time()
    shots = stage(session, aspect)
    t1 = time.time()
    visit = S["session"]
    keys = anim.TL.bake(visit)
    for actor in (S["a"], S["n"], *S["fishers"]):
        actor.bake(visit)
    lens = anim.bake_camera(S["cam"], shots, visit)
    S["hud"].bake(visit, lens)
    preview_settings()
    S["scene"].frame_set(1)
    S["shots"] = shots
    scroll = [round(visit.scroll(f / FPS), 4) for f in range(visit.frames)]
    rests = [[anim.frame(t0_), anim.frame(t1_)] for t0_, t1_, _ in visit.spans[1:-1]]
    (core.EXPORT / f"session_{visit.name}{P('', '_tall')}.json").write_text(json.dumps({"fps": FPS, "pace": PACE, "parts": PART, "scroll": scroll, "rests": rests}))  # for labels and, later, the page
    return {
        "session": visit.name, "seconds": round(visit.duration, 2), "frames": visit.frames, "build_s": round(t1 - t0, 1), "bake_s": round(time.time() - t1, 1),
        "objects": len(bpy.data.objects), "curves": len(anim.TL.channels), "keys": keys, "through": S["through"], "budget": budget(shots),
    }


if __name__ == "__main__":
    result = build()
