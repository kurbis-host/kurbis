"""The overworld's ground: soil, stone, ores, wood and leaves."""

import numpy as np

from . import Canvas, cells, edges, rgb, stamp, texture, tile

# -- soil ---------------------------------------------------------------------------
DIRT = {".": "#bd7a3c", "d": "#a2622d", "l": "#d39250", "p": "#8f6a4c"}
DIRT_MAP = """
................
..dd.......l....
...d............
.........dd.....
.l.......d......
......p.........
...........ll...
..dd............
................
......dd.....d..
.l.....d.....dd.
...........l....
...dd...........
.p..............
.........d....l.
..l.............
"""

GRASS = "#a3d95f"  # the side's top-left pixel: the toolkit tints the (grey) top with it
GRASS_DARK = "#84ba48"
GRASS_LIGHT = "#b8e67a"


@texture("block/dirt.png")
def dirt():
    return tile(DIRT_MAP, DIRT, DIRT["."])


@texture("block/grass_block_top.png")
def grass_top():
    # Grey, tinted green by the toolkit: blades as short strokes, a few lit tips.
    return tile(
        """
................
..d.......l.....
..d.....d.......
.......d.....d..
.l...........d..
.....d..........
....d......l....
..........d.....
.d........d.....
.d...l..........
......d.....d...
.....d.....d....
..l.........l...
.........d......
...d....d.......
...d............
""",
        {"d": "#d2d2d2", "l": "#ffffff"},
        "#ececec",
    )


@texture("block/grass_block_side.png")
def grass_side():
    c = tile(DIRT_MAP, DIRT, DIRT["."])
    # How far the turf hangs down each column: chunky drips, never a straight edge.
    depth = [4, 4, 3, 3, 4, 5, 5, 4, 3, 3, 4, 4, 6, 5, 4, 3]
    for x, d in enumerate(depth):
        c.rect(x, 0, 1, d, GRASS)
        c.rect(x, d - 1, 1, 1, GRASS_DARK)
    # Lit blades along the top, leaving the corner pixel the pure turf colour.
    for x in (3, 4, 9, 13):
        c.rect(x, 1, 1, 1, GRASS_LIGHT)
    return c


@texture("block/sand.png")
def sand():
    return tile(
        """
................
...d.........l..
................
.......l........
.l..........d...
.....d..........
...........l....
..l.............
........d.......
.............d..
...l............
..........l.....
.d..............
.......d........
............l...
....l...........
""",
        {"d": "#d8c084", "l": "#f6e9bd"},
        "#ead8a0",
    )


@texture("block/moss_block.png")
def moss():
    return tile(
        """
...ll...........
..llll.....d....
...l.......dd...
.........l......
.d......lll.....
.dd......l....l.
..............ll
l...ll..........
...llll.....d...
....ll......dd..
.........l......
..d.....lll.....
..dd.....l..ll..
...........llll.
.....d......ll..
.....dd.........
""",
        {"d": "#4b7624", "l": "#73a537"},
        "#5f8e2d",
    )


# -- stone --------------------------------------------------------------------------
STONE = {".": "#8d8d8d", "d": "#787878", "l": "#a1a1a1"}
STONE_MAP = """
................
.lll.......dd...
.....dddd.......
................
..........llll..
.ll.............
......ddd.......
................
...lll......ddd.
........ll......
................
.dddd...........
.........llll...
....ll..........
..............d.
.....ddd........
"""
DEEPSLATE = {".": "#505056", "d": "#3e3e44", "l": "#616168"}
DEEPSLATE_MAP = """
................
..ddddddd.......
...........llll.
lll.............
.......dddddd...
................
..llll..........
...........ddddd
dd..............
.....lllll......
................
.dddd.......ddd.
................
.........llll...
...ddddd........
................
"""


@texture("block/stone.png")
def stone():
    return tile(STONE_MAP, STONE, STONE["."])


