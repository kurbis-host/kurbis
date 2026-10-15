"""Made things: worktables, a furnace, shelves, barrels, bone, pumpkins, lamps, torches,
lanterns, the lever, rails, the spawner's cage, the cracks of digging, and the items
held or shown."""

import numpy as np

from . import Canvas, texture, tile
from .terrain import cobbled

WOOD = {"h": "#c79d60", "p": "#b88d52", "g": "#a57c44", "s": "#7e5b32", "k": "#5e4124"}


# -- worktables -------------------------------------------------------------------
@texture("block/crafting_table_top.png")
def crafting_table_top():
    # A round slab of wood on a dark square, the crafting grid in its middle.
    c = Canvas(fill="#4a2a14")
    yy, xx = np.mgrid[0:16, 0:16] + 0.5
    dx, dy = np.abs(xx - 8), np.abs(yy - 8)
    inside = dx + dy <= 10.5
    c.where(inside, "#a8673c")
    c.where(inside & (dx + dy > 9.5), "#8c5230")
    c.where(inside & (dx + dy > 8.5) & (dx + dy <= 9.5) & (xx < 8) & (yy < 8), "#bd7a4a")
    for x, y in ((0, 0), (15, 0), (0, 15), (15, 15)):
        c.rect(x, y, 1, 1, "#c4935a")
    grid = (xx > 3) & (xx < 13) & (yy > 3) & (yy < 13)
    c.where(grid, "#c48d5c")
    lines = grid & ((np.isin(np.floor(xx), (3, 6, 9, 12))) | (np.isin(np.floor(yy), (3, 6, 9, 12))))
    c.where(lines | (grid & ((np.floor(xx) == 12) | (np.floor(yy) == 12))), "#5e3418")
    return c


@texture("block/crafting_table_side.png")
def crafting_table_side():
    # The table's edge, a post down the middle, and a saw hung on the left.
    return tile(
        """
kkkkkkkkkkkkkkkk
kdddddDPPDdddddk
kDDDDDDPPDDDDDDk
kllllllPPllllllk
kllhhllPPllllllk
kllwwllPPllllslk
kllwwglPPllllslk
kllwwllPPllllllk
kllwwglPPllllllk
kllwwllPPslllllk
kllgglsPPslllllk
kllllllPPllllllk
kslllllPPlllllsk
kllllllPPllllllk
kllllllPPllllllk
kllllllPPllllllk
""",
        TABLE,
        TABLE["l"],
    )


@texture("block/crafting_table_front.png")
def crafting_table_front():
    # A hammer on the left of the post, a saw on the right.
    return tile(
        """
kkkkkkkkkkkkkkkk
kdddddDPPDdddddk
kDDDDDDPPDDDDDDk
kllllllPPllllllk
klwwwllPPllhhllk
klgwgllPPllwwllk
kllhlllPPllwwglk
kllhlllPPllwwllk
kllhlllPPllwwglk
kllhlllPPllwwllk
kllhlllPPllgglsk
kslllllPPllllllk
kllllllPPlllllsk
kllllllPPllllllk
kllllllPPllllllk
kllllllPPllllllk
""",
        TABLE,
        TABLE["l"],
    )


# -- furnace ----------------------------------------------------------------------
FURNACE_STONE = {"l": "#b0b0b0", "s": "#5e5e5e", "f": "#9a9a9a", ".": "#8d8d8d", "d": "#787878"}


@texture("block/furnace_top.png")
def furnace_top():
    c, _ = cobbled(8, 5, ("#9c9c9c", "#939393", "#a4a4a4"), "#7a7a7a", "#ababab")
    c.rect(0, 0, 16, 1, "#6a6a6a").rect(0, 15, 16, 1, "#6a6a6a").rect(0, 0, 1, 16, "#6a6a6a").rect(15, 0, 1, 16, "#6a6a6a")
    return c


