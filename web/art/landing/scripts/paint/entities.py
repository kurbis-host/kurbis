"""Things made of boxes: the chest, the cart, the sign, the strider, the salmon, the two
players, and the toast's card.

They are painted in the game's box unwrap (see `box`), which is how the models in
kurbis/boxmodel.py read them. Only the parts those models use are painted; the rest
stays clear."""

from . import CLEAR, Canvas, box, texture


def fill_face(c, rect, rows):
    """Paint a face row by row from the top: `rows` is a list of colours, the last one
    repeated down to the bottom."""
    x, y, w, h = rect
    for j in range(h):
        c.rect(x, y + j, w, 1, rows[min(j, len(rows) - 1)])


def frame(c, rect, dark, light=None):
    """A dark rim round a face, with a lit line inside its top and left edges."""
    x, y, w, h = rect
    if light is not None and w > 2 and h > 2:
        c.rect(x + 1, y + 1, w - 2, 1, light).rect(x + 1, y + 1, 1, h - 2, light)
    c.rect(x, y, w, 1, dark).rect(x, y + h - 1, w, 1, dark).rect(x, y, 1, h, dark).rect(x + w - 1, y, 1, h, dark)


# -- chests -----------------------------------------------------------------------
CHEST = {"wood": "#b67a34", "board": "#a26a28", "light": "#cf9446", "rim": "#5c3a17", "inside": "#4a2e16", "iron": "#c8c9d1", "ironk": "#5a5b65"}
# From December 24 to 26 every chest is a present: red paper, a gold ribbon round it.
PRESENT = {"wood": "#c8282e", "board": "#b42228", "light": "#e24a4a", "rim": "#7a1418", "inside": "#4a2e16", "iron": "#ffd75a", "ironk": "#c48a1c", "ribbon": "#f2c040"}


