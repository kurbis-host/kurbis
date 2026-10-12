"""The HUD: a Minecraft-style interface drawn on cards fixed in front of the lens.

Cards are placed in interface pixels: the frame is 480 wide (270 high at 16:9)
with the origin at its bottom left, like the game's interface at a large scale.
Text is set in the small pixel face (font.py) and baked to images. A card's
opacity is its object colour's alpha, so cards fade and slide as plain object
animation, and the whole interface keeps its size whatever the lens does.
"""

import math

import bpy
import numpy as np

from . import anim, core, font

WIDTH, HEIGHT = 480, 270


def shape(tall=False):
    """Set the interface's size for a wide or a tall frame. Its pixels stay the same
    size on screen: the frame's long side is always 480 of them."""
    global WIDTH, HEIGHT, CHAT_BASE
    WIDTH, HEIGHT = (270, 480) if tall else (480, 270)
    CHAT_BASE = 28 if tall else 6  # the narrow frame has no room for chat beside the shortcut: it sits above it

WHITE, YELLOW, GREY, GREEN = (1.0, 1.0, 1.0), (1.0, 1.0, 0.33), (0.66, 0.66, 0.66), (0.33, 1.0, 0.33)
LINE = 10  # the height of a line of chat
CHAT_BASE = 6  # how far above the bottom edge the newest line sits
CHAT_LINES = 4  # how many lines of chat show at once
TOAST = (160, 32)


# -- drawing ---------------------------------------------------------------------
def text(string, color=WHITE, shadow=True):
    """Pixels of one line, (rows, columns, 4) with row 0 at the bottom. The drop shadow
    makes it one pixel wider and taller than the letters."""
    mask = font.pixel_mask(string)[::-1]
    rows, cols = mask.shape
    out = np.zeros((rows + 1, cols + 1, 4), np.float32)
    if shadow:
        out[:-1, 1:][mask] = (*(c * 0.25 for c in color), 1.0)
    out[1:, :-1][mask] = (*color, 1.0)
    return out


def panel(width, height, fill, border=None):
    px = np.zeros((height, width, 4), np.float32)
    px[...] = fill
    if border is not None:
        px[0], px[-1], px[:, 0], px[:, -1] = border, border, border, border
    return px


def paste(base, top, x, y):
    """Lay `top` over `base` with its bottom left at (x, y)."""
    h, w = top.shape[:2]
    region = base[y : y + h, x : x + w]
    alpha = top[: region.shape[0], : region.shape[1], 3:4]
    region[..., :3] = top[: region.shape[0], : region.shape[1], :3] * alpha + region[..., :3] * (1 - alpha)
    region[..., 3:4] = alpha + region[..., 3:4] * (1 - alpha)
    return base


def chat_line(string, color=WHITE):
    """One line of chat: the text on a see-through dark bar."""
    letters = text(string, color)
    return paste(panel(letters.shape[1] + 6, LINE, (0.0, 0.0, 0.0, 0.5)), letters, 3, 1)


def tooltip(title, lines=()):
    """A tooltip: a title and grey lines under it on the game's dark, purple-edged panel.
    Without a title (the thing has its name beside it already), the lines alone, in white."""
    rows = ([text(title)] if title else []) + [text(line, GREY if title else WHITE) for line in lines]
    width = max(r.shape[1] for r in rows) + 8
    height = len(rows) * LINE + 6 + (2 if lines and title else 0)
    px = panel(width, height, (0.063, 0.0, 0.063, 0.94))
    px[1, 1:-1] = px[-2, 1:-1] = px[1:-1, 1] = px[1:-1, -2] = (0.16, 0.0, 0.5, 1.0)
    px[0, 0] = px[0, -1] = px[-1, 0] = px[-1, -1] = (0.0, 0.0, 0.0, 0.0)
    y = height - 4 - LINE + 1
    for i, row in enumerate(rows):
        paste(px, row, 4, y)
        y -= LINE + (2 if i == 0 and title else 0)
    return px