@texture("block/furnace_side.png")
def furnace_side():
    return tile(
        """
llllllllllllllll
l.............ds
l..lll.....dd..s
l.........dd...s
l.....dd.......s
l.ll...........s
l..........lll.s
ssssssssssssssss
ffffffffffffffff
ffffffffffffffff
f.....ffff....ff
ffffffffffffffff
ffff.......fffff
ffffffffffffffff
ffffffffffffffff
ssssssssssssssss
""",
        FURNACE_STONE,
        FURNACE_STONE["."],
    )


@texture("block/furnace_front_on.png")
def furnace_front_on():
    return tile(
        """
llllllllllllllll
l..............s
l..ssssssssss..s
l..skkkkkkkks..s
l..ssssssssss..s
l..............s
ssssssssssssssss
l..............s
l.ssssssssssss.s
l.skkkkkkkkkks.s
l.skkkyykykkks.s
l.skkyoyyoykks.s
l.skyoowoooyks.s
l.syoowwwooyys.s
l.ssssssssssss.s
ssssssssssssssss
""",
        {**FURNACE_STONE, "k": "#1e1a1a", "y": "#ffd24a", "o": "#ff8a1e", "w": "#fff2b0"},
        FURNACE_STONE["."],
    )


# -- shelves and barrels ----------------------------------------------------------
@texture("block/bookshelf.png")
def bookshelf():
    return tile(
        """
hhhhhhhhhhhhhhhh
gggggggggggggggg
kbbk.yrrkeggk.uk
kbbk.yrrkeggkuuk
kbbkcyrrkeggkuuk
kbbkcyrrkeggkuuk
kbbkcyrrkeggkuuk
ssssssssssssssss
hhhhhhhhhhhhhhhh
k.rrkgguk..ybbek
kurrkgguk.cybbek
kurrkggukccybbek
kurrkggukccybbek
kurrkggukccybbek
kurrkggukccybbek
ssssssssssssssss
""",
        {
            **WOOD,
            "k": "#3a2414",
            "b": "#2e5fa8",
            "y": "#e2b23a",
            "r": "#c0392b",
            "e": "#3e8e41",
            "g": "#7a4a28",
            "u": "#2a9d9a",
            "c": "#d8d0b8",
        },
        "#3a2414",
    )


BARREL = {".": "#8e6233", "a": "#80572c", "s": "#5a3b1c", "i": "#3a3a40", "j": "#5c5c64"}


@texture("block/barrel_side.png")
def barrel_side():
    return tile(
        """
...s...as...as..
...s...as...as..
jjjjjjjjjjjjjjjj
iiiiiiiiiiiiiiii
...s...as...as..
...s...as...as..
...s...as...as..
...s...as...as..
...s...as...as..
...s...as...as..
...s...as...as..
...s...as...as..
jjjjjjjjjjjjjjjj
iiiiiiiiiiiiiiii
...s...as...as..
...s...as...as..
""",
        BARREL,
        BARREL["."],
    )


@texture("block/barrel_top.png", "block/barrel_bottom.png")
def barrel_end(path):
    # Boards in a dark hoop; the top has its bung, a little to the left.
    c = Canvas(fill="#9e7240")
    for x in (4, 8, 12):
        c.rect(x, 1, 1, 14, "#80572c")
    for x in (1, 5, 9, 13):
        c.rect(x, 1, 1, 14, "#ad8250")
    c.rect(0, 0, 16, 1, "#5a3b1c").rect(0, 15, 16, 1, "#5a3b1c").rect(0, 0, 1, 16, "#5a3b1c").rect(15, 0, 1, 16, "#5a3b1c")
    c.rect(1, 14, 14, 1, "#80572c").rect(14, 1, 1, 14, "#80572c")
    if path.endswith("top.png"):
        c.rect(2, 7, 2, 2, "#4a4a52").rect(3, 8, 1, 1, "#2c2c30")
    else:
        c.px = c.px.transpose(1, 0, 2).copy()  # the bottom's boards run across
    return c


# -- bone -------------------------------------------------------------------------
@texture("block/bone_block_side.png")
def bone_side():
    return tile(
        """
................
.l.......l....d.
.l...d...l....d.
.l...d.......ld.
.....d...d...l..
.l.......d...l..
.l...l...d......
.l...l........d.
.....l...l....d.
.d.......l...l..
.d...d...l...l..
.d...d..........
.....d...d...l..
.l.......d...l..
.l...l...d......
................
""",
        {"d": "#d4ccae", "l": "#efe9d4"},
        "#e3dcc2",
    )


