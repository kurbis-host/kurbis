"""The Nether and the portal: obsidian, blackstone, netherrack, magma, basalt, glowstone,
lava and the portal's swirl.

Where the toolkit lets a block glow only in its coloured pixels (crying obsidian,
gilded blackstone, magma: `blocks.saturated`), the rock around them is kept close to
grey, so only the veins shine."""

import itertools

import numpy as np

from . import Canvas, cells, edges, rgb, stamp, texture, tile
from .terrain import cobbled

OBSIDIAN = ("#160f22", "#211833", "#1b1329")


def obsidian_base():
    labels, _ = cells(7, 31, squash=1.4)
    c = Canvas()
    for k, tone in enumerate(OBSIDIAN * 3):
        c.where(labels == k, tone)
    seam, lit = edges(labels)
    c.where(lit, "#352b48")
    c.where(seam, "#0c0814")
    for x, y in ((3, 2), (12, 6), (6, 11), (13, 13)):
        c.rect(x, y, 1, 1, "#4a4260")
    return c


@texture("block/obsidian.png")
def obsidian():
    return obsidian_base()


@texture("block/crying_obsidian.png")
def crying_obsidian():
    c = obsidian_base()
    # Tears run down from bright pools.
    return c.draw(
        """
..........cc....
..b.......cc....
..b.......b.....
..c.............
..c.......b.....
..........b..cc.
...........b.cc.
.....cc.....b...
.....cc......b..
.....b..........
......b.........
......c...cc....
..........cc....
.cc........b....
.cc........c....
..b.............
""",
        {"b": "#8a2be2", "c": "#c77dff"},
    )


@texture("block/blackstone.png", "block/blackstone_top.png")
def blackstone(path):
    seed = 41 if path.endswith("top.png") else 43
    c, _ = cobbled(9, seed, ("#2f2932", "#38313b", "#2a252d"), "#1e1a21", "#474049")
    return c


@texture("block/polished_blackstone_bricks.png")
def polished_blackstone_bricks():
    return bricks(8, 4, {"face": "#3b3440", "top": "#4b4351", "seam": "#211c25", "shade": "#332d38"})


@texture("block/gilded_blackstone.png")
def gilded_blackstone():
    c, _ = cobbled(9, 43, ("#2f2932", "#38313b", "#2a252d"), "#1e1a21", "#474049")
    nugget = """
.c.
cbb
.ba
"""
    return stamp(c, nugget, {"a": "#c47c18", "b": "#f2b630", "c": "#ffe07a"}, [(2, 3), (11, 2), (7, 9), (13, 12), (1, 12)])


def bricks(w, h, colours, size=16):
    """Courses of bricks, every other course shifted half a brick: a lit top row, a
    seam below and to the right of each brick."""
    c = Canvas(size, size, fill=colours["face"])
    for row in range(size // h):
        y = row * h
        shift = (w // 2) * (row % 2)
        c.rect(0, y, size, 1, colours["top"])
        c.rect(0, y + h - 1, size, 1, colours["seam"])
        c.rect(0, y + h - 2, size, 1, colours["shade"])
        for x in range(shift - 1, size, w):
            c.rect(x % size, y, 1, h, colours["seam"])
    return c


@texture("block/nether_bricks.png")
def nether_bricks():
    return bricks(8, 4, {"face": "#4b1d22", "top": "#632a2f", "seam": "#2a0e12", "shade": "#41181d"})


@texture("block/netherrack.png")
def netherrack():
    c, _ = cobbled(10, 17, ("#7c2c2c", "#6e2627", "#883434"), "#521b1d", "#9c4241")
    return c


@texture("block/magma.png")
def magma():
    # Cooled plates (near grey, so they stay dark) over a glowing seam.
    labels, _ = cells(6, 9)
    c = Canvas()
    for k, tone in enumerate(("#3c2b28", "#47332e", "#3a2926", "#45312c", "#3e2d29", "#48342f")):
        c.where(labels == k, tone)
    seam, lit = edges(labels)
    c.where(lit, "#57403a")
    wide = seam | np.roll(seam, 1, axis=0) & np.roll(seam, -1, axis=1)
    c.where(wide, "#ff7a1a")
    c.where(seam & np.roll(seam, 1, axis=1), "#ffc23a")
    return c


@texture("block/basalt_side.png")
def basalt_side():
    c = Canvas(fill="#4b4b52")
    tones = ("#4b4b52", "#56565e", "#43434a", "#505058", "#5c5c64")
    bounds = (0, 3, 7, 10, 13, 16)
    for k, (x0, x1) in enumerate(itertools.pairwise(bounds)):
        c.rect(x0, 0, x1 - x0, 16, tones[k])
        c.rect(x0, 0, 1, 16, "#62626b")
        c.rect(x1 - 1, 0, 1, 16, "#36363c")
    for x, y in ((1, 5), (4, 11), (8, 2), (11, 8), (14, 13)):
        c.rect(x, y, 2, 1, "#36363c")
    return c


@texture("block/basalt_top.png")
def basalt_top():
    c, _ = cobbled(5, 13, ("#4b4b52", "#56565e", "#43434a"), "#34343a", "#64646c")
    return c


@texture("block/glowstone.png")
def glowstone():
    labels, pts = cells(8, 27)
    c = Canvas(fill="#e9a33c")
    yy, xx = np.mgrid[0:16, 0:16] + 0.5
    for k, (px, py) in enumerate(pts):
        dx = (xx - px + 8) % 16 - 8
        dy = (yy - py + 8) % 16 - 8
        mine = labels == k
        c.where(mine & (np.abs(dx) + np.abs(dy) < 2.6), "#ffd06a")
        c.where(mine & (np.abs(dx) + np.abs(dy) < 1.2), "#fff3c4")
    seam, _ = edges(labels)
    c.where(seam, "#b36e22")
    return c


@texture("block/lava_still.png")
def lava():
    return tile(
        """
....ll......d...
...llll....dd...
....lwl.........
.....l......ll..
d..........llll.
dd..........lwl.
........d....l..
..ll....dd......
.llll...........
..lwl......d....
...l.......dd...
.........ll.....
....d...llll....
...dd....lwl....
..........l....d
...............d
""",
        {"d": "#e2560c", "l": "#ffa22c", "w": "#ffd35a"},
        "#ff7c16",
    )


@texture("block/nether_portal.png")
def nether_portal():
    """32 frames, one above the other (the page plays them as the game does): arms of
    a slow swirl turning about the middle, cut into five purples."""
    frames = 32
    yy, xx = np.mgrid[0:16, 0:16] + 0.5
    dx, dy = xx - 8, yy - 8
    r = np.hypot(dx, dy)
    a = np.arctan2(dy, dx)
    tones = [rgb(c) for c in ("#3c0d93", "#5718c4", "#7a2ef0", "#a466ff", "#d6acff")]
    out = Canvas(16, 16 * frames)
    for f in range(frames):
        t = f / frames * 2 * np.pi
        wave = (
            np.sin(2 * a + r * 0.75 - t) * 0.5
            + np.sin(3 * a - r * 0.4 + 2 * t) * 0.25
            + np.cos(r * 0.9 - 2 * t) * 0.2
            + np.sin(xx * 0.55 + t) * np.cos(yy * 0.45 - t) * 0.3  # so the arms are not all alike
        )
        level = np.clip(((wave + 1.1) / 2.2 * len(tones)).astype(int), 0, len(tones) - 1)
        for k, tone in enumerate(tones):
            out.px[f * 16 : (f + 1) * 16][level == k] = tone
    return out
