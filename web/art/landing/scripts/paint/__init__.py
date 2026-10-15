"""Kurbis's own textures, painted in code.

The look follows the trailer references: flat, simplified pixel art with few tones a
material, big readable shapes, a light edge on top and to the left and a dark one
below and to the right, the way a sunlit block reads in the trailers. Only what the
landing page draws is painted (see `paint.py`, which lists it).

Each texture is a function that returns a `Canvas`, registered under the path the
toolkit asks for (`block/dirt.png`, `entity/chest/normal.png`, ...). Hand-drawn ones
are pixel maps written as text, a character a pixel; patterned ones (stones, crystals,
crusts) are cells on a torus, so every block tile repeats without a seam. Every random
choice is seeded, so a run paints the same pixels every time.
"""

from pathlib import Path

import numpy as np
from PIL import Image

PAINTED = {}


def texture(*paths):
    """Register a painter for one or more paths. A painter for several paths gets the
    path as its argument."""

    def register(fn):
        for path in paths:
            PAINTED[path] = (fn, len(paths) > 1)
        return fn

    return register


def paint(path):
    fn, takes_path = PAINTED[path]
    return fn(path) if takes_path else fn()


def rgb(value, alpha=1.0):
    """'#rrggbb' as float RGBA."""
    value = value.lstrip("#")
    return np.array([int(value[i : i + 2], 16) / 255 for i in (0, 2, 4)] + [alpha], np.float32)


CLEAR = np.zeros(4, np.float32)


class Canvas:
    """RGBA floats, row 0 at the top (as the picture is drawn)."""

    def __init__(self, w=16, h=16, fill=None):
        self.px = np.zeros((h, w, 4), np.float32)
        if fill is not None:
            self.px[:] = rgb(fill) if isinstance(fill, str) else fill

    @property
    def w(self):
        return self.px.shape[1]

    @property
    def h(self):
        return self.px.shape[0]

    def copy(self):
        out = Canvas(self.w, self.h)
        out.px = self.px.copy()
        return out

    def rect(self, x, y, w, h, color):
        self.px[y : y + h, x : x + w] = rgb(color) if isinstance(color, str) else color
        return self

    def where(self, mask, color):
        self.px[mask] = rgb(color) if isinstance(color, str) else color
        return self

    def draw(self, art, legend, x=0, y=0, wrap=False):
        """Lay a text pixel map over the canvas. Characters missing from `legend` (and
        '.') leave the pixel as it is; a legend value of None clears it."""
        for j, row in enumerate(art.strip("\n").split("\n")):
            for i, ch in enumerate(row):
                if ch == "." or ch == " " or ch not in legend:
                    continue
                yy, xx = y + j, x + i
                if wrap:
                    yy, xx = yy % self.h, xx % self.w
                elif not (0 <= yy < self.h and 0 <= xx < self.w):
                    continue
                value = legend[ch]
                self.px[yy, xx] = CLEAR if value is None else (rgb(value) if isinstance(value, str) else value)
        return self

    def paste(self, other, x=0, y=0):
        """Copy another canvas in, keeping what is under its clear pixels."""
        h, w = other.h, other.w
        region = self.px[y : y + h, x : x + w]
        solid = other.px[: region.shape[0], : region.shape[1], 3] > 0
        region[solid] = other.px[: region.shape[0], : region.shape[1]][solid]
        return self

    def shade(self, mask, factor):
        """Darken (factor < 1) or lighten (> 1) the colour under a mask."""
        self.px[mask, :3] = np.clip(self.px[mask, :3] * factor, 0, 1)
        return self

    def save(self, path):
        path = Path(path)
        path.parent.mkdir(parents=True, exist_ok=True)
        data = np.round(np.clip(self.px, 0, 1) * 255).astype(np.uint8)
        data[data[..., 3] == 0] = 0  # clear pixels carry no colour
        Image.fromarray(data, "RGBA").save(path, optimize=True)


def tile(art, legend, base):
    """A 16 by 16 tile: a base colour with a pixel map over it."""
    rows = art.strip("\n").split("\n")
    if len(rows) != 16 or any(len(row) != 16 for row in rows):
        raise ValueError(f"a tile's map must be 16 by 16, not {[len(row) for row in rows]}")
    return Canvas(fill=base).draw(art, legend)


# -- patterns on a torus ----------------------------------------------------------
def cells(n, seed, size=16, relax=2, squash=1.0):
    """Voronoi cells that wrap at the edges: (labels, points). `relax` steps of Lloyd's
    method even their sizes out; `squash` > 1 stretches them sideways."""
    rng = np.random.default_rng(seed)
    pts = rng.uniform(0, size, (n, 2))
    yy, xx = np.mgrid[0:size, 0:size] + 0.5
    for step in range(relax + 1):
        dx = (xx[..., None] - pts[:, 0] + size / 2) % size - size / 2
        dy = (yy[..., None] - pts[:, 1] + size / 2) % size - size / 2
        dist = (dx / squash) ** 2 + dy**2
        labels = dist.argmin(axis=-1)
        if step == relax:
            break
        for k in range(n):
            mine = labels == k
            if mine.any():
                # the mean on a torus: of the offsets from the current point
                pts[k, 0] = (pts[k, 0] + dx[..., k][mine].mean()) % size
                pts[k, 1] = (pts[k, 1] + dy[..., k][mine].mean()) % size
    return labels, pts


def edges(labels):
    """Where a cell meets the next one below or to the right (a 1 px seam), and the
    pixels just inside a cell's top and left edges (its lit rim)."""
    right = np.roll(labels, -1, axis=1) != labels
    below = np.roll(labels, -1, axis=0) != labels
    seam = right | below
    above = np.roll(labels, 1, axis=0) != labels
    left = np.roll(labels, 1, axis=1) != labels
    lit = (above | left) & ~seam
    return seam, lit


def stamp(canvas, art, legend, spots):
    """The same small pixel map laid at several places, wrapping at the edges."""
    for x, y in spots:
        canvas.draw(art, legend, x, y, wrap=True)
    return canvas


def box(u, v, w, d, h):
    """The six faces of a cuboid in the game's box unwrap, as (x, y, w, h) rects."""
    return {
        "top": (u + d, v, w, d),
        "bottom": (u + d + w, v, w, d),
        "right": (u, v + d, d, h),
        "front": (u + d, v + d, w, h),
        "left": (u + d + w, v + d, d, h),
        "back": (u + d + w + d, v + d, w, h),
    }