@texture("block/bone_block_top.png")
def bone_top():
    return tile(
        """
dddddddddddddddd
dllllllllllllllD
dl............dD
dl...mmmmmm...dD
dl..mmmmmmmm..dD
dl.mmmccccmmm.dD
dl.mmccccccmm.dD
dl.mmcckkccmm.dD
dl.mmcckkccmm.dD
dl.mmccccccmm.dD
dl.mmmccccmmm.dD
dl..mmmmmmmm..dD
dl...mmmmmm...dD
dl............dD
ddddddddddddddDD
DDDDDDDDDDDDDDDD
""",
        {"d": "#c9c0a0", "D": "#b3a985", "l": "#f1ecd8", "m": "#d6ccab", "c": "#c3b78f", "k": "#a99c72"},
        "#e3dcc2",
    )


# -- pumpkins and lamps -----------------------------------------------------------
PUMPKIN = {".": "#e88b20", "r": "#c56710", "l": "#f7a83e", "d": "#d47716"}
PUMPKIN_SIDE = """
d...ld.r...ld.r.
r..l...r..l...r.
r..l...r..l...r.
r..l..dr..l..dr.
r..l..dr..l..dr.
r..l..dr..l..dr.
r..l..dr..l..dr.
r..l..dr..l..dr.
r..l..dr..l..dr.
r..l..dr..l..dr.
r..l..dr..l..dr.
r..l..dr..l..dr.
r..l..dr..l..dr.
r..l...r..l...r.
r..l...r..l...r.
d...ld.r...ld.r.
"""


@texture("block/pumpkin_side.png")
def pumpkin_side():
    return tile(PUMPKIN_SIDE, PUMPKIN, PUMPKIN["."])


@texture("block/pumpkin_top.png")
def pumpkin_top():
    return tile(
        """
r......r.......r
.r.....r......r.
..r....r.....r..
...r...r....r...
....r..r...r....
.....r.rr.r.....
......wsstr.....
rrrrrrsSSsrrrrrr
......tssw......
.....r.rr.r.....
....r..r...r....
...r...r....r...
..r....r.....r..
.r.....r......r.
r......r.......r
.......r........
""",
        {**PUMPKIN, "s": "#6a5a22", "S": "#8a7a33", "t": "#4e421a", "w": "#a35210"},
        PUMPKIN["."],
    )


PUMPKIN_FACE = """
................
................
................
....y......y....
...yyy....yyy...
..yyyyy..yyyyy..
..ooooo..ooooo..
................
.y............y.
.yy..........yy.
.yyyyyyyyyyyyyy.
..yyy.yyyy.yyy..
...oo..oo..oo...
................
................
................
"""


@texture("block/jack_o_lantern.png")
def jack_o_lantern():
    c = tile(PUMPKIN_SIDE, PUMPKIN, PUMPKIN["."])
    # Three-cornered eyes and a toothy grin, lit from inside. Where it is worn on a head
    # the toolkit lets only the candle's yellow glow (`blocks.candle`: green over 0.7).
    return c.draw(PUMPKIN_FACE, {"y": "#ffe36a", "o": "#ffcc3a"})


@texture("block/redstone_lamp.png")
def redstone_lamp():
    return lamp("#3a2414", "#24150a", "#7a4a2a", "#96603a", "#8a5532")


@texture("block/redstone_lamp_on.png")
def redstone_lamp_on():
    return lamp("#5a3a1e", "#3a2410", "#f2a050", "#ffd28a", "#fff0c8")