@texture("block/deepslate.png")
def deepslate():
    return tile(DEEPSLATE_MAP, DEEPSLATE, DEEPSLATE["."])


COBBLE = (8, 11, ("#989898", "#8d8d8d", "#a2a2a2"), "#676767", "#adadad")


def cobbled(n, seed, tones, seam, rim, size=16, squash=1.0):
    """Stones in a bed of mortar: one tone a stone, a dark seam below and to the right
    of each, a lit rim inside its top and left edges."""
    labels, _ = cells(n, seed, size, squash=squash)
    c = Canvas(size, size)
    pick = np.random.default_rng(seed + 1).integers(0, len(tones), n)
    for k in range(n):
        c.where(labels == k, tones[pick[k]])
    s, lit = edges(labels)
    c.where(lit, rim)
    c.where(s, seam)
    return c, labels


@texture("block/cobblestone.png")
def cobblestone():
    c, _ = cobbled(*COBBLE)
    return c


@texture("block/mossy_cobblestone.png")
def mossy_cobblestone():
    # Moss takes whole stones, and creeps from them into the mortar around.
    c, labels = cobbled(*COBBLE)
    mossy = np.isin(labels, (3, 6))  # two stones apart, one at each side
    seam, lit = edges(labels)
    c.where((labels == 7) & lit, "#7fac40")  # and a third just starting
    c.where(mossy, "#6a9534")
    c.where(mossy & lit, "#7fac40")
    near = (np.roll(mossy, 1, 0) | np.roll(mossy, -1, 0) | np.roll(mossy, 1, 1) | np.roll(mossy, -1, 1)) & seam
    c.where(near | (mossy & seam), "#4e7426")
    return c


@texture("block/gravel.png")
def gravel():
    # Many small pebbles, browns and greys, barely seamed.
    c, _ = cobbled(22, 7, ("#968f8b", "#857f7b", "#a8a19d", "#7c7673", "#9b928a"), "#736d69", "#a9a39f")
    return c


@texture("block/andesite.png")
def andesite():
    return tile(
        """
..lll...........
.lllll......dd..
..lll......dddd.
............dd..
......d.........
.....ddd...lll..
......d...lllll.
...........lll..
.dd.............
dddd.....dd.....
.dd.....dddd....
.........dd...l.
....lll.......ll
...lllll......l.
....lll.........
................
""",
        {"d": "#848481", "l": "#b2b2ae"},
        "#9b9b98",
    )


@texture("block/tuff.png")
def tuff():
    # Grey-green stone laid down in flat, wide lumps.
    c, _ = cobbled(9, 3, ("#6e7169", "#666962", "#777a71"), "#55584f", "#868a80", squash=2.2)
    return c


@texture("block/deepslate_top.png")
def deepslate_top():
    c, _ = cobbled(7, 3, ("#505056", "#58585e", "#4a4a50"), "#38383d", "#64646b")
    return c


@texture("block/bedrock.png")
def bedrock():
    c, _ = cobbled(18, 21, ("#3a3a3a", "#575757", "#727272", "#8a8a8a", "#4a4a4a"), "#2a2a2a", "#7e7e7e")
    return c