def chest_side(c, rect, look):
    x, y, w, h = rect
    c.rect(x, y, w, h, look["wood"])
    for j in range(2, h - 1, 3):
        c.rect(x + 1, y + j, w - 2, 1, look["board"])
    frame(c, rect, look["rim"], look["light"])
    if "ribbon" in look:
        c.rect(x + w // 2 - 1, y, 2, h, look["ribbon"])


def chest(look):
    c = Canvas(64, 64)
    lid, base = box(0, 0, 14, 14, 5), box(0, 19, 14, 14, 10)
    # The model is drawn upside down (as the game's is): the lid's outside is the
    # region the unwrap calls its bottom, and the base's inside is the one it calls top.
    for name in ("right", "front", "left", "back"):
        chest_side(c, lid[name], look)
        chest_side(c, base[name], look)
    chest_side(c, lid["bottom"], look)
    chest_side(c, base["bottom"], look)
    if "ribbon" in look:
        x, y, w, h = lid["bottom"]
        c.rect(x, y + h // 2 - 1, w, 2, look["ribbon"])  # the ribbon crosses on the lid
    for rect in (lid["top"], base["top"]):
        x, y, w, h = rect
        c.rect(x, y, w, h, look["inside"])
        frame(c, rect, look["rim"])
    # The latch, a small box at the corner of the picture.
    c.rect(0, 0, 6, 5, look["iron"])
    c.rect(0, 4, 6, 1, look["ironk"]).rect(3, 0, 3, 1, look["ironk"])
    c.rect(1, 2, 2, 1, look["ironk"])
    return c


@texture("entity/chest/normal.png")
def chest_normal():
    return chest(CHEST)


@texture("entity/chest/christmas.png")
def chest_christmas():
    return chest(PRESENT)


# -- minecart ---------------------------------------------------------------------
@texture("entity/minecart/minecart.png")
def minecart():
    c = Canvas(64, 32)
    plate, dark, light, rivet = "#8a8c96", "#5c5e68", "#b2b4be", "#d6d8e0"
    c.rect(2, 12, 20, 16, "#6e7078")  # the floor, seen from inside
    for y in range(15, 28, 4):
        c.rect(3, y, 18, 1, "#62646c")
    frame(c, (2, 12, 20, 16), dark)
    c.rect(24, 12, 20, 16, "#55575f")  # underneath
    frame(c, (24, 12, 20, 16), "#42444b")
    for rect in ((2, 10, 20, 2), (22, 10, 20, 2), (0, 12, 2, 16), (22, 12, 2, 16), (0, 2, 2, 8)):
        c.rect(*rect, dark)
    c.rect(2, 0, 16, 2, light)  # the rim
    c.rect(2, 1, 16, 1, "#9a9ca6")
    c.rect(2, 2, 16, 8, "#74767f")  # a wall, inside
    for x in (6, 10, 14):
        c.rect(x, 2, 1, 8, "#686a72")
    c.rect(20, 2, 16, 8, plate)  # a wall, outside
    frame(c, (20, 2, 16, 8), dark, light)
    c.rect(21, 5, 14, 1, dark)
    for x in (22, 27, 32):
        c.rect(x, 3, 1, 1, rivet).rect(x, 7, 1, 1, rivet)
    return c


# -- sign -------------------------------------------------------------------------
@texture("entity/signs/oak.png")
def sign():
    c = Canvas(64, 32)
    board = box(0, 0, 24, 2, 12)
    for name in ("front", "back"):
        x, y, _, _ = board[name]
        fill_face(c, board[name], ["#d9b577", "#c9a061", "#c9a061", "#a07a44"] * 3)
        c.rect(x + 9, y, 1, 4, "#a07a44").rect(x + 17, y + 4, 1, 4, "#a07a44").rect(x + 5, y + 8, 1, 4, "#a07a44")
    for name in ("top", "bottom", "right", "left"):
        c.rect(*board[name], "#a07a44")
    post = box(0, 14, 2, 2, 14)
    for name, rect in post.items():
        c.rect(*rect, "#7e5a33" if name in ("front", "right", "top") else "#5e4124")
    return c


# -- strider ----------------------------------------------------------------------
@texture("entity/strider/strider.png")
def strider():
    c = Canvas(64, 128)
    red, dark, light = "#b8383a", "#8e2a2e", "#d4504c"
    body = box(0, 0, 16, 16, 14)
    for name, rect in body.items():
        x, y, w, h = rect
        c.rect(*rect, red)
        if name in ("top", "bottom"):
            for k in range(0, 16, 5):
                c.rect(x + k, y + (k * 3) % 13, 2, 2, dark)
            continue
        for j in (3, 7, 11):  # folds of skin
            for i in range(w):
                if (i + j) % 6 < 4:
                    c.rect(x + i, y + j, 1, 1, dark)
        c.rect(x, y, w, 1, light)
    x, y, w, h = body["front"]
    c.rect(x + 3, y + 4, 3, 2, "#2a0e10").rect(x + 10, y + 4, 3, 2, "#2a0e10")
    c.rect(x + 3, y + 4, 1, 1, "#ffd0b0").rect(x + 10, y + 4, 1, 1, "#ffd0b0")
    c.rect(x + 4, y + 9, 8, 1, "#5e1a1e")
    for v in (32, 55):
        for name, rect in box(0, v, 4, 4, 16).items():
            x, y, w, h = rect
            c.rect(*rect, "#7a2e3e")
            c.rect(x, y, 1, h, "#94404f")
            for j in range(3, h, 4):
                c.rect(x, y + j, w, 1, "#4e1e2a")
    return c


# -- salmon -----------------------------------------------------------------------
FLANK = ["#7e2a22", "#c24e36", "#c24e36", "#e07a54", "#f0b08a"]


@texture("entity/fish/salmon.png")
def salmon():
    c = Canvas(32, 32)
    for v in (0, 13):
        faces = box(0, v, 3, 8, 5)
        for name in ("right", "left", "front", "back"):
            fill_face(c, faces[name], FLANK)
        c.rect(*faces["top"], FLANK[0])
        c.rect(*faces["bottom"], FLANK[-1])
        for name in ("right", "left"):
            x, y, w, _ = faces[name]
            for i in range(1, w, 3):
                c.rect(x + i, y + 1 + (i % 2), 1, 1, "#7e2a22")  # spots
    head = box(22, 0, 2, 3, 4)
    for name in ("right", "left", "front", "back"):
        fill_face(c, head[name], FLANK[:1] + FLANK[2:])
    c.rect(*head["top"], FLANK[0])
    c.rect(*head["bottom"], FLANK[-1])
    for name in ("right", "left"):
        x, y, _, _ = head[name]
        c.rect(x + 1, y + 1, 1, 1, "#14100e")  # an eye
    return c


# -- players ----------------------------------------------------------------------
def limb(c, faces, rows, top, bottom):
    for name in ("right", "front", "left", "back"):
        fill_face(c, faces[name], rows)
    c.rect(*faces["top"], top)
    c.rect(*faces["bottom"], bottom)


def player(look, slim):
    """A skin in the game's layout (64 by 64). The second layer stays clear."""
    c = Canvas(64, 64)
    L = look
    arm = 3 if slim else 4
    head = box(0, 0, 8, 8, 8)
    for name in ("right", "left", "back", "front"):
        c.rect(*head[name], L["skin"])
    c.rect(*head["top"], L["hair"])
    c.rect(*head["bottom"], L["skin_dark"])
    c.rect(*head["back"], L["hair"])
    side = L["side"].strip("\n").split("\n")
    c.draw("\n".join(side), L["legend"], *head["right"][:2])  # its back is to the left
    c.draw("\n".join(row[::-1] for row in side), L["legend"], *head["left"][:2])  # and this one's to the right
    c.draw(L["face"], L["legend"], *head["front"][:2])

    body = box(16, 16, 8, 4, 12)
    limb(c, body, L["shirt_rows"], L["shirt"], L["pants"])
    c.draw(L["chest"], L["legend"], *body["front"][:2])
    if "back" in L:
        c.draw(L["back"], L["legend"], *body["back"][:2])

    sleeve = [L["shirt_light"]] + [L["shirt"]] * 3 + [L["skin"]] * 7 + [L["skin_dark"]]
    for u, v in ((40, 16), (32, 48)):
        limb(c, box(u, v, arm, 4, 12), sleeve, L["shirt"], L["skin"])
    leg = [L["pants"]] * 9 + [L["pants_dark"]] + [L["shoe"]] * 2
    for u, v in ((0, 16), (16, 48)):
        limb(c, box(u, v, 4, 4, 12), leg, L["pants"], L["shoe"])
    return c


STEVE = {
    "skin": "#c68e6c",
    "skin_dark": "#ad7756",
    "hair": "#4a2f1e",
    "shirt": "#2ea3a6",
    "shirt_light": "#45bcbe",
    "shirt_rows": ["#45bcbe"] + ["#2ea3a6"] * 10 + ["#23878a"],
    "pants": "#3e3aa0",
    "pants_dark": "#302d86",
    "shoe": "#56565e",
    "legend": {"h": "#4a2f1e", "H": "#5e3c26", "s": "#c68e6c", "d": "#ad7756", "w": "#f4f4f4", "e": "#3a52b4", "m": "#6b3f2a", "n": "#a26a4c", "t": "#23878a"},
    "face": """
hHhhhhHh
hhhhhhhh
hsssssdh
ssssssss
swessews
sssnnsss
ssmmmmss
sshhhhss
""",
    "side": """
hhhhhhhh
hhhhhhhs
hhhsssss
hhssssss
hsssssss
hsssssss
hsssssss
hsssssss
""",
    "chest": """
...ss...
....s...
""",
}

ALEX = {
    "skin": "#f2c8a0",
    "skin_dark": "#e0ae86",
    "hair": "#e2822a",
    "shirt": "#6aaa4a",
    "shirt_light": "#82c260",
    "shirt_rows": ["#82c260"] + ["#6aaa4a"] * 10 + ["#4f8a35"],
    "pants": "#8a5a34",
    "pants_dark": "#6e4628",
    "shoe": "#4a3424",
    "legend": {"h": "#e2822a", "H": "#f39b40", "k": "#c46a1c", "s": "#f2c8a0", "d": "#e0ae86", "w": "#f4f4f4", "e": "#3f9a4a", "m": "#d48474"},
    "face": """
hHhhhHhh
hhhhhhhh
hhssshhh
hsssssdh
hweswesh
hsssssdh
hssmmssh
hsssssdk
""",
    "side": """
hhhhhhhh
hhhhhhhH
hhhhhhhs
hhhhhhss
hhhhhsss
hhhhhsss
khhhhsss
kkhhhhss
""",
    "chest": """
..ssss..
...ss...
""",
    "back": """
hhhhhhhh
hhhhhhhh
hHhhhhHh
khhhhhhk
.kkhhkk.
""",
}


@texture("entity/player/wide/steve.png")
def steve():
    return player(STEVE, slim=False)


@texture("entity/player/slim/alex.png")
def alex():
    return player(ALEX, slim=True)


# -- the toast's card -------------------------------------------------------------
@texture("gui/sprites/toast/advancement.png")
def toast():
    """The toast's card, as the game's: a black outline with its corners rounded off, a
    grey border two pixels wide, and a near-black inside, rounded again within."""
    c = Canvas(160, 32, fill="#000000")
    c.rect(1, 1, 158, 30, "#555555")
    c.rect(3, 3, 154, 26, "#212121")
    for x, y in ((0, 0), (1, 0), (0, 1), (158, 0), (159, 0), (159, 1), (0, 30), (0, 31), (1, 31), (159, 30), (158, 31), (159, 31)):
        c.rect(x, y, 1, 1, CLEAR)
    for x, y in ((1, 1), (158, 1), (1, 30), (158, 30)):
        c.rect(x, y, 1, 1, "#000000")
    for x, y in ((3, 3), (156, 3), (3, 28), (156, 28)):
        c.rect(x, y, 1, 1, "#555555")
    return c


# -- the chicken jockey -----------------------------------------------------------
@texture("entity/chicken/chicken_temperate.png")
def chicken():
    """A white hen (kurbis/boxmodel.py `chicken`, a 64 by 32 picture)."""
    c = Canvas(64, 32)
    white, shade, light = "#f4f2ea", "#d9d5c8", "#ffffff"
    head = box(0, 0, 4, 3, 6)
    for name, rect in head.items():
        fill_face(c, rect, [light, white, white, white, white, shade])
    x, y, _, _ = head["front"]
    c.rect(x, y + 2, 1, 1, "#1a1a1a").rect(x + 3, y + 2, 1, 1, "#1a1a1a")  # its eyes
    for name in ("right", "left"):
        x, y, _, _ = head[name]
        c.rect(x + 1, y + 2, 1, 1, "#1a1a1a")
    for rect in box(14, 0, 4, 2, 2).values():  # the beak
        fill_face(c, rect, ["#f6b02a", "#d88a18"])
    for rect in box(14, 4, 2, 2, 2).values():  # the wattle
        c.rect(*rect, "#d6262a")
    body = box(0, 9, 6, 6, 8)
    for name, rect in body.items():
        x, y, w, h = rect
        c.rect(*rect, white)
        if name in ("right", "front", "left", "back"):
            c.rect(x, y + h - 2, w, 2, shade)  # the underside, lying down
            c.rect(x + 1, y + 2, 1, 1, shade).rect(x + w - 2, y + 4, 1, 1, shade)
    for rect in box(24, 13, 1, 6, 4).values():  # the wings
        fill_face(c, rect, [light, white, white, shade])
    # Each leg is a stick and a foot; the rest of its box is clear. The stick runs down
    # one back corner, the edge the right side (its first column) shares with the back
    # (its last), so it is one stick from wherever it is seen. The toes point forward.
    leg = box(26, 0, 3, 3, 5)
    x, y, _, h = leg["right"]
    c.rect(x, y, 1, h, "#e89a22")
    x, y, w, h = leg["back"]
    c.rect(x + w - 1, y, 1, h, "#d6861a")
    x, y, w, h = leg["front"]
    c.rect(x, y + h - 1, w, 1, "#e89a22")
    c.rect(*leg["bottom"], "#d6861a")
    return c


@texture("entity/zombie/zombie_baby.png")
def zombie_baby():
    """A baby zombie (kurbis/boxmodel.py `baby_zombie`, a 64 by 64 picture). Babies have
    their own small model since 26.1 (a head six pixels a side), so this is drawn at that
    size, as the game's is: no mouth, two wide-set eyes, a pale muzzle and a dark nose.
    Its hat layer stays clear."""
    c = Canvas(64, 64)
    skin, dark, pale, paler = "#5a9a46", "#477c36", "#73b05a", "#86c06a"
    head = box(3, 3, 6, 6, 6)
    for name, rect in head.items():
        c.rect(*rect, dark if name in ("top", "back") else skin)
    for name in ("right", "left"):
        x, y, w, _ = head[name]
        c.rect(x, y, w, 1, dark)
    x, y, _, _ = head["front"]
    c.draw(
        """
ssssss
splLps
sLLLLs
kkLLkk
splnps
ssssss
""",
        {"s": skin, "p": pale, "L": paler, "l": "#98cc7c", "k": "#1c1c1c", "n": "#3f6e30"},
        x,
        y,
    )
    shirt = ["#3fc0bf", "#2aa8a8", "#2aa8a8", "#1f8a8a", "#3b3a9e"]
    limb(c, box(16, 16, 4, 2, 5), shirt, "#2aa8a8", "#3b3a9e")
    x, y, _, _ = box(16, 16, 4, 2, 5)["front"]
    c.rect(x + 1, y + 1, 1, 1, "#56d6d4").rect(x + 2, y + 2, 1, 1, "#1f8a8a")
    arm = ["#2aa8a8", skin, skin, pale, dark]
    for u in (36, 28):
        limb(c, box(u, 16, 2, 2, 5), arm, "#2aa8a8", skin)
    leg = ["#3b3a9e", "#3b3a9e", "#302d86", "#5a5a62"]
    for u in (8, 0):
        limb(c, box(u, 16, 2, 2, 4), leg, "#3b3a9e", "#5a5a62")
    return c


# -- pictures the interface draws from skins --------------------------------------
@texture("entity/bed/red.png")
def bed():
    """A red bed. kurbis/hud.py `bed` draws its inventory picture from these regions:
    the two halves' tops (the head's begins with the pillow), their ends, their long
    sides (columns go up the side, from the bottom on the west and from the right on
    the east) and the legs."""
    c = Canvas(64, 64)
    red, red_dark, red_light, sheet, wood, wood_dark = "#c42a2a", "#9a1e22", "#dc4848", "#f2eee4", "#a8743c", "#7a5028"
    for x, y in ((6, 6), (6, 28)):  # the tops
        c.rect(x, y, 16, 16, red)
        for j in range(3, 16, 4):
            c.rect(x, y + j, 16, 1, red_dark)
        c.rect(x, y, 1, 16, red_light)
    c.rect(6, 6, 16, 7, sheet).rect(7, 7, 14, 5, "#ffffff").rect(6, 12, 16, 1, "#d6d0c2")  # the pillow, at the head
    for x, y in ((6, 0), (22, 22)):  # the ends: row 0 is the bottom
        fill_face(c, (x, y, 16, 6), [wood_dark, wood, sheet, red, red, red_light])
    for x, y, flip in ((0, 6, False), (22, 6, True), (0, 28, False), (22, 28, True)):  # the sides: columns go up
        cols = [wood_dark, wood, sheet, red, red, red_light]
        for i, colour in enumerate(cols[::-1] if flip else cols):
            c.rect(x + i, y, 1, 16, colour)
    c.rect(50, 3, 3, 3, wood_dark).rect(50, 3, 3, 1, wood)  # a leg
    return c


@texture("entity/shield/shield_base_nopattern.png")
def shield():
    """A plain shield (kurbis/hud.py `shield`): a plate 12 by 22 by 1 at (0, 0), its face
    at (1, 1) and its back at (14, 1), and a handle 2 by 6 by 6 at (26, 0)."""
    c = Canvas(64, 64)
    plate = box(0, 0, 12, 1, 22)
    for name, wood, seam in (("front", "#a87a44", "#8a6236"), ("back", "#8a6236", "#6e4c28")):
        x, y, w, h = plate[name]
        c.rect(x, y, w, h, wood)
        for i in (3, 6, 9):  # boards
            c.rect(x + i, y + 1, 1, h - 2, seam)
        frame(c, plate[name], "#5e6068", "#a9acb4")  # an iron rim
        c.rect(x + 1, y + 9, w - 2, 1, "#8e9098").rect(x + 1, y + 10, w - 2, 1, "#6a6c74")  # and a band across
    for name in ("top", "bottom", "right", "left"):
        c.rect(*plate[name], "#6a6c74")
    for rect in box(26, 0, 2, 6, 6).values():
        c.rect(*rect, "#5e4124")
    return c