# -- small lights and the lever ---------------------------------------------------
# These are cut from fixed places of a 16 px picture by the models (kurbis/boxmodel.py):
# the torch and the lever's stick are columns 7 and 8 from row 6 down, the lantern's
# body is columns 0-5 of rows 2-8, its top and bottom rows 9-14, its cap rows 0-1.
@texture("block/torch.png", "block/torch_e.png")
def torch(path):
    glow = path.endswith("_e.png")
    c = Canvas()
    c.draw(
        """
.......ww.......
.......yo.......
.......or.......
""",
        {"w": "#fff3b0", "y": "#ffd24a", "o": "#ff9a2a", "r": "#e0601a"},
        0,
        6,
    )
    if not glow:
        for y in range(9, 16):
            c.rect(7, y, 1, 1, "#9a6a3a").rect(8, y, 1, 1, "#6b4424")
    return c


@texture("block/lantern.png", "block/lantern_e.png")
def lantern(path):
    glow = path.endswith("_e.png")
    c = Canvas()
    iron = {"i": "#3a3a46", "j": "#5a5a68", "k": "#26262e"}
    light = {"g": "#ffc24a", "w": "#fff0b4"}
    if glow:
        iron = {key: None for key in iron}
    c.draw(
        """
.jjjj...........
.ikki...........
jjjjjj..........
iggggi..........
igwwgi..........
igwwgi..........
iggggi..........
iggggi..........
iiiiii..........
jjjjjj..........
jijjij..........
jjkkjj..........
jjkkjj..........
jijjij..........
jjjjjj..........
""",
        {**iron, **light},
    )
    return c


@texture("block/lever.png")
def lever():
    c = Canvas()
    c.rect(7, 6, 2, 2, "#c99a5c")
    for y in range(8, 16):
        c.rect(7, y, 1, 1, "#9a6a3a").rect(8, y, 1, 1, "#6b4424")
    return c


# -- rails ------------------------------------------------------------------------
RAIL = {"t": "#8c5c30", "T": "#6a4422", "m": "#c4c6d0", "n": "#868894", "b": "#4a4a52"}


@texture("block/rail.png")
def rail():
    return Canvas().draw(
        """
..mn........mn..
.tmnttttttttmnT.
.TbnTTTTTTTTbnT.
..mn........mn..
..mn........mn..
.tmnttttttttmnT.
.TbnTTTTTTTTbnT.
..mn........mn..
..mn........mn..
.tmnttttttttmnT.
.TbnTTTTTTTTbnT.
..mn........mn..
..mn........mn..
.tmnttttttttmnT.
.TbnTTTTTTTTbnT.
..mn........mn..
""",
        RAIL,
    )


@texture("block/rail_corner.png")
def rail_corner():
    """The rails turn from the bottom edge to the right one, about the bottom right
    corner: ties laid along the radius, then the two rails over them."""
    c = Canvas()
    yy, xx = np.mgrid[0:16, 0:16] + 0.5
    r = np.hypot(16 - xx, 16 - yy)
    a = np.degrees(np.arctan2(16 - yy, 16 - xx))
    for angle in (8, 30, 52, 74):
        tie = (np.abs(a - angle) < 6.5) & (r > 1.5) & (r < 15.5)
        c.where(tie, RAIL["t"])
        c.where(tie & (np.abs(a - angle) > 3.5) & (a > angle), RAIL["T"])
    for lo, hi in ((12.6, 13.6), (2.6, 3.6)):
        c.where((r >= lo) & (r < hi), RAIL["m"])
        c.where((r >= hi) & (r < hi + 0.9), RAIL["n"])
    return c


# -- the spawner's cage -----------------------------------------------------------
@texture("block/spawner.png")
def spawner():
    return Canvas().draw(
        """
kkkkkkkkkkkkkkkk
kjjjjjjjjjjjjjjk
kj..ij....ij..jk
kj..ij....ij..jk
kj..ij....ij..jk
kjiiwjiiiiwjiijk
kjjjjjjjjjjjjjjk
kj..ij....ij..jk
kj..ij....ij..jk
kjiiwjiiiiwjiijk
kjjjjjjjjjjjjjjk
kj..ij....ij..jk
kj..ij....ij..jk
kj..ij....ij..jk
kjjjjjjjjjjjjjjk
kkkkkkkkkkkkkkkk
""",
        {"k": "#1c242d", "j": "#3a4756", "i": "#5e7184", "w": "#9fb3c4"},
    )


