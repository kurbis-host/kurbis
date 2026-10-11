"""Gate 0: look-dev stills for the landing v2 short.

Run inside Blender (Blender MCP, or `blender -b -P gate0.py`). `build()` makes
the whole scene from scratch; `render(shot)` writes a still to ../renders.
Each shot keeps its own objects in a `Shot_*` collection so only one renders.
"""

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
from mathutils import Matrix, Vector  # noqa: E402

from kurbis import blocks as blk  # noqa: E402
from kurbis import boxmodel, core, font, look, mascot, scenery, voxel  # noqa: E402
from kurbis import world as wld  # noqa: E402

# The sun sits on the horizon straight down the valley. The lamp is a little
# higher than the disc so the forest keeps some direct light (a standard cheat).
SUN = Vector((-0.035, 1.0, 0.062)).normalized()
SUN_LIGHT = Vector((-0.035, 1.0, 0.135)).normalized()

S = {"shots": {}}  # scene state shared between build() and the shots


def undug(*names):
    """Collections for the shafts that have not been dug yet in a scene."""
    return [S["plugs"][n] for n in names]
SCENES = ("hero", "landing", "dig", "cave_awake", "cave_asleep", "vault", "portal")  # each also as <scene>_portrait


def rad(*deg):
    return tuple(math.radians(d) for d in deg)


def build():
    t0 = time.time()
    scene = core.reset_scene()
    b = blk.build_registry()
    mascot.register(b)
    b.atlas.bake()
    mats = {
        "voxel": blk.voxel_material("Voxel", b.atlas, emit_strength=4.0),
        "type": blk.voxel_material("Type", b.atlas, emit_strength=0.0, self_illum=0.55, roughness=0.7),
        "type_dim": blk.voxel_material("TypeDim", b.atlas, emit_strength=0.0, self_illum=0.2, roughness=0.7),
        "mini": blk.voxel_material("VoxelMini", b.atlas, emit_strength=5.0, self_illum=0.34),
        "glass": blk.flat_material("SnapshotGlass", (1.0, 0.86, 0.5), alpha=0.06, emit=0.25, roughness=0.05),
        "glow_edge": blk.flat_material("SnapshotEdge", (1.0, 0.78, 0.3), emit=5.0),
        "mascot": blk.voxel_material("Kurbis", b.atlas, emit_strength=3.2),
        "water": blk.flat_material("Water", (0.03, 0.22, 0.80), alpha=0.74, roughness=0.3),
        "cloud": blk.flat_material("Cloud", (1.0, 0.80, 0.74), alpha=0.8, emit=0.16, emit_rgb=(1.0, 0.50, 0.46), roughness=1.0, dither=True),
        "cloud_near": blk.flat_material("CloudNear", (0.90, 0.80, 0.82), emit=0.05, emit_rgb=(1.0, 0.6, 0.6), roughness=1.0),
        "leaf": blk.flat_material("Leaf", (1.0, 0.42, 0.08), emit=0.5, roughness=0.8),
        "petal": blk.flat_material("Petal", (1.0, 0.62, 0.78), emit=0.45, roughness=0.8),
        "line": blk.flat_material("Line", (0.92, 0.92, 0.95), emit=0.3),
        "bobber": blk.flat_material("Bobber", (0.9, 0.12, 0.1), emit=0.25),
        "streak": blk.flat_material("Streak", (1.0, 0.9, 0.7), alpha=0.35, emit=1.2),
        "spark": blk.flat_material("PortalSpark", (0.72, 0.3, 1.0), emit=3.0),
    }
    w = wld.World(b).generate()
    # Type on the cut face needs a flat wall behind it: undo the relief there.
    for layout in DIG_LAYOUTS.values():
        for lines, _palette, cell, (x, z), _lead in layout["type"]:
            rows, cols = font.text_mask(lines, leading=_lead).shape
            w.restore_front((x - cols * cell / 2 - 0.6, x + cols * cell / 2 + 0.6), (z - 0.6, z + rows * cell + 0.6))
    for layout in VAULT_LAYOUTS.values():
        lines, at, cell, on_face = layout["caption"]
        if on_face:
            rows, cols = font.text_mask(lines, leading=3).shape
            w.restore_front((at[0] - cols * cell / 2 - 0.6, at[0] + cols * cell / 2 + 0.6), (at[2] - 0.6, at[2] + rows * cell + 0.6))
    closed = voxel.ALL_CLOSED_BUT_FRONT_AND_TOP
    voxel.grid_object("Terrain", w.grid, b, mats["voxel"], "World", origin=w.origin, closed=closed)
    voxel.grid_object("Water", w.grid, b, mats["water"], "World", group="water", origin=w.origin, closed=closed)
    boxmodel.cross_sprites("GrassTufts", "block/short_grass.png", w.sprites["grass"], col="World", tint=(0.62, 0.86, 0.34))
    boxmodel.cross_sprites("SugarCane", "block/sugar_cane.png", w.sprites["cane"], col="World")
    boxmodel.flat_sprites("LilyPads", "block/lily_pad.png", w.sprites["lily"], col="World", tint=(0.22, 0.58, 0.16))
    boxmodel.flat_sprites("Petals", "block/pink_petals.png", w.sprites["petals"], col="World")
    for kind, flower in enumerate(("poppy", "dandelion", "cornflower", "oxeye_daisy")):
        spots = [(x, y, z) for x, y, z, k in w.sprites["flowers"] if k == kind]
        if spots:
            boxmodel.cross_sprites(f"Flowers_{flower}", f"block/{flower}.png", spots, col="World")
    # The ground the pumpkin has not dug yet: one plug per shaft, shown until that dig.
    S["plugs"] = {}
    for name, (origin, grid) in w.plugs.items():
        S["plugs"][name] = scenery.shot_collection(scene, f"Plug_{name}")
        voxel.grid_object(f"Plug_{name}", grid, b, mats["voxel"], S["plugs"][name], origin=origin)

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
    S["sun"] = look.sun("Sun", SUN_LIGHT, energy=7.0, color=(1.0, 0.62, 0.34), angle_deg=1.6)
    S["haze"] = look.fog("Haze", (-300, -120, -4), (300, 420, 72), density=0.0014, fade=(0, 54), color=(1.0, 0.72, 0.5), anisotropy=0.74)
    S["comp"] = look.compositor(scene, bloom=0.38, threshold=1.1, size=0.55, vignette=0.32, saturation=1.05)
    S.update(scene=scene, b=b, mats=mats, w=w)
    return {"seconds": round(time.time() - t0, 2), "tiles": len(b.atlas.index), "blocks": len(b.names) - 1}


# -- shots -----------------------------------------------------------------
HERO_LAYOUTS = {
    # Same scene, two framings. The web page picks one by aspect ratio.
    "hero": {
        "size": (1920, 1080),
        "pitch": 3.0,
        "title": (["YOUR WORLD", "IS WAITING BELOW"], 40.0),
        "caption": ["FREE MINECRAFT SERVERS", "FOR YOU AND YOUR FRIENDS"],
        "perch": ((12, 8), 3, 28.25),  # cloud footprint in cells, riser layers, base height
        "pumpkin": 2.0,
        "cue": ((0.5, 15.0, 28.02), 0.022),
        "zzz": ((1.75, 1.3), (2.5, 1.6), (3.35, 1.95)),  # kept under the title
        "clouds": (),
    },
    "hero_portrait": {
        "size": (1080, 1920),
        "pitch": 0.0,
        "title": (["YOUR WORLD", "IS WAITING", "BELOW"], 42.9),
        "caption": ["FREE MINECRAFT", "SERVERS FOR YOU", "AND YOUR FRIENDS"],
        "perch": ((9, 8), 4, 27.9),
        "pumpkin": 2.2,
        "cue": ((0.5, 15.0, 25.7), 0.03),
        "zzz": ((1.7, 1.9), (2.4, 2.3), (3.25, 2.75)),
        "clouds": (("top_left", (-14, 64, 68.0), (5, 3), (3.0, 3.0, 1.2), 6), ("top_right", (17, 104, 92.0), (6, 3), (3.5, 3.5, 1.4), 4)),
    },
}
CAPTION_CELL = 0.042