# -- ores ---------------------------------------------------------------------------
# Each ore has its own shape, so they tell apart at a glance even in the dark:
# (pixel map, colours, spots it is laid at). 'a' dark, 'b' body, 'c' lit.
ORES = {
    "coal": (
        """
................
...bb...........
..bbbc......aa..
..abb......abba.
...a.......bbc..
............a...
.......bb.......
......bbbc......
......abb.......
.bb.............
abbc........bb..
.ab........bbbc.
..........abb...
....bb.....a....
...bbbc.........
....ab..........
""",
        {"a": "#1d1d21", "b": "#2c2c31", "c": "#4b4b53"},
    ),
    "iron": (
        """
................
..bbb.......cb..
.bccbb.....bbba.
..bbba......aa..
...aa...........
.........bb.....
........bcbb....
....b...bbba....
...bcb...aa.....
...bba..........
....a.......bb..
...........bcba.
..bb........ba..
.bcbb...........
..bba....bc.....
...a.....ba.....
""",
        {"a": "#b37a58", "b": "#d6a07a", "c": "#efc8a4"},
    ),
    "copper": (
        """
................
..be.......bb...
.bcbe.....bcbe..
..eba......eba..
...a............
.......ebb......
......bcbea.....
.......ea.......
..bb............
.bcbe.......be..
..eba......bcb..
...a........ea..
.......bb.......
......ecbe......
.......ba.......
................
""",
        {"a": "#a24e2c", "b": "#df7b4d", "c": "#f6a87c", "e": "#5cc6a0"},
    ),
    "gold": (
        """
................
...c........bc..
..bcb......bbba.
..bbba......aa..
...aa...........
........c.......
.......bcb......
.......bbba.....
..c.....aa......
.bcb............
.bbba.......c...
..aa.......bcb..
...........bbba.
......c.....aa..
.....bbba.......
......aa........
""",
        {"a": "#d38c18", "b": "#f7c334", "c": "#fff09a"},
    ),
    "redstone": (
        """
................
....bcb.........
...bbbba....bb..
....aa.....bcba.
............a...
..bb............
.bcbb.....bcb...
..abba...bbbba..
...........aa...
................
.......bcb......
......bbbba..bb.
.bb.....aa..bcb.
bcba.........a..
.a..............
................
""",
        {"a": "#9c130e", "b": "#e2261e", "c": "#ff6c58"},
    ),
    "lapis": (
        """
................
..cb.....b......
.bbba...bcb..cb.
..a......ab.bba.
......b......a..
.....bcb........
..b...ab...cb...
.bcb......bbba..
..ab.......a....
.......cb.......
......bbba...b..
.cb....a....bcb.
bbba.........ab.
.a.....b........
......bcb...cb..
.......a...bba..
""",
        {"a": "#1b3897", "b": "#2f5fd0", "c": "#6d95f6"},
    ),
    "diamond": (
        """
................
..cb............
.cbba......cb...
..ba......cbba..
...........ba...
.....cb.........
....cbba........
.....ba....cb...
..........cbba..
...cb......ba...
..cbba..........
...ba....cb.....
........cbba....
.........ba..cb.
............cbba
.............ba.
""",
        {"a": "#1b9a94", "b": "#40d9cf", "c": "#bafff8"},
    ),
}


@texture(*[f"block/{ore}_ore.png" for ore in ORES], *[f"block/deepslate_{ore}_ore.png" for ore in ("diamond", "gold", "redstone")])
def ore(path):
    name = path.split("/")[-1][: -len("_ore.png")]
    deep = name.startswith("deepslate_")
    art, legend = ORES[name.removeprefix("deepslate_")]
    base = tile(DEEPSLATE_MAP, DEEPSLATE, DEEPSLATE["."]) if deep else tile(STONE_MAP, STONE, STONE["."])
    return base.draw(art, legend)


# -- wood ---------------------------------------------------------------------------
BARK_MAP = """
.d..l..d...d.l..
.d..l..d...d.l..
.d.....d...d....
.d..d..d.l.d..d.
....d..d.l....d.
.l..d....l.d..d.
.l..d..l...d....
.d.....l...d.l..
.d..l..l.d...l..
.d..l....d.d.l..
.d..l..d.d.d....
....d..d...d..d.
.l..d..d...l..d.
.l..d..d.d.l..d.
.d.....d.d......
.d..l..d.d.d.l..
"""


def rings(bark, light, dark, core):
    """A log's end: a ring of bark, then growth rings, squarish as the game's are."""
    c = Canvas(fill=light)
    yy, xx = np.mgrid[0:16, 0:16] + 0.5
    dx, dy = np.abs(xx - 8), np.abs(yy - 8)
    r = np.maximum(dx, dy) * 0.7 + np.hypot(dx, dy) * 0.3
    c.where((r > 2.6) & (r < 3.6), dark)
    c.where((r > 5.0) & (r < 6.0), dark)
    c.where(r <= 1.6, core)
    edge = np.maximum(dx, dy) > 7
    c.where(edge, bark)
    return c