def toast(icon, title, body, back=None):
    """A toast, 160 by 32: an icon, a yellow title and a white line."""
    px = back.copy() if back is not None else panel(160, 32, (0.13, 0.13, 0.13, 1.0), (0.33, 0.33, 0.33, 1.0))
    if icon is not None:
        paste(px, icon, 8, 8)
    paste(px, text(title, YELLOW), 30, 17)
    paste(px, text(body), 30, 6)
    return px


def button(icon, label):
    """A small button: an icon and a label on the game's grey."""
    letters = text(label)
    room = 24 if icon is not None else 9
    px = panel(letters.shape[1] + room + 6, 20, (0.42, 0.42, 0.42, 1.0), (0.0, 0.0, 0.0, 1.0))
    px[-2, 1:-1] = px[1:-1, 1] = (0.66, 0.66, 0.66, 1.0)  # lit from the top left
    px[1, 1:-1] = px[1:-1, -2] = (0.3, 0.3, 0.3, 1.0)
    if icon is not None:
        paste(px, icon, 4, 2)
    paste(px, letters, room, 6)
    return px


_ARROW = (
    "X          ", "XX         ", "XoX        ", "XooX       ", "XoooX      ", "XooooX     ", "XoooooX    ", "XooooooX   ", "XoooooooX  ",
    "XooooooooX ", "XoooooXXXXX", "XooXooX    ", "XoX XooX   ", "XX  XooX   ", "X    XooX  ", "     XooX  ", "      XX   ",
)


def arrow():
    """The pointer, for showing in the previs where a visitor's hand is."""
    px = np.zeros((len(_ARROW), len(_ARROW[0]), 4), np.float32)
    for r, row in enumerate(_ARROW[::-1]):
        for c, ch in enumerate(row):
            if ch != " ":
                px[r, c] = (0.0, 0.0, 0.0, 1.0) if ch == "X" else (1.0, 1.0, 1.0, 1.0)
    return px


# -- cards -----------------------------------------------------------------------
def _material(name, image):
    mat = bpy.data.materials.new(f"Hud_{name}")
    mat.use_nodes = True
    nt = mat.node_tree
    nt.nodes.clear()
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    tex = nt.nodes.new("ShaderNodeTexImage")
    tex.image = image
    tex.interpolation = "Closest"
    tex.extension = "CLIP"
    info = nt.nodes.new("ShaderNodeObjectInfo")
    both = nt.nodes.new("ShaderNodeMath")
    both.operation = "MULTIPLY"
    nt.links.new(tex.outputs["Alpha"], both.inputs[0])
    nt.links.new(info.outputs["Alpha"], both.inputs[1])
    clear = nt.nodes.new("ShaderNodeBsdfTransparent")
    light = nt.nodes.new("ShaderNodeEmission")
    nt.links.new(tex.outputs["Color"], light.inputs["Color"])
    mix = nt.nodes.new("ShaderNodeMixShader")
    nt.links.new(both.outputs[0], mix.inputs[0])
    nt.links.new(clear.outputs[0], mix.inputs[1])
    nt.links.new(light.outputs[0], mix.inputs[2])
    nt.links.new(mix.outputs[0], out.inputs["Surface"])
    mat.surface_render_method = "BLENDED"
    return mat