def shot_hero():
    """Beat 1. The pumpkin dozes on a cloud above the autumn valley, under the title,
    with the sun going down straight behind it. Built once per framing."""
    import numpy as np

    scene, b, mats = S["scene"], S["b"], S["mats"]
    shared = scenery.shot_collection(scene, "Shot_HeroShared")
    eye = Vector((0.0, 4.0, 31.0))

    clouds = (
        ("near_left", (-33, 44, 45.0), (7, 3), (3.0, 3.0, 1.2), 6),
        ("near_right", (35, 52, 47.5), (8, 3), (3.0, 3.0, 1.2), 4),
        ("mid_right", (66, 118, 40.0), (7, 3), (4.0, 4.0, 1.6), 8),
        ("mid_left", (-70, 126, 42.0), (7, 3), (4.0, 4.0, 1.6), 5),
    )
    for name, center, cells, cell, seed in clouds:
        scenery.cloud(f"Hero_Cloud_{name}", b, mats["cloud"], center, cells, cell, seed, shared, profile=(1, 0))
    scenery.leaves("Hero_Leaves", mats["leaf"], shared, lo=(-16, 13, 18), hi=(16, 46, 50), count=110, size=(0.05, 0.12), seed=5)
    # Everything is backlit, so a second warm lamp from the camera side keeps the
    # forest saturated and the pumpkin's face readable (trailer lighting, not physics).
    look.sun("Hero_Key", (-0.5, -0.62, 0.6), energy=2.1, color=(1.0, 0.60, 0.40), angle_deg=8.0, volume=0.0, col=shared)

    for key, cfg in HERO_LAYOUTS.items():
        tag = "".join(part.title() for part in key.split("_"))
        col = scenery.shot_collection(scene, f"Shot_{tag}")

        # Title: floats in the dusk above the valley, square to the camera.
        lines, base = cfg["title"]
        title = font.text_object(f"{tag}_Title", lines, b, mats["type"], col, palette="gold", cell=0.5, depth=6, leading=1)
        title.location = (0, 59.5, base)

        # The pumpkin's perch: a stepped cloud. It sleeps on top, and the caption
        # is embossed on the riser, standing on the ledge below.
        (nx, ny), riser, base_z = cfg["perch"]
        mask = np.ones((nx, ny), bool)
        mask[:1, 1:] = mask[-1:, 2:] = False  # the front edge stays straight for the caption
        mask[:3, -2:] = mask[-3:, -1:] = False
        step = (0.7, 0.7, 0.35)
        perch = Vector((0.5, 18.6, base_z))
        scenery.cloud(f"{tag}_Perch", b, mats["cloud_near"], perch, (nx, ny), step, col=col, profile=(1, 0) + (1,) * riser, mask=mask)
        ledge = perch.z + step[2] * 2
        top = ledge + step[2] * riser

        size = cfg["pumpkin"]
        kurbis = mascot.build(f"{tag}_Kurbis", b, mats["mascot"], state="asleep", col=col, size=size)
        kurbis.location = (perch.x + 0.35, perch.y + 0.2, top)
        kurbis.rotation_euler = rad(0, -7, -5)
        k = size / 2.0
        for i, ((dx, dz), cell) in enumerate(zip(cfg["zzz"], (0.034, 0.046, 0.062))):
            z = font.text_object(f"{tag}_Zzz_{i}", ["Z"], b, mats["type"], col, palette="plum", cell=cell * k, depth=2)
            z.location = (kurbis.location.x + dx * k, kurbis.location.y - 0.4, kurbis.location.z + dz * k)
            z.rotation_euler = rad(0, 8 + i * 9, -6)

        depth = 3
        caption = font.text_object(f"{tag}_Caption", cfg["caption"], b, mats["type"], col, palette="plum", cell=CAPTION_CELL, depth=depth, leading=3)
        rows = len(cfg["caption"]) * 8 + (len(cfg["caption"]) - 1) * 3
        riser_face = perch.y - step[1] * (ny / 2 - 1)
        caption.location = (perch.x, riser_face - depth * CAPTION_CELL, ledge + (step[2] * riser - rows * CAPTION_CELL) / 2)

        at, cell = cfg["cue"]
        cue = font.text_object(f"{tag}_Cue", ["DIG DOWN \u2193"], b, mats["type"], col, palette="cream", cell=cell, depth=3)
        cue.location = at

        for name, center, cells, cell, seed in cfg["clouds"]:
            scenery.cloud(f"{tag}_Cloud_{name}", b, mats["cloud"], center, cells, cell, seed, col, profile=(1, 0))

        look.area(f"{tag}_Fill", eye + Vector((-3.5, 0, 2.0)), kurbis.location + Vector((0, 0, 0.8)), energy=500, color=(1.0, 0.76, 0.68), size=6, col=col)
        aim_at = eye + Vector((0, 100, 100 * math.tan(math.radians(cfg["pitch"]))))
        cam = look.camera(f"Cam_{tag}", eye, aim_at, lens=28, col=col)
        S["shots"][key] = {"collections": [shared, col, *undug("shaft", "vault", "nether")], "camera": cam, "sun": 7.0, "world": 1.0, "exposure": 0.0, "size": cfg["size"]}


def floor_z(x, y, start=-21):
    """Height of the cave floor under a point: first solid cell below `start`."""
    w, z = S["w"], start
    while z > w.z0 and w.get(int(math.floor(x)), int(math.floor(y)), z - 1) == 0:
        z -= 1
    return float(z)


def on_floor(x, y):
    return (x, y, floor_z(x, y))