# -- cracks -----------------------------------------------------------------------
# Lines that run out from the middle of the face; each pixel says at which of the ten
# stages it opens, so every stage keeps what the one before it showed.


@texture(*[f"block/destroy_stage_{i}.png" for i in range(10)])
def cracks(path):
    stage = int(path.split("_")[-1].split(".")[0])
    return Canvas().where(crack_stages() <= stage, "#1b1613")


# -- items ------------------------------------------------------------------------
@texture("item/diamond.png")
def diamond():
    return Canvas().draw(
        """
................
......kkkk......
....kkwwcbkk....
...kwwwccbbbk...
..kwwcccbbbbak..
..kwccccbbbbak..
.kcccccbbbbbbak.
.kccccbbbbbbbak.
.kbbbbbbbbbbaak.
.kddbbbbbbbbddk.
..kdddbbbbdddk..
..kdddddddddak..
...kkddddddkk...
.....kkkkkk.....
................
................
""",
        {"k": "#0f5450", "w": "#e8fffd", "c": "#9af5ec", "b": "#45d6cc", "a": "#2aa59e", "d": "#1e8c86"},
    )


@texture("item/iron_pickaxe.png")
def iron_pickaxe():
    # The head is drawn on one side of the handle's line and mirrored across it, so its
    # two arms are alike; the handle runs from the corner up that line.
    c = Canvas().draw(PICK_HEAD, {"k": "#2a2a30", "m": "#c8ccd4", "l": "#f0f2f6"})
    for y in range(16):
        for x in range(16):
            if x + y > 15:
                c.px[y, x] = c.px[15 - x, 15 - y]
    for x in range(1, 10):
        c.rect(x, 15 - x, 1, 1, "#a3743e").rect(x - 1, 15 - x, 1, 1, "#6b4424")
    return c


@texture("item/fishing_rod_cast.png")
def fishing_rod_cast():
    # No outline: the sprite becomes a row of little cubes in the hand.
    return Canvas().draw(
        """
................
..............w.
.............wn.
............wn..
...........wn...
..........wn....
.........wn.....
........wn......
.......wn.......
......wn........
.....wn.........
....wnmm........
...wn.mm........
..wn............
.wn.............
.n..............
""",
        {"w": "#a3743e", "n": "#6b4424", "m": "#b8bcc6"},
    )


@texture("item/ender_pearl.png")
def ender_pearl():
    # A dark eye in a sea-green rim: lit on its upper left, a glint low on the right.
    c = Canvas()
    yy, xx = np.mgrid[0:16, 0:16] + 0.5
    d = np.hypot(xx - 8, yy - 8)
    upper_left = (xx + yy) < 16
    c.where(d <= 6.9, "#0b2a26")
    c.where(d <= 6.0, "#2a8d7c")
    c.where((d <= 6.0) & (d > 4.6) & upper_left, "#4fb8a4")
    c.where(d <= 4.6, "#1d6458")
    c.where(d <= 3.4, "#0c2e2a")
    c.where(d <= 1.6, "#123c36")
    for x, y, colour in ((4, 4, "#a8f0e0"), (5, 3, "#a8f0e0"), (3, 5, "#7ad8c6"), (10, 11, "#7ad8c6"), (11, 10, "#4fb8a4"), (6, 6, "#2a8d7c")):
        c.rect(x, y, 1, 1, colour)
    return c


# -- carved pumpkins --------------------------------------------------------------
@texture("block/carved_pumpkin.png")
def carved_pumpkin():
    """The jack o'lantern's face, carved but unlit (worn on heads at Halloween)."""
    c = tile(PUMPKIN_SIDE, PUMPKIN, PUMPKIN["."])
    return c.draw(PUMPKIN_FACE, {"y": "#3a1806", "o": "#5e2a0c"})


# -- items shown in toasts and fished up ------------------------------------------
@texture("item/leather_boots.png", "item/leather_boots_overlay.png")
def leather_boots(path):
    """A pair of leather boots. The picture is grey and takes the leather's colour (the
    page tints it #a06540); the overlay is laid over it as it is: the feet, which the
    dye leaves alone."""
    if path.endswith("_overlay.png"):
        return pair(SOLE, {"o": "#3a2416", "F": "#8a5a36", "f": "#6b4429"}, 10)
    return pair(BOOT, {"k": "#5a5a5a", "w": "#ffffff", "m": "#c8c8c8"}, 3)


