"""The Kurbis pumpkin: one block with a carved face that lights up when awake."""

import numpy as np

from . import core, voxel

# 16x16, row 0 at the top. '#' is carved.
FACES = {
    "awake": [
        "................",
        "................",
        "................",
        "................",
        "...##......##...",
        "..####....####..",
        "..####....####..",
        "..####....####..",
        "................",
        "................",
        ".##..........##.",
        ".####..##..####.",
        "..############..",
        "....########....",
        "................",
        "................",
    ],
    "whee": [
        "................",
        "................",
        "................",
        "...##......##...",
        "..#..#....#..#..",
        ".#....#..#....#.",
        "................",
        "................",
        "..############..",
        "..############..",
        "...##########...",
        "....########....",
        ".....######.....",
        "................",
        "................",
        "................",
    ],
    "asleep": [
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "..#..#....#..#..",
        "...##......##...",
        "................",
        "................",
        "................",
        ".......##.......",
        ".......##.......",
        "................",
        "................",
        "................",
    ],
}
CARVED = (0.22, 0.085, 0.01)
CANDLE = (1.0, 0.86, 0.24)
EMBER = (1.0, 0.42, 0.06)


def _face(blocks, key, lit):
    px = core.pixels("block/pumpkin_side.png")
    # Soften the ribs on the face side so the features are the darkest shapes there.
    base = px[..., :3].reshape(-1, 3).max(axis=0)
    px[..., :3] = px[..., :3] * 0.3 + base * 0.7
    mask = np.array([[c == "#" for c in row] for row in FACES[key]], bool)[::-1]
    glow = np.zeros_like(px)
    if lit >= 1.0:
        px[mask, :3] = CANDLE
        glow[mask, :3] = CANDLE
    else:
        px[mask, :3] = CARVED
        glow[mask, :3] = np.asarray(EMBER) * lit
    return blocks.custom(f"kurbis_{key}", px, glow)


def register(blocks):
    side, top = blocks.tex("pumpkin_side"), blocks.tex("pumpkin_top")
    blocks.cube("kurbis_awake", side=side, front=_face(blocks, "awake", 1.0), top=top, bottom=top, group="mascot")
    blocks.cube("kurbis_whee", side=side, front=_face(blocks, "whee", 1.0), top=top, bottom=top, group="mascot")
    blocks.cube("kurbis_asleep", side=side, front=_face(blocks, "asleep", 0.3), top=top, bottom=top, group="mascot")
    blocks.cube("kurbis_stem", all=blocks.flat((0.33, 0.40, 0.13)), group="mascot")


def build(name, blocks, material, state="awake", col="Characters", size=1.0):
    """Root empty at the bottom centre, so squash and stretch pivot on the ground."""
    root = core.empty(name, col)
    body = np.full((1, 1, 1), blocks[f"kurbis_{state}"], np.uint8)
    obj = voxel.grid_object(f"{name}_body", body, blocks, material, col, group="mascot", origin=(-0.5, -0.5, 0))
    obj.parent = root
    stem = np.zeros((3, 2, 3), np.uint8)
    stem[0:2, :, 0:2] = blocks["kurbis_stem"]
    stem[1:3, :, 2] = blocks["kurbis_stem"]
    s = 0.105
    tip = voxel.grid_object(f"{name}_stem", stem, blocks, material, col, group="mascot", scale=s, origin=(-s, -s, 1.0))
    tip.parent = root
    root.scale = (size,) * 3
    return root