CAVE_LAYOUTS = {
    # One cave, two framings, each used for both the awake and the asleep still.
    # The camera sits in the cave mouth, low, so the cast is large.
    "": {
        "size": (1920, 1080),
        "eye": (0.2, -2.2, -23.6),
        "aim": (0.8, 6.0, -22.3),
        "lens": 20,
        "title": ((2.2, 6.5, -20.6), 0.085, (-8, 0, 0)),
        "caption": ((2.2, 6.3, -21.35), 0.05),
        "awake": (["IT ONLY GLOWS", "WHEN SOMEONE'S HOME"], ["SOMEONE JOINS. THE LIGHTS COME ON."]),
        "asleep": (["NOBODY HOME?", "EVERY BLOCK IS SAVED."], ["IT SLEEPS AS ONE SMALL SNAPSHOT."]),
        "tag": 0.034,
        "kurbis": (-0.4, 3.6),
        "steve": ((-2.5, 4.2), -66, "mine"),
        "alex": (2.7, 3.6),
        "snapshot": (1.2, 3.0),
    },
    "_portrait": {
        "size": (1080, 1920),
        "eye": (-0.4, -2.6, -23.5),
        "aim": (-0.2, 6.0, -20.8),
        "lens": 24,
        "title": ((-0.1, 6.5, -20.2), 0.07, (-10, 0, 0)),
        "caption": ((-0.1, 6.3, -20.95), 0.045),
        "awake": (["IT ONLY GLOWS", "WHEN SOMEONE'S", "HOME"], ["SOMEONE JOINS.", "THE LIGHTS COME ON."]),
        "asleep": (["NOBODY HOME?", "EVERY BLOCK", "IS SAVED."], ["IT SLEEPS AS ONE", "SMALL SNAPSHOT."]),
        "tag": 0.034,
        "kurbis": (-0.4, 3.2),
        "steve": ((-2.3, 5.3), -24, "cheer"),
        "alex": (2.0, 5.5),
        "snapshot": (0.8, 3.0),
    },
}
STEVE_POSES = {
    "mine": dict(body=(12, 0, 0), head=(2, 0, 50), arm_r=(-150, 0, 0), hand_r=(-28, 0, 0), arm_l=(34, 0, 0), hand_l=(-18, 0, 0), leg_r=(-30, 0, 0), foot_r=(26, 0, 0), leg_l=(22, 0, 0), foot_l=(8, 0, 0)),
    "cheer": dict(body=(-4, 0, 0), head=(-8, 0, 10), arm_r=(-168, 0, -10), hand_r=(-14, 0, 0), arm_l=(-12, -14, 0), hand_l=(-24, 0, 0), leg_r=(-6, 0, 0), foot_r=(4, 0, 0), leg_l=(8, 0, 0), foot_l=(2, 0, 0)),
}
TORCHES = ((-8.6, 1.6), (7.6, 7.6), (-0.5, 9.6), (12.6, 1.4), (4.4, 9.2), (-4.5, 5.5))
LANTERNS = ((3.6, 1.2), (-2.4, 1.2))


def cave_set():
    """Shared by every cave still: glow-berry vines and the haze that shows the light."""
    scene, w = S["scene"], S["w"]
    col = scenery.shot_collection(scene, "Shot_Cave")
    by_length = {}
    for x, y, z, length in w.sprites["vines"]:
        by_length.setdefault(length, []).append((x, y, z))
    for length, spots in by_length.items():
        boxmodel.cross_sprites(f"Cave_Vines_{length}", "block/cave_vines_lit.png", spots, col=col, emit=1.4, emit_rel="block/cave_vines_lit_e.png", rows=length)
    look.fog("Cave_Haze", (-16, -9, -29), (22, 15, -13), density=0.007, color=(1.0, 0.9, 0.8), anisotropy=0.4, col=col)
    # Thicker air right under the shaft, so the light coming down it reads as a beam.
    shaft = wld.SPOTS["shaft"]
    look.fog("Cave_Beam", (shaft["x"][0] - 0.3, -0.3, -25.2), (shaft["x"][1] + 1.3, 2.3, -9.0), density=0.07, color=(1.0, 0.92, 0.8), anisotropy=0.2, col=col)
    return col


def cave_type(tag, cfg, state, material, col):
    """Headline and caption floating in the cave, tilted back toward the low camera."""
    b = S["b"]
    lines, caption = cfg[state]
    at, cell, tilt = cfg["title"]
    title = font.text_object(f"{tag}_Title", lines, b, material, col, palette="gold", cell=cell, depth=4, leading=1 if len(lines) > 2 else 2)
    title.location = at
    title.rotation_euler = rad(*tilt)
    cap_at, cap_cell = cfg["caption"]
    cap = font.text_object(f"{tag}_Caption", caption, b, material, col, palette="cream", cell=cap_cell, depth=3, leading=3)
    rows = len(caption) * 8 + (len(caption) - 1) * 3
    cap.location = (cap_at[0], cap_at[1], cap_at[2] - (rows - 8) * cap_cell)
    cap.rotation_euler = rad(*tilt)


def shot_cave_awake():
    """Beat 4a. Friends are in: the cave is lit, warm and busy."""
    scene, b, mats = S["scene"], S["b"], S["mats"]
    base = S.get("cave_col") or S.setdefault("cave_col", cave_set())
    shared = scenery.shot_collection(scene, "Shot_CaveAwakeShared")
    shaft = wld.SPOTS["shaft"]
    sx = (shaft["x"][0] + shaft["x"][1] + 1) / 2

    chest, _ = boxmodel.chest("Awake_Chest", col=shared, open_deg=-38)
    chest.location = on_floor(-0.4, 8.2)
    chest.rotation_euler = rad(0, 0, 8)

    warm = (1.0, 0.54, 0.18)
    for i, (x, y) in enumerate(TORCHES):
        t = boxmodel.torch(f"Awake_Torch_{i}", col=shared)
        t.location = on_floor(x, y)
        look.point(f"Awake_TorchLight_{i}", (x, y, t.location.z + 0.85), 110, warm, radius=0.15, col=shared)
    for i, (x, y) in enumerate(LANTERNS):
        lamp = boxmodel.lantern(f"Awake_Lantern_{i}", col=shared)
        lamp.location = on_floor(x, y)
        look.point(f"Awake_LanternLight_{i}", (x, y, lamp.location.z + 0.45), 70, warm, radius=0.12, col=shared)
    look.point("Awake_Furnace", (-2.5, 7.4, floor_z(-2.5, 7.0) + 0.5), 90, (1.0, 0.45, 0.12), radius=0.3, col=shared)
    look.point("Awake_Furnace2", (6.5, 3.3, floor_z(6.5, 3.0) + 0.5), 70, (1.0, 0.45, 0.12), radius=0.3, col=shared)
    for i, (x, y, z) in enumerate(((10.5, 5.5, -19.8), (13.0, 2.5, -20.6), (9.0, 8.0, -19.2))):
        look.point(f"Awake_Berries_{i}", (x, y, z), 45, (1.0, 0.70, 0.28), radius=0.5, shadow=False, col=shared)
    look.point("Awake_Diamonds", (-7.0, 3.0, -23.6), 26, (0.35, 0.95, 1.0), radius=0.4, shadow=False, col=shared)
    # Daylight down the shaft the pumpkin dug: the shot's signature beam.
    look.spot("Awake_Shaft", (sx, 1.0, 16.0), (sx, 1.0, -25.0), 70000, (1.0, 0.84, 0.62), angle_deg=4.6, blend=0.3, radius=0.4, volume=3.0, col=shared)
    # Cool sky bounce through the open front, so the rock keeps its shape.
    look.area("Awake_Front", (3, -12, -17), (3, 4, -23), energy=260, color=(0.42, 0.52, 1.0), size=18, col=shared)

    for suffix, cfg in CAVE_LAYOUTS.items():
        key = f"cave_awake{suffix}"
        tag = "".join(part.title() for part in key.split("_"))
        col = scenery.shot_collection(scene, f"Shot_{tag}")
        cave_type(tag, cfg, "awake", mats["type"], col)

        kx, ky = cfg["kurbis"]
        kurbis = mascot.build(f"{tag}_Kurbis", b, mats["mascot"], state="awake", col=col, size=1.0)
        kurbis.location = on_floor(kx, ky)
        kurbis.rotation_euler = rad(0, 0, 10)
        look.point(f"{tag}_Candle", (kx + 0.1, ky - 1.1, kurbis.location.z + 0.3), 34, (1.0, 0.58, 0.18), radius=0.4, shadow=False, col=col)
        # A warm key on the two friends, as if from the pumpkin and the lanterns.
        look.point(f"{tag}_CastKey", (kx + 0.4, ky - 2.2, kurbis.location.z + 1.9), 150, (1.0, 0.66, 0.34), radius=0.9, col=col)

        # Java friend with a pickaxe: mining the diamond outcrop, or cheering with it.
        sxy, yaw, pose = cfg["steve"]
        steve = boxmodel.player(f"{tag}_Steve", "steve", col=col)
        steve.place(on_floor(*sxy), yaw=yaw)
        steve.pose(**STEVE_POSES[pose])
        pick = boxmodel.item(f"{tag}_Pickaxe", "item/iron_pickaxe.png", col=col)
        pick.parent = steve.parts["hand_r"]
        turn = Matrix.Rotation(math.pi, 3, "X") @ Matrix.Rotation(math.pi / 2, 3, "Z")
        pick.rotation_euler = turn.to_euler()
        pick.location = Vector((0.0, -0.1, -0.34)) - turn @ Vector((-5 / 16, 0, -5 / 16))

        # Bedrock friend, mid-hop with both arms up.
        ax, ay = cfg["alex"]
        alex = boxmodel.player(f"{tag}_Alex", "alex", slim=True, col=col)
        alex.place((ax, ay, floor_z(ax, ay) + 0.2), yaw=-24)
        alex.pose(body=(-6, 0, 0), head=(-12, 6, -16), arm_r=(0, 152, 0), hand_r=(0, 24, 0), arm_l=(0, -140, 0), hand_l=(0, -30, 0), leg_r=(-38, 0, 0), foot_r=(62, 0, 0), leg_l=(14, 0, 0), foot_l=(34, 0, 0))

        for who, rig, label, lift in (("Steve", steve, "JAVA", 2.62), ("Alex", alex, "BEDROCK", 2.2)):  # Steve's clears his pickaxe
            name_tag = font.text_object(f"{tag}_Tag_{who}", [label], b, mats["type"], col, palette="cream", cell=cfg["tag"], depth=2)
            name_tag.location = (rig.root.location.x, rig.root.location.y, rig.root.location.z + lift)
            name_tag.rotation_euler = rad(-6, 0, 0)

        cam = look.camera(f"Cam_{tag}", cfg["eye"], cfg["aim"], lens=cfg["lens"], col=col)
        S["shots"][key] = {"collections": [base, shared, col, *undug("vault", "nether")], "camera": cam, "sun": 0.0, "world": 0.12, "exposure": 0.0, "size": cfg["size"]}