class Hud:
    def __init__(self, cam, dist=0.11):
        self.cam, self.dist = cam, dist
        self.root = core.empty("Hud", "FX", (0.0, 0.0, -dist), parent=cam)
        self.cards, self.said, self.toasts, self.pointer = 0, [], [], None
        (core.EXPORT / "hud").mkdir(parents=True, exist_ok=True)

    def card(self, name, pixels, at=(0.0, 0.0), anchor=(0.0, 0.0), shown=True, inside=None):
        """A card showing `pixels`, with its `anchor` (0..1 across and up) at `at`. With
        `inside`, it is moved as little as it takes to keep that many pixels of margin
        to every edge of the frame."""
        h, w = pixels.shape[:2]
        if inside is not None:
            at = (min(max(at[0], inside + anchor[0] * w), WIDTH - inside - (1.0 - anchor[0]) * w), min(max(at[1], inside + anchor[1] * h), HEIGHT - inside - (1.0 - anchor[1]) * h))
        image = core.image_from_pixels(f"hud_{name}", pixels, core.EXPORT / "hud" / f"{name}.png")
        x0, y0 = -anchor[0] * w, -anchor[1] * h
        quad = np.array([[(x0, y0, 0), (x0 + w, y0, 0), (x0 + w, y0 + h, 0), (x0, y0 + h, 0)]], np.float32)
        obj = bpy.data.objects.new(f"Hud_{name}", core.mesh_from_quads(f"Hud_{name}", quad, np.array([[(0, 0), (1, 0), (1, 1), (0, 1)]], np.float32)))
        obj.data.materials.append(_material(name, image))
        obj.parent = self.root
        self.cards += 1
        obj.location = self.place(at)
        obj.location.z = 0.2 * self.cards  # later cards lie a hair nearer the lens
        obj.color = (1.0, 1.0, 1.0, 1.0 if shown else 0.0)
        obj.visible_shadow = False
        core.link(obj, "FX")
        obj["size"] = (w, h)
        return obj

    def place(self, at):
        """Interface pixels (origin bottom left) to the card's place under the root."""
        return (at[0] - WIDTH / 2, at[1] - HEIGHT / 2, 0.0)

    # things that are said ------------------------------------------------------
    def say(self, start, string, color=WHITE, clock="scroll", stay=7.0, delay=0.0, until=None):
        """A line of chat that arrives `delay` seconds after the scroll gets to `start`
        (or after that moment, for clock="real"), pushes the older lines up, and fades
        after a while, or as soon as the scroll passes `until`: talk belongs to its place."""
        self.said.append((clock, start, self.card(f"chat_{len(self.said):02d}", chat_line(string, color), shown=False), stay, delay, until))

    def announce(self, start, name, pixels, clock="scroll", stay=3.0, until=None):
        """A toast: it slides in at the top right when the scroll gets to `start`, waits,
        and slides out. There is only ever one: a new one sends the old one off early,
        and so does the scroll passing `until`."""
        self.toasts.append((clock, start, self.card(f"toast_{name}", pixels, at=(WIDTH + 12, HEIGHT - 4), anchor=(0.0, 1.0)), stay, until))

    def fade(self, obj, start, stay=None, clock="scroll", rise=0.15, fall=0.4):
        """Triggered: a card fades in, and out again after `stay` seconds if given."""
        def alpha(dt):
            up = anim.clamp01(dt / rise)
            return (1.0, 1.0, 1.0, up if stay is None else up * (1.0 - anim.clamp01((dt - stay) / fall)))

        anim.TL.add(obj, "color", start, lambda s, dt: alpha(dt), clock)

    def slide(self, obj, start, at, offset, stay, clock="scroll", travel=0.35):
        """Triggered: a card slides in from `offset` away, waits, and slides out again."""
        z = obj.location.z

        def where(dt):
            away = 1.0 - anim.ease_out(dt / travel, 3.0) if dt < stay + travel else anim.ease_in((dt - stay - travel) / travel, 3.0)
            place = self.place((at[0] + offset[0] * away, at[1] + offset[1] * away))
            return (place[0], place[1], z)

        anim.TL.add(obj, "location", start, lambda s, dt: where(dt), clock)

    def point(self, keys, clicks=()):
        """The pointer follows `keys` [(moment, (x, y))] and dips at each of `clicks`."""
        self.pointer = (self.card("pointer", arrow(), anchor=(0.0, 1.0), shown=False), anim.Track(keys) if keys else None, sorted(keys)[0][0] if keys else 0.0, sorted(keys)[-1][0] if keys else 0.0, tuple(clicks))

    # finishing -------------------------------------------------------------------
    def bake(self, session, lens):
        """Give the interface its size on every frame, and lay out what is said."""
        anim.write(self.root, "scale", [(self.dist * 36.0 / (value * max(WIDTH, HEIGHT)),) * 3 for value in lens])  # the sensor's 36 mm lie along the long side
        def moment(clock, start, delay=0.0):
            if isinstance(clock, tuple):  # a scrubbed sequence: its seconds are scroll
                return session.when(start + delay / clock[1])
            return anim.begins(session, clock, start) + delay

        arrive = [(moment(clock, start, delay), card, stay if until is None else min(stay, max(session.when(until) - moment(clock, start, delay), 0.0))) for clock, start, card, stay, delay, until in self.said]
        rows = {card: ([], []) for _, card, _ in arrive}
        for f in range(session.frames):
            tau = f / anim.FPS
            for begun, card, stay in arrive:
                age = tau - begun
                later = sorted(other for other, _, _ in arrive if begun < other <= tau)  # each pushes this one up a line
                newer = len(later)
                risen = sum(anim.ease_out((tau - other) / 0.12, 2.0) for other in later)
                alpha = 0.0 if age < 0 else anim.clamp01(age / 0.1) * (1.0 - anim.clamp01((age - stay) / 0.5))
                if newer >= CHAT_LINES:  # crowded out
                    alpha *= 1.0 - anim.clamp01((tau - later[CHAT_LINES - 1]) / 0.2)
                place = self.place((4.0, CHAT_BASE + LINE * (risen if newer else 0.0)))
                rows[card][0].append((place[0], place[1], card.location.z))
                rows[card][1].append((1.0, 1.0, 1.0, alpha))
        for card, (where, colour) in rows.items():
            anim.write(card, "location", where)
            anim.write(card, "color", colour)
        # Toasts, one at a time: each leaves when its time is up or the next one is due.
        travel = 0.3
        due = sorted(((moment(clock, start), card, stay if until is None else max(min(stay, session.when(until) - moment(clock, start) - 2 * travel), 0.0)) for clock, start, card, stay, until in self.toasts), key=lambda toast: toast[0])
        for i, (begun, card, stay) in enumerate(due):
            if i + 1 < len(due):
                stay = max(min(stay, due[i + 1][0] - begun - 2 * travel), 0.0)
            where = []
            for f in range(session.frames):
                age = f / anim.FPS - begun
                away = 1.0 if age < 0 else (1.0 - anim.ease_out(age / travel, 3.0) if age < stay + travel else anim.ease_in((age - stay - travel) / travel, 3.0))
                place = self.place((WIDTH - 4 - TOAST[0] + (TOAST[0] + 16) * away, HEIGHT - 4))
                where.append((place[0], place[1], card.location.z))
            anim.write(card, "location", where)
        if self.pointer and self.pointer[1] is not None:
            card, track, first, last, clicks = self.pointer
            where, size, colour = [], [], []
            for f in range(session.frames):
                tau = f / anim.FPS
                at = track(tau)
                place = self.place((at[0], at[1]))
                press = max((1.0 - abs(tau - click - 0.05) / 0.09 for click in clicks), default=0.0)
                grow = 1.0 - 0.22 * max(press, 0.0)
                where.append((place[0], place[1], card.location.z))
                size.append((grow, grow, 1.0))
                colour.append((1.0, 1.0, 1.0, anim.clamp01((tau - first) / 0.2) * (1.0 - anim.clamp01((tau - last - 0.4) / 0.3))))
            anim.write(card, "location", where)
            anim.write(card, "scale", size)
            anim.write(card, "color", colour)


def project(shot, s, point, lens=None):
    """Where a point of the world is on the interface, for the camera at scroll position
    s: (x, y) in interface pixels, and how far in front of the lens it is."""
    state = shot(s)
    quat = anim.look_quat(state["eye"], state["target"], state["up"], state["roll"])
    local = quat.inverted() @ (point - state["eye"])
    scale = (lens or state["lens"]) / 36.0 * max(WIDTH, HEIGHT) / max(-local.z, 1e-6)
    return (WIDTH / 2 + local.x * scale, HEIGHT / 2 + local.y * scale), -local.z
