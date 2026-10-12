"""Sets that live apart from the valley: the Nether.

A set is its own block grid, built in local coordinates and placed far from the
valley. The film reaches it through a transition that covers the whole frame,
so where it sits in the world does not matter.
"""

import numpy as np

from . import voxel
from .world import _hash, _smooth, fbm2, noise2, noise3

NETHER = {
    "origin": (600.0, 0.0, -60.0),
    "ledge": 19,  # height of the ledge the portal stands on
    "portal_row": 14,
    "bridge": (20, 32),  # rows of the bridge from the ledge out over the sea
    "gate_row": 36,  # the second, unlit frame at the end of the bridge
    "pillars": ((-22, 40, 3.2), (38, 62, 3.5), (-10, 70, 3.0), (16, 78, 3.8), (-40, 78, 3.0), (52, 86, 3.5), (-30, 104, 4.0), (30, 108, 4.0), (0, 100, 3.0)),
    "falls_free": ((-26, 60), (44, 74), (40, 100), (-48, 96)),
    "falls_cliff": (-13, 18),
}


class Grid:
    """A block grid in local cell coordinates [lo, hi)."""

    def __init__(self, blocks, lo, hi, fill=None):
        self.b = blocks
        self.lo, self.hi = np.array(lo), np.array(hi)
        self.g = np.zeros(tuple(self.hi - self.lo), np.uint8)
        if fill:
            self.g[...] = blocks[fill]

    def _id(self, name):
        return self.b[name] if isinstance(name, str) else name

    def box(self, xs, ys, zs, name, only=None):
        """Inclusive ranges. `only` limits the fill to cells that currently hold that block."""
        sl = tuple(slice(max(r[0], lo) - lo, min(r[1], hi - 1) - lo + 1) for r, lo, hi in zip((xs, ys, zs), self.lo, self.hi))
        view = self.g[sl]
        if only is None:
            view[...] = self._id(name)
        else:
            view[view == self._id(only)] = self._id(name)

    def set(self, x, y, z, name):
        self.box((x, x), (y, y), (z, z), name)

    def get(self, x, y, z):
        i = np.array((x, y, z)) - self.lo
        return int(self.g[tuple(i)]) if np.all(i >= 0) and np.all(i < self.g.shape) else 0

    def coords(self):
        """Broadcastable float arrays of the local cell coordinates."""
        ax = [np.arange(lo, hi, dtype=np.float64) for lo, hi in zip(self.lo, self.hi)]
        return ax[0][:, None, None], ax[1][None, :, None], ax[2][None, None, :]

    def object(self, name, material, col, origin, group="opaque"):
        at = tuple(np.asarray(origin, float) + self.lo)
        return voxel.grid_object(name, self.g, self.b, material, col, group=group, origin=at)