def shot_cave_asleep():
    """Beat 4b. Everyone left: lights out, and the cave is kept as one small glowing block."""
    scene, b, mats, w = S["scene"], S["b"], S["mats"], S["w"]
    base = S.get("cave_col") or S.setdefault("cave_col", cave_set())
    shared = scenery.shot_collection(scene, "Shot_CaveAsleepShared")
    shaft = wld.SPOTS["shaft"]
    sx = (shaft["x"][0] + shaft["x"][1] + 1) / 2

    chest, _ = boxmodel.chest("Asleep_Chest", col=shared, open_deg=0)
    chest.location = on_floor(-0.4, 8.2)
    chest.rotation_euler = rad(0, 0, 8)
    for i, (x, y) in enumerate(TORCHES):
        boxmodel.torch(f"Asleep_Torch_{i}", col=shared, lit=False).location = on_floor(x, y)
    for i, (x, y) in enumerate(LANTERNS):
        boxmodel.lantern(f"Asleep_Lantern_{i}", col=shared, lit=False).location = on_floor(x, y)
    # Moonlight: a thin cold beam down the shaft and a blue wash from the open front.
    look.spot("Asleep_Shaft", (sx, 1.0, 16.0), (sx, 1.0, -25.0), 15000, (0.55, 0.70, 1.0), angle_deg=4.6, blend=0.3, radius=0.4, volume=3.0, col=shared)
    look.area("Asleep_Front", (3, -12, -17), (3, 4, -23), energy=560, color=(0.30, 0.42, 1.0), size=18, col=shared)

    for suffix, cfg in CAVE_LAYOUTS.items():
        key = f"cave_asleep{suffix}"
        tag = "".join(part.title() for part in key.split("_"))
        col = scenery.shot_collection(scene, f"Shot_{tag}")
        cave_type(tag, cfg, "asleep", mats["type_dim"], col)

        kx, ky = cfg["kurbis"]
        kurbis = mascot.build(f"{tag}_Kurbis", b, mats["mascot"], state="asleep", col=col, size=1.0)
        kurbis.location = on_floor(kx, ky)
        kurbis.rotation_euler = rad(0, -5, 10)
        for i, (dx, dz, cell) in enumerate(((-0.55, 1.15, 0.024), (-0.9, 1.5, 0.032), (-1.35, 1.95, 0.042))):
            z = font.text_object(f"{tag}_Zzz_{i}", ["Z"], b, mats["type_dim"], col, palette="cream", cell=cell, depth=2)
            z.location = (kx + dx, ky - 0.2, kurbis.location.z + dz)
            z.rotation_euler = rad(0, -10 - i * 8, 8)

        # The snapshot: the cave itself, shrunk into exactly one block that keeps its own light.
        cx, cy = cfg["snapshot"]
        cz = floor_z(cx, cy) + 1.25
        snap = core.empty(f"{tag}_Snapshot", col, (cx, cy, cz))
        snap.rotation_euler = rad(10, 0, 24)
        size = 1.0
        mini = scenery.snapshot_block(f"{tag}_Mini", b, mats["mini"], col, size=size * 0.86)
        edge = scenery.frame_cube(f"{tag}_Edge", size, 0.022, mats["glow_edge"], col)
        glass = scenery.boxes_object(f"{tag}_Glass", [((-size / 2,) * 3, (size,) * 3)], mats["glass"], col)
        for obj in (mini, edge, glass):
            obj.parent = snap
        gold = (1.0, 0.72, 0.32)
        # Its own little light, inside the miniature cave, plus the glow it throws on the room.
        look.point(f"{tag}_SnapLight", (cx - 0.8, cy - 0.6, cz + 0.1), 40, gold, radius=0.4, shadow=False, col=col)
        look.point(f"{tag}_SnapFloor", (cx, cy - 0.3, cz - 0.9), 60, gold, radius=0.4, shadow=False, col=col)

        cam = look.camera(f"Cam_{tag}", cfg["eye"], cfg["aim"], lens=cfg["lens"], col=col)
        S["shots"][key] = {"collections": [base, shared, col, *undug("vault", "nether")], "camera": cam, "sun": 0.0, "world": 0.06, "exposure": 0.0, "emit": 0.1, "size": cfg["size"]}


VAULT_LAYOUTS = {
    # exhibits: (what, x, front y, plinth height). The label is embossed on the plinth.
    "vault": {
        "size": (1920, 1080),
        "eye": (0.0, -4.6, -42.5),
        "aim": (0.0, 4.5, -42.2),
        "lens": 20,
        "exhibits": (("console", -6.3, 4.2, 1), ("players", -3.15, 4.5, 1), ("backups", 0.0, 6.4, 2), ("settings", 3.15, 4.5, 1), ("ai", 6.3, 4.2, 1)),
        "label": 0.046,
        "title": (["TREASURE ROOM"], (0.0, 5.0, -40.25), 0.12),
        "caption": (["EVERYTHING TO RUN YOUR WORLD."], (0.0, 4.8, -40.95), 0.05, False),
        "kurbis": (0.0, 1.3, 0.0),
    },
    "vault_portrait": {
        "size": (1080, 1920),
        "eye": (0.0, -4.6, -42.2),
        "aim": (0.0, 4.6, -41.6),
        "lens": 24,
        "exhibits": (("players", -2.05, 2.8, 1), ("ai", 2.05, 2.8, 1), ("backups", 0.0, 4.8, 2), ("console", -1.75, 6.8, 3), ("settings", 1.75, 6.8, 3)),
        "label": 0.046,
        "title": (["TREASURE", "ROOM"], (0.0, 5.0, -39.85), 0.072),
        "caption": (["EVERYTHING TO RUN", "YOUR WORLD."], (0.0, 0.0, -44.84), 0.021, True),
        "kurbis": (0.0, 1.4, 0.0),
    },
}
LABELS = {"console": "CONSOLE", "players": "PLAYERS", "backups": "BACKUPS", "settings": "SETTINGS", "ai": "/AI"}