@texture("item/minecart.png")
def minecart_item():
    return Canvas().draw(
        """
................
................
................
....kkkkkkkk....
..kkddddddddkk..
.kddiiiiiiiiddk.
.kdiiiiiiiiiidk.
.kddiiiiiiiiddk.
.klkkkkkkkkkklk.
.kmlmmmmmmmmlmk.
.kmmnmmmmmmnmmk.
..kmmmmmmmmmmk..
..kkmkkkkkkmkk..
...kwk....kwk...
....k......k....
................
""",
        {"k": "#1e2024", "d": "#4a5058", "i": "#2a2e33", "l": "#b8bcc4", "m": "#8a8e96", "n": "#5a5e66", "w": "#3a3e44"},
    )


TABLE = {"k": "#4a2a14", "d": "#8a5a2c", "D": "#6e4622", "P": "#5a3418", "l": "#c49a62", "s": "#a07a44", "w": "#e8e8ea", "g": "#9a9ca4", "h": "#6b4424"}


def lamp(frame, seam, body, light, core):
    """The lamp's glass: a diamond in the middle and a quarter of one in each corner
    (they meet whole across neighbouring lamps), each in a dark seam on a dark frame."""
    c = Canvas(fill=frame)
    yy, xx = np.mgrid[0:16, 0:16] + 0.5
    middle = np.abs(xx - 8) + np.abs(yy - 8)
    corner = np.min([np.abs(xx - cx) + np.abs(yy - cy) for cx in (0, 16) for cy in (0, 16)], axis=0)
    for d, size in ((middle, 5.0), (corner, 4.0)):
        c.where((d > size) & (d <= size + 1.0), seam)
        c.where(d <= size, body)
        c.where((d <= size) & (d > size - 1.2) & (xx + yy < 16), light)
    c.where(middle <= 1.0, core)
    return c


def crack_stages():
    """For each pixel, the first of the ten stages it is cracked at (10: never): short
    strokes, mostly across, never touching side by side, the nearer the middle the sooner."""
    rng = np.random.default_rng(4)
    stage = np.full((16, 16), 10)
    steps = ((1, 0), (1, 0), (1, 0), (1, 1), (1, -1))
    for _ in range(400):
        r = rng.uniform(0, 1) ** 0.6 * 10.5
        a = rng.uniform(0, 2 * np.pi)
        x, y = 7.5 + r * np.cos(a), 7.5 + r * np.sin(a)
        dx, dy = steps[rng.integers(len(steps))]
        cells = [(round(x + i * dx), round(y + i * dy)) for i in range(int(rng.integers(2, 4)))]
        if not all(0 <= px < 16 and 0 <= py < 16 for px, py in cells):
            continue
        near = [(px + ox, py) for px, py in cells for ox in (-1, 0, 1)] + [(px, py + oy) for px, py in cells for oy in (-1, 1)]
        if any(0 <= nx < 16 and 0 <= ny < 16 and stage[ny, nx] < 10 for nx, ny in near):
            continue
        s = int(np.clip(r / 10.5 * 9.5 + rng.uniform(-1.5, 0.3), 0, 9))
        for px, py in cells:
            stage[py, px] = s
    return stage


PICK_HEAD = """
................
...kkkkkk.......
..kmmmmmlkk.....
...kkkkmlllk....
........kkmlk...
..........k.....
"""


BOOT = """
....kkk.
...kwmk.
...kwmk.
...kwmk.
...kwmk.
..kwmmk.
..kwmmk.
"""


SOLE = """
.oFFFfo.
oFFFFfo.
.oooooo.
"""


def pair(art, legend, y):
    """A left boot from a map, and the right one mirrored beside it."""
    c = Canvas().draw(art, legend, 0, y)
    c.px[:, 8:] = c.px[:, 7::-1]
    return c