def nether(blocks):
    """A vast cavern over a lava sea. The portal stands on a ledge high on the south
    cliff, facing north across the sea; a bridge runs out from the ledge to a platform
    where a second frame waits, unlit. Returns the grid."""
    g = Grid(blocks, (-72, 0, -6), (72, 132, 54), fill="netherrack")
    X, Y, Z = g.coords()
    lava, air = blocks["lava"], 0

    vein = noise3(X, Y, Z, 6, 501)
    g.g[vein > 0.66] = blocks["blackstone"]
    g.g[(vein > 0.66) & (_hash(X + 0 * Y + 0 * Z, Y + 0 * X + 0 * Z, Z + 0 * X + 0 * Y, 506) < 0.05)] = blocks["gilded_blackstone"]
    g.g[vein < 0.2] = blocks["basalt"]

    # The cavern: a wide dome, lower toward its rim, over a sea with a few islands.
    rim = ((X - 0) / 66.0) ** 2 + ((Y - 68) / 62.0) ** 2 + (noise2(X, Y, 18, 502) - 0.5) * 0.35
    ceiling = 41 + (fbm2(X, Y, 26, 503) - 0.5) * 14 - 16 * _smooth((rim - 0.5) / 0.5)
    land = fbm2(X, Y, 22, 504)
    ground = np.where(land > 0.64, 1 + (land - 0.64) * 26, -3.0) + 30 * _smooth((rim - 0.82) / 0.25)
    cliff = 13 + (noise2(X, Z, 7, 505) - 0.5) * 5
    hollow = (rim < 1.0) & (Z > ground) & (Z < ceiling) & (Y >= cliff)
    g.g[hollow] = air
    g.g[hollow & (Z <= 0)] = lava
    shore = (~hollow) & (Z >= -1) & (Z <= 1) & (rim < 1.0) & (Y >= cliff)
    g.g[shore & (_hash(X + 0 * Y + 0 * Z, Y + 0 * X + 0 * Z, Z + 0 * X + 0 * Y, 507) < 0.5)] = blocks["magma"]

    # Glowstone hanging from the ceiling in clusters.
    # None straight ahead of the bridge: that part of the ceiling is the backdrop of the name field.
    cluster = (noise2(X, Y, 9, 520) > 0.76) & (rim < 0.9) & (Y > 22) & ((np.abs(X) > 20) | (Y > 112))
    top = np.floor(ceiling)
    g.g[cluster & (Z >= top - 2) & (Z <= top + 1)] = blocks["glowstone"]

    # Pillars from the sea to the ceiling, flaring at both ends.
    for i, (px, py, radius) in enumerate(NETHER["pillars"]):
        flare = 1 + 1.4 * ((Z - 20) / 22.0) ** 2
        wobble = 1 + 0.3 * (noise3(X, Y, Z, 5, 530 + i) - 0.5)
        column = (np.hypot(X - px, Y - py) < radius * flare * wobble) & (rim < 1.0)
        g.g[column & ((g.g == air) | (g.g == lava))] = blocks["basalt"]

    # Lava falling free from the ceiling, and two falls running down the cliff.
    for fx, fy in NETHER["falls_free"]:
        g.box((fx, fx + 1), (fy, fy + 1), (1, 52), "lava", only=air)
    for fx in NETHER["falls_cliff"]:
        for z in range(1, 50):
            for x in (fx, fx + 1):
                row = next((y for y in range(4, 40) if g.get(x, y, z) == air), None)
                if row is not None:
                    g.set(x, row, z, "lava")

    # The ledge, an alcove above it, and the portal frame against the alcove's back wall.
    ledge, row = NETHER["ledge"], NETHER["portal_row"]
    for z in range(ledge - 5, ledge):
        reach = 20 - (ledge - 1 - z) * 1.6
        g.box((-6 + (ledge - 1 - z), 5 - (ledge - 1 - z)), (4, int(reach)), (z, z), "blackstone")
    g.box((-5, 4), (row, 22), (ledge, ledge + 7), air)
    g.box((-2, 1), (row, row), (ledge - 1, ledge + 3), "obsidian")
    g.box((-1, 0), (row, row), (ledge, ledge + 2), air)
    g.box((-5, 4), (4, row - 1), (ledge, ledge + 7), "blackstone", only=air)
    g.set(-2, row, ledge + 1, "crying_obsidian")
    g.set(1, row, ledge + 3, "crying_obsidian")

    # The bridge, its platform on a pier that rises out of the sea, and the second frame.
    (b0, b1), gate = NETHER["bridge"], NETHER["gate_row"]
    g.box((-2, 1), (b0, b1), (ledge - 1, ledge - 1), "nether_bricks")
    g.box((-1, 0), (b0, b1), (ledge - 2, ledge - 2), "blackstone")
    for y in range(b0 + 2, b1, 4):  # ribs under the deck
        g.box((-2, 1), (y, y), (ledge - 2, ledge - 2), "polished_blackstone_bricks")
    g.box((-4, 3), (b1 + 1, gate + 2), (ledge - 1, ledge - 1), "polished_blackstone_bricks")
    g.box((-3, 2), (b1 + 1, gate + 2), (ledge - 3, ledge - 2), "blackstone")
    g.box((-2, 1), (b1 + 2, gate + 1), (-4, ledge - 4), "basalt")
    g.box((-3, 2), (b1 + 1, gate + 2), (-4, 3), "basalt")
    g.box((-2, 1), (gate, gate), (ledge - 1, ledge + 3), "obsidian")
    g.box((-1, 0), (gate, gate), (ledge, ledge + 2), air)
    g.set(1, gate, ledge + 1, "crying_obsidian")
    g.set(-2, gate, ledge + 3, "crying_obsidian")
    return g