def shot_vault():
    """Beat 5. The treasure room: what you run the world with, set out as exhibits."""
    import numpy as np

    scene, b, mats = S["scene"], S["b"], S["mats"]
    shared = scenery.shot_collection(scene, "Shot_VaultShared")
    v = wld.SPOTS["vault"]
    floor, top = float(v["floor"]), float(v["top"] + 1)
    (x0, x1), (_, y1) = v["x"], v["y"]

    for i, x in enumerate(range(x0 + 2, x1, 5)):  # under each ceiling lamp
        for j, y in enumerate((2, 6)):
            look.point(f"Vault_Lamp_{i}{j}", (x + 0.5, y + 0.5, top - 0.5), 60, (1.0, 0.72, 0.36), radius=0.4, col=shared)
    sx = (v["shaft"][0] + v["shaft"][1] + 1) / 2
    look.spot("Vault_Shaft", (sx, 1.0, floor + 24), (sx, 1.0, floor), 16000, (1.0, 0.84, 0.62), angle_deg=5.0, blend=0.3, radius=0.4, volume=3.0, col=shared)
    look.area("Vault_Front", (0, -9, floor + 5), (0, 4, floor + 2), energy=220, color=(0.45, 0.55, 1.0), size=16, col=shared)
    look.fog("Vault_Haze", (x0 - 2, -8, floor - 8), (x1 + 3, y1 + 2, top + 14), density=0.008, color=(1.0, 0.9, 0.8), anisotropy=0.4, col=shared)

    for key, cfg in VAULT_LAYOUTS.items():
        tag = "".join(part.title() for part in key.split("_"))
        col = scenery.shot_collection(scene, f"Shot_{tag}")
        cell, depth = cfg["label"], 3

        for what, x, y, h in cfg["exhibits"]:
            plinth = np.full((3, 2, h), b["stone_bricks"], np.uint8)
            plinth[1, :, h - 1] = b["chiseled_stone_bricks"]
            voxel.grid_object(f"{tag}_Plinth_{what}", plinth, b, mats["voxel"], col, origin=(x - 1.5, y, floor))
            label = font.text_object(f"{tag}_Label_{what}", [LABELS[what]], b, mats["type"], col, palette="cream", cell=cell, depth=depth)
            label.location = (x, y - depth * cell, floor + h - 1 + (1 - 8 * cell) / 2)
            cx, cy, z = x, y + 1.0, floor + h
            # A museum spot on every exhibit; the haze shows its cone.
            look.spot(f"{tag}_Spot_{what}", (cx, cy - 1.6, top - 0.1), (cx, cy, z + 0.4), 2400, (1.0, 0.78, 0.46), angle_deg=34, blend=0.5, radius=0.1, volume=2.2, col=col)
            if what in ("console", "settings"):
                block = "command_block" if what == "console" else "smithing_table"
                voxel.grid_object(f"{tag}_{what}", np.full((1, 1, 1), b[block], np.uint8), b, mats["voxel"], col, origin=(cx - 0.5, cy - 0.5, z))
                if what == "console":
                    look.point(f"{tag}_ConsoleGlow", (cx, cy - 0.9, z + 0.5), 14, (1.0, 0.5, 0.15), radius=0.2, shadow=False, col=col)
            elif what == "backups":
                chest, _ = boxmodel.chest(f"{tag}_Chest", col=col, open_deg=-52)
                chest.location = (cx, cy, z)
                look.point(f"{tag}_ChestGlow", (cx, cy - 0.1, z + 1.0), 70, (1.0, 0.8, 0.3), radius=0.25, shadow=False, col=col)
            elif what == "players":
                for skin, slim, dx, yaw in (("steve", False, -0.42, 16), ("alex", True, 0.42, -14)):
                    hd = boxmodel.head(f"{tag}_Head_{skin}", skin, slim, col=col, scale=1.4)
                    hd.location = (cx + dx, cy - 0.1, z)
                    hd.rotation_euler = rad(0, 0, yaw)
            elif what == "ai":
                boxmodel.cage(f"{tag}_Spawner", col=col).location = (cx, cy, z)
                core_ = voxel.grid_object(f"{tag}_SpawnerCore", np.full((1, 1, 1), b["jack_o_lantern"], np.uint8), b, mats["mascot"], col, scale=0.36, origin=(-0.18, -0.18, -0.18))
                core_.location = (cx, cy, z + 0.46)
                core_.rotation_euler = rad(0, 0, 28)
                look.point(f"{tag}_SpawnerGlow", (cx, cy - 0.2, z + 0.5), 30, (1.0, 0.6, 0.2), radius=0.15, col=col)

        lines, at, tcell = cfg["title"]
        title = font.text_object(f"{tag}_Title", lines, b, mats["type"], col, palette="gold", cell=tcell, depth=4, leading=1)
        title.location = at
        lines, at, ccell, on_face = cfg["caption"]
        cap = font.text_object(f"{tag}_Caption", lines, b, mats["type"], col, palette="cream", cell=ccell, depth=3, leading=3)
        cap.location = (at[0], at[1] - (3 * ccell if on_face else 0), at[2])

        kx, ky, hop = cfg["kurbis"]
        kurbis = mascot.build(f"{tag}_Kurbis", b, mats["mascot"], state="awake", col=col, size=1.0)
        kurbis.location = (kx, ky, floor + hop)
        kurbis.rotation_euler = rad(-6 if hop else 0, 0, 8)
        look.point(f"{tag}_Candle", (kx, ky - 1.2, floor + 0.5), 30, (1.0, 0.58, 0.18), radius=0.4, shadow=False, col=col)

        cam = look.camera(f"Cam_{tag}", cfg["eye"], cfg["aim"], lens=cfg["lens"], col=col)
        S["shots"][key] = {"collections": [shared, col, *undug("nether")], "camera": cam, "sun": 0.0, "world": 0.1, "exposure": 0.0, "size": cfg["size"]}


PORTAL_LAYOUTS = {
    # The last beat: name your world, then step through.
    "portal": {
        "size": (1920, 1080),
        "eye": (0.5, -5.0, -58.2),
        "aim": (0.8, 5.0, -56.6),
        "lens": 20,
        "type_at": (-2.7, 4.0),
        "headline": (["PICK A NAME."], -53.3, 0.11),
        "address": (False, -54.75, 0.06),
        "caption": (["STEP THROUGH TO YOUR WORLD.", "FREE. NO CARD. NO CATCH."], -56.2, 0.045),
        "kurbis": (1.9, 2.0, 0.45, 24),
    },
    "portal_portrait": {
        "size": (1080, 1920),
        "eye": (5.0, -6.0, -58.3),
        "aim": (5.0, 5.0, -55.4),
        "lens": 24,
        "type_at": (5.0, 4.6),
        "headline": (["PICK A NAME."], -51.25, 0.085),
        "address": (True, -53.25, 0.085),
        "caption": (["STEP THROUGH.", "FREE. NO CARD. NO CATCH."], -54.5, 0.045),
        "kurbis": (4.3, 2.4, 0.4, -10),
    },
}