@texture("block/oak_log.png")
def oak_log():
    return tile(BARK_MAP, {"d": "#5e4124", "l": "#9a6e41"}, "#7e5a33")


@texture("block/oak_log_top.png")
def oak_log_top():
    return rings("#5e4124", "#bd955a", "#a47c42", "#8f6a36")


@texture("block/spruce_log.png")
def spruce_log():
    return tile(BARK_MAP, {"d": "#3f2a16", "l": "#6b4b2a"}, "#553a20")


@texture("block/spruce_log_top.png")
def spruce_log_top():
    return rings("#3f2a16", "#8c693c", "#755631", "#634827")


PLANK = {"h": "#c79d60", ".": "#b88d52", "g": "#a57c44", "s": "#7e5b32"}
PLANKS_MAP = """
hhhhhhhhhhhhhhhh
.......s........
...ggg.s........
ssssssssssssssss
hhhhhhhhhhhhhhhh
...............s
..........ggg..s
ssssssssssssssss
hhhhhhhhhhhhhhhh
...s............
...s.....ggg....
ssssssssssssssss
hhhhhhhhhhhhhhhh
...........s....
.gg........s....
ssssssssssssssss
"""


@texture("block/oak_planks.png")
def oak_planks():
    return tile(PLANKS_MAP, PLANK, PLANK["."])


# -- leaves -------------------------------------------------------------------------
@texture("block/oak_leaves.png")
def oak_leaves():
    # Grey, tinted by the toolkit (orange, amber, red, green...): clumps of leaves,
    # each lit at its top left and shaded below.
    return tile(
        """
.ll....d...lll..
llll..dd..lllll.
.lldd.....llldd.
..dd...ll..ddd..
d.....llll......
dd.....lldd...ll
......ldd....lll
.lll...d....llld
lllll.......ldd.
.llldd...ll..d..
..ddd...llll....
.......llllld...
..ll....lldd..ll
.llll....d...lll
.lllld......llld
..ddd.......ddd.
""",
        {"l": "#f4f4f4", "d": "#a9a9a9"},
        "#d4d4d4",
    )


@texture("block/flowering_azalea_leaves.png")
def flowering_azalea_leaves():
    c = oak_leaves()
    c.px[..., :3] *= rgb("#6c9c34")[:3] / 0.83  # green, painted in rather than tinted
    flower = """
.c.
cfc
.c.
"""
    return stamp(c, flower, {"c": "#e86cc8", "f": "#ffd6f4"}, [(1, 2), (10, 1), (6, 8), (12, 11), (2, 12)])


@texture("block/cherry_leaves.png")
def cherry_leaves():
    return tile(
        """
.ll....d...lll..
llwl..dd..lllwl.
.lldd.....llldd.
..dd...ll..ddd..
d.....lwll......
dd.....lldd...ll
......ldd....lwl
.lll...d....llld
lwlll.......ldd.
.llldd...ll..d..
..ddd...llwl....
.......llllld...
..ll....lldd..ll
.lwll....d...lll
.lllld......lwld
..ddd.......ddd.
""",
        {"l": "#f8c4dc", "w": "#fff0f6", "d": "#d67fa7"},
        "#eea2c4",
    )


@texture("block/end_stone.png")
def end_stone():
    return tile(
        """
................
..ll.......dd...
.llll......dd...
..ll............
.......dd.....l.
......dddd...lll
.......dd.....l.
.ll.............
llll......ll....
.ll......llll...
..........ll....
...dd...........
..dddd.......dd.
...dd........dd.
.........l......
........lll.....
""",
        {"d": "#c4c088", "l": "#f0eebe"},
        "#dedba2",
    )