def shot_portal():
    """Beat 6. The portal: pick a name, step through to your world."""
    import numpy as np

    scene, b, mats = S["scene"], S["b"], S["mats"]
    shared = scenery.shot_collection(scene, "Shot_PortalShared")
    n = wld.SPOTS["nether"]
    floor, top = float(n["floor"]), float(n["top"] + 1)
    (x0, x1), (_, y1) = n["x"], n["y"]
    px, py = n["portal"]

    # The portal's surface: the swirling texture's first frame, tiled over the 2 x 3 opening.
    mat = boxmodel.tex_material("Portal", "block/nether_portal.png", clip=False, emit=2.6)
    quads, uvs = [], []
    for i in range(2):
        for k in range(3):
            xa, za = px + 1 + i, floor + 2 + k
            quads.append([(xa, py + 0.5, za), (xa + 1, py + 0.5, za), (xa + 1, py + 0.5, za + 1), (xa, py + 0.5, za + 1)])
            uvs.append([(0, 31 / 32), (1, 31 / 32), (1, 1), (0, 1)])
    surface = bpy.data.objects.new("Portal_Surface", core.mesh_from_quads("Portal_Surface", np.asarray(quads, np.float32), np.asarray(uvs, np.float32)))
    surface.data.materials.append(mat)
    core.link(surface, shared)
    centre = Vector((px + 2.0, py - 0.6, floor + 3.5))
    look.point("Portal_Glow", centre, 900, (0.62, 0.22, 1.0), radius=0.8, shadow=True, col=shared)
    look.point("Portal_GlowLow", centre + Vector((0, -1.6, -2.2)), 260, (0.62, 0.22, 1.0), radius=0.6, shadow=False, col=shared)
    scenery.leaves("Portal_Sparks", mats["spark"], shared, lo=(px - 1.5, py - 4.5, floor + 0.6), hi=(px + 5.5, py + 0.3, floor + 7.5), count=70, size=(0.025, 0.06), seed=9)

    # Lava either side throws orange up the walls.
    for i, y in enumerate((1.5, 5.0, 8.0)):
        for side, x in (("L", x0 + 1.0), ("R", x1)):
            look.point(f"Portal_Lava_{side}{i}", (x, y, floor + 0.4), 520, (1.0, 0.36, 0.07), radius=0.7, col=shared)
    sx = (n["shaft"][0] + n["shaft"][1] + 1) / 2
    look.spot("Portal_Shaft", (sx, 1.0, top + 14), (sx, 1.0, floor), 9000, (1.0, 0.84, 0.62), angle_deg=5.0, blend=0.3, radius=0.4, volume=3.0, col=shared)
    look.area("Portal_Front", (0, -10, floor + 7), (0, 4, floor + 3), energy=160, color=(0.5, 0.5, 1.0), size=16, col=shared)
    look.fog("Portal_Haze", (x0 - 2, -9, floor - 3), (x1 + 3, y1 + 2, top + 10), density=0.012, color=(1.0, 0.8, 0.8), anisotropy=0.4, col=shared)

    for key, cfg in PORTAL_LAYOUTS.items():
        tag = "".join(part.title() for part in key.split("_"))
        col = scenery.shot_collection(scene, f"Shot_{tag}")
        tx, ty = cfg["type_at"]

        lines, z, cell = cfg["headline"]
        head = font.text_object(f"{tag}_Headline", lines, b, mats["type"], col, palette="gold", cell=cell, depth=4)
        head.location = (tx, ty, z)

        # The name field: the part you type is cream and underlined. (No caret: at
        # this size a caret block reads as the letter I.)
        stacked, z, cell = cfg["address"]
        name = font.text_object(f"{tag}_Name", ["YOURNAME"], b, mats["type"], col, palette="cream", cell=cell, depth=4)
        host = font.text_object(f"{tag}_Host", [".KURBIS.HOST"], b, mats["type"], col, palette="gold", cell=cell, depth=4)
        name_w, host_w = 58, 71
        if stacked:
            left, name_z = tx - name_w * cell / 2, z + 10.5 * cell
            name.location = (tx, ty, name_z)
            host.location = (tx, ty, z)
        else:
            left, name_z = tx - (name_w + 1 + host_w) * cell / 2, z
            name.location = (left + name_w * cell / 2, ty, z)
            host.location = (left + (name_w + 1 + host_w / 2) * cell, ty, z)
        bar = np.full((name_w, 4, 1), b["type_cream"], np.uint8)
        voxel.grid_object(f"{tag}_Underline", bar, b, mats["type"], col, group="type", scale=cell, origin=(left, ty, name_z - 2.2 * cell))

        lines, z, cell = cfg["caption"]
        cap = font.text_object(f"{tag}_Caption", lines, b, mats["type"], col, palette="cream", cell=cell, depth=3, leading=3)
        cap.location = (tx, ty, z)

        kx, ky, hop, yaw = cfg["kurbis"]
        kurbis = mascot.build(f"{tag}_Kurbis", b, mats["mascot"], state="whee", col=col, size=1.0)
        kurbis.location = (kx, ky, floor + hop)
        kurbis.rotation_euler = rad(-8, 10, yaw)
        look.point(f"{tag}_Candle", (kx, ky - 1.2, floor + hop + 0.3), 26, (1.0, 0.58, 0.18), radius=0.4, shadow=False, col=col)

        cam = look.camera(f"Cam_{tag}", cfg["eye"], cfg["aim"], lens=cfg["lens"], col=col)
        S["shots"][key] = {"collections": [shared, col], "camera": cam, "sun": 0.0, "world": 0.05, "exposure": 0.0, "size": cfg["size"]}


LANDING_LAYOUTS = {
    # The world is the same; the cast stands closer together for the narrow frame.
    # Scale rule: players are two blocks; the pumpkin, the barrel and the one-block rise
    # of the meadow behind them are one, all at the cast's depth, so the eye can count.
    "landing": {
        "size": (1920, 1080),
        "eye": (-2.9, -4.2, 2.2),
        "pitch": 7.0,
        "lens": 20,
        "cell": 0.125,
        "address": (["STEVE"], [".KURBIS.HOST"], -3.05, False, 4.25),
        "caption": 0.05,
        "tag": 0.042,
        "kurbis": (-0.9, 2.2),
        "steve": (-4.3, 3.2),
        "alex": (1.6, 3.2),
    },
    "landing_portrait": {
        "size": (1080, 1920),
        "eye": (-1.8, -5.4, 2.2),
        "pitch": 8.0,
        "lens": 24,
        "cell": 0.1,
        "address": (["STEVE"], [".KURBIS.HOST"], -1.85, True, 4.3),
        "caption": 0.042,
        "tag": 0.034,
        "kurbis": (-1.6, 2.2),
        "steve": (-3.7, 3.2),
        "alex": (0.2, 3.2),
    },
}


def shot_landing():
    """Beat 2. The pumpkin has landed in the meadow. The address stands on the hill
    behind it, and two friends on different editions are already there."""
    scene, b, mats = S["scene"], S["b"], S["mats"]
    shared = scenery.shot_collection(scene, "Shot_LandingShared")
    w = S["w"]

    def ground_at(x, y):
        """Top of the terrain under a point: the cast stands on the real ground."""
        return w.top(int(math.floor(x)), int(math.floor(y))) + 1.0

    type_y = 4.6  # the address floats just behind and above the cast, in front of the blossom tree

    # Slab clouds high in the dusk, so the sky above the address is not empty.
    for cname, center, cells, csize, seed in (
        ("a", (-16, 70, 52.0), (7, 3), (3.5, 3.5, 1.4), 6),
        ("b", (14, 96, 78.0), (8, 3), (4.0, 4.0, 1.6), 4),
        ("c", (-4, 130, 124.0), (9, 3), (5.0, 5.0, 2.0), 8),
        ("d", (46, 84, 46.0), (8, 3), (3.5, 3.5, 1.4), 2),
    ):
        scenery.cloud(f"Landing_Cloud_{cname}", b, mats["cloud"], center, cells, csize, seed, shared, profile=(1, 0))

    # Leaves and petals stay out of the air in front of the type.
    clear = [((-16, -8, 2.4), (10, type_y + 1.5, 8.0))]
    scenery.leaves("Landing_Petals", mats["petal"], shared, lo=(0, -3, 1.4), hi=(13, 11, 12), count=46, size=(0.03, 0.07), seed=3, keep_clear=clear)
    scenery.leaves("Landing_Leaves", mats["leaf"], shared, lo=(-20, -4, 1.4), hi=(12, 30, 18), count=90, size=(0.04, 0.09), seed=4, keep_clear=clear)
    # Backlit by the low sun. A warm key from the camera side keeps faces and grass alive.
    look.sun("Landing_Key", (-0.38, -0.5, 0.78), energy=2.3, color=(1.0, 0.62, 0.42), angle_deg=6.0, volume=0.0, col=shared)
    look.fog("Landing_Mist", (-80, -14, 0.6), (80, 140, 30), density=0.005, color=(1.0, 0.74, 0.5), anisotropy=0.76, col=shared)

    for key, cfg in LANDING_LAYOUTS.items():
        tag_name = "".join(part.title() for part in key.split("_"))
        col = scenery.shot_collection(scene, f"Shot_{tag_name}")

        # The address. The name is the part you choose, so it is set apart in
        # colour (and on its own line when narrow), with the caption over it.
        name_lines, host_lines, centre, stacked, base = cfg["address"]
        cell = cfg["cell"]
        name = font.text_object(f"{tag_name}_Name", name_lines, b, mats["type"], col, palette="cream", cell=cell, depth=4)
        host = font.text_object(f"{tag_name}_Host", host_lines, b, mats["type"], col, palette="gold", cell=cell, depth=4)
        if stacked:
            name.location = (centre, type_y, base + 9 * cell)
            host.location = (centre, type_y, base)
            top = base + 17 * cell
        else:
            total = 106  # cells: STEVE (34) + gap (1) + .KURBIS.HOST (71)
            name.location = (centre + (-total / 2 + 17) * cell, type_y, base)
            host.location = (centre + (-total / 2 + 35 + 35.5) * cell, type_y, base)
            top = base + 8 * cell
        caption = font.text_object(f"{tag_name}_Caption", ["YOU CHOOSE THE NAME."], b, mats["type"], col, palette="cream", cell=cfg["caption"], depth=3)
        caption.location = (centre, type_y, top + 5 * cfg["caption"])

        kx, ky = cfg["kurbis"]
        kurbis = mascot.build(f"{tag_name}_Kurbis", b, mats["mascot"], state="awake", col=col, size=1.0)
        ground = ground_at(kx, ky)
        kurbis.location = (kx, ky, ground)
        kurbis.rotation_euler = rad(0, 0, 6)
        look.point(f"{tag_name}_Candle", (kx, ky - 1.1, ground + 0.4), 40, (1.0, 0.6, 0.2), radius=0.3, shadow=False, col=col)

        # Java friend, fishing from the pond's bank.
        sx, sy = cfg["steve"]
        steve = boxmodel.player(f"{tag_name}_Steve", "steve", col=col)
        steve.place((sx, sy, ground_at(sx, sy)), yaw=-58)
        steve.pose(body=(-4, 0, 0), head=(4, 0, 38), arm_r=(-72, 0, 6), hand_r=(-24, 0, 0), arm_l=(-22, 0, 0), hand_l=(-26, 0, 0), leg_r=(-8, 0, 0), foot_r=(4, 0, 0), leg_l=(7, 0, 0), foot_l=(2, 0, 0))
        # Rod: gripped by the handle, pointing ahead of the forearm and up.
        rod = boxmodel.item(f"{tag_name}_Rod", "item/fishing_rod.png", col=col)
        rod.parent = steve.parts["hand_r"]
        turn = Matrix.Rotation(math.pi, 3, "X") @ Matrix.Rotation(math.pi / 2, 3, "Z")
        scale = 1.5
        rod.rotation_euler = turn.to_euler()
        rod.scale = (scale,) * 3
        rod.location = Vector((0.0, -0.1, -0.34)) - turn @ (Vector((-6 / 16, 0, -6 / 16)) * scale)
        bpy.context.view_layer.update()
        tip = rod.matrix_world @ Vector((7.5 / 16, 0, 7.5 / 16))
        float_at = Vector((sx - 3.4, sy - 1.9, 1.02))
        scenery.strand(f"{tag_name}_Line", tip, float_at + Vector((0, 0, 0.1)), 0.025, mats["line"], col)
        scenery.boxes_object(f"{tag_name}_Bobber", [(float_at - Vector((0.07, 0.07, 0.0)), (0.14, 0.14, 0.16))], mats["bobber"], col)

        # Bedrock friend, mid-hop, waving at the pumpkin.
        ax, ay = cfg["alex"]
        alex = boxmodel.player(f"{tag_name}_Alex", "alex", slim=True, col=col)
        alex.place((ax, ay, ground_at(ax, ay) + 0.16), yaw=-30)
        alex.pose(body=(-5, 0, 0), head=(-8, 6, -14), arm_r=(0, 150, 0), hand_r=(0, 22, 0), arm_l=(-14, -18, 0), hand_l=(-30, 0, 0), leg_r=(-34, 0, 0), foot_r=(58, 0, 0), leg_l=(12, 0, 0), foot_l=(30, 0, 0))

        for who, rig, label in (("Steve", steve, "JAVA"), ("Alex", alex, "BEDROCK")):
            tag = font.text_object(f"{tag_name}_Tag_{who}", [label], b, mats["type"], col, palette="cream", cell=cfg["tag"], depth=2)
            tag.location = (rig.root.location.x, rig.root.location.y, rig.root.location.z + 2.2)

        # Low and tilted up, as the trailers do: the cast is large and the sky is in the shot.
        eye = Vector(cfg["eye"])
        cam = look.camera(f"Cam_{tag_name}", eye, eye + Vector((-0.2, 14.5, 14.5 * math.tan(math.radians(cfg["pitch"])))), lens=cfg["lens"], col=col)
        S["shots"][key] = {"collections": [shared, col, *undug("shaft", "vault", "nether")], "camera": cam, "sun": 7.0, "world": 1.0, "exposure": 0.0, "size": cfg["size"]}


DIG_LAYOUTS = {
    # Jokes are embossed on the cut face at the depth where the pumpkin passes them.
    # type: (lines, palette, cell, (centre x, bottom z), leading)
    "dig": {
        "size": (1920, 1080),
        "eye": (1.2, -13.5, -6.0),
        "aim": (-2.3, 0.0, -6.6),
        "lens": 24,
        "kurbis": -7.7,
        "type": (
            (["COST: NOTHING."], "gold", 0.08, (3.0, -4.4), 1),
            (["HIDDEN FEES: DIRT."], "cream", 0.056, (3.0, -5.5), 1),
            (["DAY 1. DUG DOWN. DIRT."], "cream", 0.056, (-8.2, -6.4), 1),
            (["DAY 2. MORE DIRT."], "cream", 0.056, (-8.2, -8.7), 1),
            (["DAY 3. STILL DIRT.", "MORALE: HIGH."], "cream", 0.056, (-8.2, -11.6), 3),
            (["KEEP DIGGING \u2193"], "gold", 0.066, (3.0, -11.1), 1),
        ),
    },
    "dig_portrait": {
        "size": (1080, 1920),
        "eye": (-0.2, -17.0, -7.4),
        "aim": (-2.0, 0.0, -8.0),
        "lens": 24,
        "kurbis": -9.4,
        "type": (
            (["COST:", "NOTHING."], "gold", 0.085, (1.9, -4.9), 1),
            (["HIDDEN FEES:", "DIRT."], "cream", 0.06, (1.9, -6.6), 3),
            (["DAY 1.", "DUG DOWN.", "DIRT."], "cream", 0.0625, (-6.0, -8.9), 3),
            (["DAY 2.", "MORE DIRT."], "cream", 0.0625, (1.9, -11.6), 3),
            (["DAY 3.", "STILL DIRT.", "MORALE: HIGH."], "cream", 0.0625, (-5.9, -14.6), 3),
            (["KEEP", "DIGGING \u2193"], "gold", 0.075, (1.9, -15.6), 2),
        ),
    },
}


def shot_dig():
    """Beat 3. The cross-section. The pumpkin tumbles down the shaft it is digging,
    past jokes in the dirt, toward a glow far below."""
    scene, b, mats, w = S["scene"], S["b"], S["mats"], S["w"]
    shared = scenery.shot_collection(scene, "Shot_DigShared")
    shaft = wld.SPOTS["shaft"]
    sx = (shaft["x"][0] + shaft["x"][1] + 1) / 2  # middle of the shaft

    cx, cz = wld.SPOTS["chest"]
    chest, _ = boxmodel.chest("Dig_Chest", col=shared, open_deg=0)
    chest.location = (cx + 1.0, 0.5, cz)
    chest.rotation_euler = rad(0, 0, 6)

    # Daylight comes from above and fades with depth: a soft raking key for the relief,
    # plus a big lamp over the meadow whose falloff darkens the wall on the way down.
    look.sun("Dig_Key", (-0.52, -0.56, 0.64), energy=0.9, color=(1.0, 0.86, 0.72), angle_deg=5.0, volume=0.0, col=shared)
    look.area("Dig_Daylight", (-3.0, -7.0, 7.0), (-3.0, 0.0, -9.0), energy=2600, color=(1.0, 0.80, 0.62), size=22, shadow=True, col=shared)
    look.spot("Dig_Shaft", (sx, 1.0, 24.0), (sx, 1.0, -25.0), 90000, (1.0, 0.82, 0.58), angle_deg=4.4, blend=0.3, radius=0.5, volume=3.0, col=shared)
    look.point("Dig_CaveGlow", (2.5, 4.0, -21.5), 2600, (1.0, 0.56, 0.2), radius=1.0, col=shared)
    look.point("Dig_ShaftGlow", (sx, 1.0, -16.0), 700, (1.0, 0.5, 0.18), radius=0.6, col=shared)
    look.fog("Dig_Haze", (-34, -5, -32), (34, 7, 2), density=0.011, color=(1.0, 0.9, 0.8), anisotropy=0.4, col=shared)

    for key, cfg in DIG_LAYOUTS.items():
        tag = "".join(part.title() for part in key.split("_"))
        col = scenery.shot_collection(scene, f"Shot_{tag}")

        for i, (lines, palette, cell, (x, z), lead) in enumerate(cfg["type"]):
            depth = 4 if palette == "gold" else 3
            obj = font.text_object(f"{tag}_Type_{i}", lines, b, mats["type"], col, palette=palette, cell=cell, depth=depth, leading=lead)
            obj.location = (x, -depth * cell, z)

        kz = cfg["kurbis"]
        kurbis = mascot.build(f"{tag}_Kurbis", b, mats["mascot"], state="whee", col=col, size=1.0)
        kurbis.location = (sx + 0.1, 0.65, kz)
        kurbis.rotation_euler = rad(8, 19, 6)
        look.point(f"{tag}_Glow", (sx, -1.2, kz + 0.2), 22, (1.0, 0.6, 0.2), radius=0.4, shadow=False, col=col)
        look.point(f"{tag}_GlowBelow", (sx, 1.0, kz - 1.4), 60, (1.0, 0.6, 0.2), radius=0.3, col=col)
        scenery.debris(f"{tag}_Debris", b, mats["voxel"], col, ("dirt", "dirt", "stone", "gravel"), (sx, 0.4, kz + 1.6), 14, (1.3, 0.9, 1.5), seed=7)
        streaks = []
        for dx, dy, lo, hi in ((-0.7, 0.9, 1.5, 3.4), (-0.2, 1.5, 2.0, 4.6), (0.45, 0.6, 1.3, 3.0), (0.8, 1.3, 2.2, 4.0)):
            streaks.append(((sx + dx - 0.02, dy, kz + lo), (0.04, 0.04, hi - lo)))
        scenery.boxes_object(f"{tag}_Streaks", streaks, mats["streak"], col)

        cam = look.camera(f"Cam_{tag}", cfg["eye"], cfg["aim"], lens=cfg["lens"], col=col)
        S["shots"][key] = {"collections": [shared, col, *undug("vault", "nether")], "camera": cam, "sun": 7.0, "world": 0.55, "exposure": 0.0, "size": cfg["size"]}


def activate(name):
    scene = S["scene"]
    wanted = set(S["shots"][name]["collections"])
    for shot in S["shots"].values():
        for key in shot["collections"]:
            c = bpy.data.collections[key]
            c.hide_render = c.hide_viewport = key not in wanted
    shot = S["shots"][name]
    scene.camera = shot["camera"]
    S["sun"].data.energy = shot["sun"]
    scene.world.node_tree.nodes["Background"].inputs["Strength"].default_value = shot["world"]
    S["mats"]["voxel"].node_tree.nodes["EmitStrength"].inputs["Scale"].default_value = shot.get("emit", 4.0)
    return shot


def render(name, width=None, height=None, samples=32, suffix="", scale=1.0, **settings):
    scene = S["scene"]
    shot = activate(name)
    full = shot.get("size", (1920, 1080))
    width = width or int(full[0] * scale)
    height = height or int(full[1] * scale)
    settings.setdefault("exposure", shot["exposure"])
    report = look.render_settings(scene, width, height, samples, **settings)
    path = core.RENDERS / f"{name}{suffix}.png"
    scene.render.filepath = str(path)
    t0 = time.time()
    bpy.ops.render.render(write_still=True)
    return {"path": str(path), "seconds": round(time.time() - t0, 2), "unavailable": report}


def save():
    bpy.ops.wm.save_as_mainfile(filepath=str(core.BLEND / "gate0.blend"))


def closeup(name, target, offset=(1.5, -4.5, 0.6), lens=50, size=(720, 720)):
    """Debug view of one subject under the active shot's lighting."""
    scene = S["scene"]
    keep = scene.camera
    cam = look.camera("Cam_Closeup", Vector(target) + Vector(offset), target, lens=lens, col="Cameras")
    scene.camera = cam
    look.render_settings(scene, size[0], size[1], 24)
    scene.render.filepath = str(core.RENDERS / f"debug_{name}.png")
    bpy.ops.render.render(write_still=True)
    bpy.data.objects.remove(cam, do_unlink=True)
    scene.camera = keep


def build_all():
    """Everything, without rendering. Renders are triggered one at a time."""
    out = {"build": build()}
    for shot in (shot_hero, shot_landing, shot_dig, shot_cave_awake, shot_cave_asleep, shot_vault, shot_portal):
        shot()
    out["shots"] = sorted(S["shots"])
    return out


if __name__ == "__main__":
    result = build_all()
