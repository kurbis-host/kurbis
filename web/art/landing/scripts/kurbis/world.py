"""The film world: an autumn valley cut open at y = 0, laid out for the camera.

Grid axes are (x, y, z) with z up and y running away from the viewer. The cut
face (y = 0) carries the short tunnel the pumpkin digs: down, a jog, and down
through the roof of a cave that the cut face opens too. From the back of the
cave a cart track runs through the rock into a closed dungeon. A slab of ground
in front of the cut face hides the cut until it breaks away.
The Nether is a separate set (see sets.py). Deterministic.
"""

import numpy as np

SPOTS = {
    "landing": (-0.9, 2.2),  # where the pumpkin lands
    # Cut open by the y = 0 face, so the cross-section shows water over a sand basin.
    "pond": {"center": (-8.5, 0.8), "radius": (3.6, 3.3)},
    "blossom": (6, 7),  # the flowering tree
    "blossom_trunk": 4,
    "chest": (11, -9),  # a niche in the cut face with a buried chest
    "fossil": ((7, -7), (8, -7), (9, -7), (9, -6), (7, -8)),  # bone showing in the cut face: (x, z)
    # Corners of the dig on the cut face, as (x, z) of the pumpkin's bottom centre: down, a
    # jog, and down through the roof of the cave. Two blocks wide and two high, so the
    # one-block pumpkin can tumble.
    "tunnel": ((-1, 1), (-1, -5), (-5, -5), (-5, -24)),
    # Cut open by the y = 0 face as well: the camera looks in from the front.
    "cave": {"center": (3.0, 0.0, -21.5), "radius": (13.0, 12.0, 6.0), "floor": -25},
    # The cart track from the cave to the dungeon: corners of the rail's centre line. It
    # runs north out of the cave, turns east, drops five blocks and runs in through the door.
    "rail": ((0.5, 6.5, -25.0), (0.5, 17.5, -25.0), (4.0, 17.5, -25.0), (9.0, 17.5, -30.0), (16.5, 17.5, -30.0)),
    # The homestead's cavern: its interior cells, its floor and how high its dome is.
    # Daylight falls into it through a shaft in the roof onto a grass floor, where a house
    # stands in a fenced yard. Friends come in by a tunnel in the south wall; the portal
    # frame is in the east wall, to the north.
    "dungeon": {"x": (12, 34), "y": (11, 36), "floor": -30, "height": 12, "portal": (30, 31), "shaft": ((23, 27), (15, 19)), "tunnel": (21, 23), "field": ((23, 26), (25, 28))},
    # Ground in front of the cut face. It makes the meadow whole until the slice.
    "front": {"x": (-30, 30), "depth": 14, "bottom": -11},
}


DETAIL_DEPTH = 40


def tunnel_steps():
    """Unit steps of the pumpkin's bottom centre along the cut face: [(x, z), ...]."""
    corners = SPOTS["tunnel"]
    steps = [corners[0]]
    for (x0, z0), (x1, z1) in zip(corners, corners[1:]):
        n = abs(x1 - x0) + abs(z1 - z0)
        sx, sz = (x1 > x0) - (x1 < x0), (z1 > z0) - (z1 < z0)
        steps += [(x0 + sx * i, z0 + sz * i) for i in range(1, n + 1)]
    return steps


def _hash(ix, iy, iz, seed):
    h = (ix.astype(np.int64) * 374761393) ^ (iy.astype(np.int64) * 668265263) ^ (iz.astype(np.int64) * 1274126177) ^ (seed * 974634397)
    h &= 0xFFFFFFFF
    h = ((h ^ (h >> 13)) * 1274126177) & 0xFFFFFFFF
    return ((h ^ (h >> 16)) & 0xFFFFFFFF) / 4294967295.0


def _smooth(t):
    t = np.clip(t, 0.0, 1.0)
    return t * t * (3 - 2 * t)


def noise2(x, y, scale, seed):
    fx, fy = x / scale, y / scale
    x0, y0 = np.floor(fx), np.floor(fy)
    tx, ty = _smooth(fx - x0), _smooth(fy - y0)
    zero = np.zeros(1, np.int64)
    c = lambda dx, dy: _hash(x0 + dx, y0 + dy, zero, seed)
    return (c(0, 0) * (1 - tx) + c(1, 0) * tx) * (1 - ty) + (c(0, 1) * (1 - tx) + c(1, 1) * tx) * ty


def noise3(x, y, z, scale, seed):
    fx, fy, fz = x / scale, y / scale, z / scale
    x0, y0, z0 = np.floor(fx), np.floor(fy), np.floor(fz)
    tx, ty, tz = _smooth(fx - x0), _smooth(fy - y0), _smooth(fz - z0)
    c = lambda dx, dy, dz: _hash(x0 + dx, y0 + dy, z0 + dz, seed)
    lo = (c(0, 0, 0) * (1 - tx) + c(1, 0, 0) * tx) * (1 - ty) + (c(0, 1, 0) * (1 - tx) + c(1, 1, 0) * tx) * ty
    hi = (c(0, 0, 1) * (1 - tx) + c(1, 0, 1) * tx) * (1 - ty) + (c(0, 1, 1) * (1 - tx) + c(1, 1, 1) * tx) * ty
    return lo * (1 - tz) + hi * tz


def fbm2(x, y, scale, seed):
    return noise2(x, y, scale, seed) * 0.6 + noise2(x, y, scale / 2, seed + 1) * 0.28 + noise2(x, y, scale / 4, seed + 2) * 0.12


class World:
    def __init__(self, blocks, x0=-128, x1=128, depth=224, z0=-44, z1=72, flat=(), dungeon=None):
        """`flat` lists (x0, x1, z0, z1) rectangles of the cut face kept free of relief,
        so type embossed there has a flat wall behind it. `dungeon` replaces entries of
        SPOTS["dungeon"]: a frame of another shape wants its ore wall laid out otherwise."""
        self.b = blocks
        self.flat = tuple(flat)
        self.dungeon = {**SPOTS["dungeon"], **(dungeon or {})}
        self.x0, self.x1, self.depth, self.z0, self.z1 = x0, x1, depth, z0, z1
        self.shape = (x1 - x0, depth, z1 - z0)
        self.grid = np.zeros(self.shape, np.uint8)
        self.height = np.zeros(self.shape[:2], int)
        self.sprites = {"grass": [], "vines": [], "cane": [], "lily": [], "petals": [], "flowers": []}
        self.tunnel = []  # per dig step: where the pumpkin is, and the blocks that step removes
        self.front = None  # (origin, grid) of the slab in front of the cut face
        self.rng = np.random.default_rng(7)

    @property
    def origin(self):
        return (self.x0, 0, self.z0)

    def inside(self, x, y, z):
        return self.x0 <= x < self.x1 and 0 <= y < self.depth and self.z0 <= z < self.z1

    def get(self, x, y, z):
        return int(self.grid[x - self.x0, y, z - self.z0]) if self.inside(x, y, z) else 0

    def set(self, x, y, z, name, only_air=False):
        if self.inside(x, y, z) and not (only_air and self.grid[x - self.x0, y, z - self.z0]):
            self.grid[x - self.x0, y, z - self.z0] = self.b[name] if isinstance(name, str) else name

    def box(self, xs, ys, zs, name, only_air=False):
        """Inclusive world-coordinate ranges."""
        x0, x1 = max(xs[0], self.x0), min(xs[1], self.x1 - 1)
        y0, y1 = max(ys[0], 0), min(ys[1], self.depth - 1)
        z0, z1 = max(zs[0], self.z0), min(zs[1], self.z1 - 1)
        if x0 > x1 or y0 > y1 or z0 > z1:
            return
        view = self.grid[x0 - self.x0 : x1 - self.x0 + 1, y0 : y1 + 1, z0 - self.z0 : z1 - self.z0 + 1]
        value = self.b[name] if isinstance(name, str) else name
        if only_air:
            view[view == 0] = value
        else:
            view[...] = value

    def top(self, x, y):
        return int(self.height[x - self.x0, y])

    # -- generation --------------------------------------------------------
    def generate(self):
        b = self.b
        X = np.arange(self.x0, self.x1, dtype=np.float64)[:, None]
        Y = np.arange(self.depth, dtype=np.float64)[None, :]
        Z = np.arange(self.z0, self.z1, dtype=np.float64)[None, None, :]
        X3, Y3 = X[:, :, None], Y[:, :, None]

        # Surface: flat landing meadow at the front, a valley running straight
        # back between wooded hills, snow peaks either side of the far end.
        outside = np.maximum(np.abs(X + 1) - 13, 0) + np.maximum(Y - 13, 0)
        blend = _smooth(outside / 26)
        hills = (fbm2(X, Y, 30, 1) - 0.42) * 13
        sides = _smooth((np.abs(X) - 12) / 54) * 19
        back = _smooth((Y - 90) / 120) * 5
        ridged = 0.6 + 0.8 * fbm2(X, Y, 12, 9)

        def peak(cx, cy, radius, height):
            d = np.sqrt((X - cx) ** 2 + (Y - cy) ** 2) / radius
            return height * np.clip(1 - d, 0, 1) ** 0.95 * ridged

        peaks = peak(-104, 176, 39, 56) + peak(-74, 214, 30, 30) + peak(108, 186, 40, 50) + peak(82, 218, 28, 26)
        h = np.rint(blend * (hills + sides + back) + peaks).astype(int).clip(-1, self.z1 - 6)
        # The landing meadow is never flat or straight: it rolls in single-block steps along
        # wandering contours, rises gently to the back and sides, and dips for the pond.
        # Only the small spot where the cast stands is kept level.
        near = (np.abs(X + 1) < 31) & (Y < 34)
        edge = np.minimum(31 - np.abs(X + 1), 34 - Y)
        roll = (fbm2(X, Y, 10, 55) - 0.5) * 4.6
        rise = _smooth((Y - 4.2) / 5) * 1.25 + _smooth((Y - 15) / 16) * 1.5  # gentle: wide treads, few risers
        flank = _smooth((np.abs(X + 1.5) - 6) / 8) * 1.6
        stage = np.exp(-((((X + 1.2) / 6.0) ** 2 + ((Y - 2.8) / 2.3) ** 2) ** 2))  # flat-topped; the front lip stays free
        (pcx, pcy), (prx, pry) = SPOTS["pond"]["center"], SPOTS["pond"]["radius"]
        basin = _smooth(2.1 - np.hypot((X - pcx) / prx, (Y - pcy) / pry))
        meadow = (roll + rise + flank) * (1 - stage) * (1 - basin) * _smooth(edge / 8)
        h = np.where(near, h + np.rint(meadow).astype(int), h)
        # Terrace the mountains: tall steps read as banded rock from far away,
        # where single-block steps only read as noise.
        h = np.where(h > 27, 27 + ((h - 27) // 4) * 4, h)
        self.height = h
        hh = h[:, :, None]
        solid = Z <= hh
        below = hh - Z

        g = self.grid
        g[solid] = b["stone"]
        soil = 7 + np.rint(noise2(X, Y, 8, 5) * 5)[:, :, None]
        meadow = hh <= 25
        g[solid & meadow & (below <= soil)] = b["dirt"]
        g[solid & meadow & (below == 0)] = b["grass"]
        crag = noise2(X, Y, 10, 13)[:, :, None] + (Z % 5 == 0) * 0.25
        g[solid & ~meadow] = b["rock"]
        g[solid & ~meadow & (crag > 0.66)] = b["rock_dark"]
        g[solid & (Z > 46 + np.rint(noise2(X, Y, 14, 12) * 4)[:, :, None]) & (below <= 2)] = b["snow_far"]

        # Rock variety, ore veins and deepslate only matter where the ground is
        # ever seen from the side: the slab just behind the cut face.
        n = min(self.depth, DETAIL_DEPTH)
        gs, Ys, solid_s = g[:, :n], Y3[:, :n], solid[:, :n]
        Xn, Yn = X, Y[:, :n]
        stone = gs == b["stone"]
        rock = noise3(X3, Ys, Z, 5, 21)
        gs[stone & (rock > 0.72)] = b["andesite"]
        gs[stone & (noise3(X3, Ys, Z, 3.5, 22) > 0.86)] = b["gravel"]
        deep = stone & (Z < -35 + noise2(Xn, Yn, 6, 7)[:, :, None] * 5)
        gs[deep] = b["deepslate"]
        gs[deep & (rock > 0.74)] = b["tuff"]
        vein = stone & (noise3(X3, Ys, Z, 2.4, 31) > 0.8)
        kind = noise3(X3, Ys, Z, 9, 32)
        bands = (
            (Z > -12, ("coal_ore", "iron_ore", "copper_ore", "coal_ore")),
            ((Z <= -12) & (Z > -28), ("iron_ore", "gold_ore", "redstone_ore", "lapis_ore", "copper_ore")),
            ((Z <= -28) & ~deep, ("diamond_ore", "redstone_ore", "gold_ore", "lapis_ore")),
            (deep, ("deepslate_diamond_ore", "deepslate_redstone_ore", "deepslate_gold_ore", "deepslate_diamond_ore")),
        )
        for band, ores in bands:
            pick = np.minimum((kind * len(ores)).astype(int), len(ores) - 1)
            for i, ore in enumerate(ores):
                gs[vein & band & (pick == i)] = b[ore]
        dirt = gs == b["dirt"]
        gs[dirt & (below[:, :n] > 3) & (noise3(X3, Ys, Z, 3, 23) > 0.8)] = b["stone"]
        gs[solid_s & (Z <= self.z0 + 1 + (noise2(Xn, Yn, 2, 8)[:, :, None] > 0.5))] = b["bedrock"]

        self._carve_cave(X3[:, :n], Ys, Z)
        self._dungeon()
        self._rail()

        # Relief on the cut face: knock out patches of the front one or two
        # layers so a raking light finds edges instead of a flat wall.
        self.front_before = g[:, :2, :].copy()
        XZ, ZX = X, Z[0]
        relief = noise2(XZ, ZX, 6, 71) * 0.7 + noise2(XZ, ZX, 2.5, 72) * 0.3
        under = ZX < h[:, :1]
        keep = np.zeros(relief.shape, bool)
        for xa, xb, za, zb in self.flat:
            keep[int(np.floor(xa)) - self.x0 : int(np.ceil(xb)) - self.x0, int(np.floor(za)) - self.z0 : int(np.ceil(zb)) - self.z0] = True
        g[:, 0, :][(relief > 0.61) & under & ~keep] = 0
        g[:, 1, :][(relief > 0.76) & under & ~keep] = 0
        cx, cz = SPOTS["chest"]
        self.box((cx, cx + 1), (0, 0), (cz, cz), 0)
        for x, z in SPOTS["fossil"]:
            self.set(x, 0, z, "bone")
        self._tunnel()
        self._pond()
        self._front_slab()
        self._trees()
        self._blossom()
        self._pumpkins()
        self._grass_tufts()
        return self

    def _carve_cave(self, X3, Y3, Z):
        """A full cavern well behind the cut face, so the camera can fly inside it."""
        b, g = self.b, self.grid[:, : Y3.shape[1]]
        (cx, cy, cz), (rx, ry, rz) = SPOTS["cave"]["center"], SPOTS["cave"]["radius"]
        cy = int(cy)
        floor = SPOTS["cave"]["floor"]
        wobble = (noise3(X3, Y3, Z, 4, 9) - 0.5) * 0.5
        e = ((X3 - cx) / rx) ** 2 + ((Y3 - cy) / ry) ** 2 + ((Z - cz) / rz) ** 2
        cave = (e < 1 + wobble) & (Z >= floor)
        g[cave] = 0

        # Lush corner on the right: moss on the floor and ceiling, a pool, vines.
        mossy = noise2(X3[:, :, 0], Y3[:, :, 0], 5, 41) > 0.42
        for x in range(int(cx - rx) - 1, int(cx + rx) + 2):
            for y in range(int(cy - ry) - 1, int(cy + ry) + 2):
                if not self.inside(x, y, floor):
                    continue
                lush = x > 4 and mossy[x - self.x0, y]
                if self.get(x, y, floor) == 0 and self.get(x, y, floor - 1):
                    if lush:
                        self.set(x, y, floor - 1, "moss")
                for z in range(floor + 2, int(cz + rz) + 3):
                    if self.get(x, y, z) and self.get(x, y, z - 1) == 0:
                        if lush:
                            self.set(x, y, z, "moss")
                            if _hash(np.int64(x), np.int64(y), np.int64(0), 43) < 0.2 and x > 8:
                                self.sprites["vines"].append((x, y, z - 1, 1 + int(_hash(np.int64(x), np.int64(y), np.int64(1), 44) * 3)))
                        break
        for x in range(9, 14):
            for y in range(cy + 2, cy + 7):
                if self.get(x, y, floor) == 0 and (x - 11) ** 2 / 6.5 + (y - cy - 4) ** 2 / 6.5 < 1:
                    self.set(x, y, floor - 1, "water")

        self._ore_patch((-8, cy + 3, floor + 1), 2.4, "diamond_ore")
        self._ore_patch((-9, cy + 1, -20), 2.2, "redstone_ore")
        self._ore_patch((2, cy + 11, -19), 2.6, "gold_ore")
        self._ore_patch((15, cy + 3, -21), 2.2, "lapis_ore")
        self._ore_patch((-5, cy + 9, -17), 2.0, "iron_ore")
        self._ore_patch((6, cy - 9, -20), 2.4, "gold_ore")
        self._ore_patch((-6, cy - 8, -22), 2.2, "redstone_ore")

        # A low outcrop of diamond ore near the cast: something to mine, and a stack of
        # single blocks right beside a two-block player.
        for x, y, tall in ((-5, 4, 3), (-4, 4, 2), (-5, 5, 2), (-4, 5, 1), (-5, 6, 1), (-6, 5, 2)):
            self.box((x, x), (cy + y, cy + y), (floor, floor + tall - 1), "stone")
        for x, y, dz in ((-4, 4, 1), (-5, 4, 2), (-4, 5, 0), (-5, 5, 1), (-4, 4, 0)):
            self.set(x, cy + y, floor + dz, "diamond_ore")

        # A small base camp against the back wall: somebody lives here.
        # The crafting table and barrel near the cast are one-block scale cues.
        # The middle of the back wall stays free: that is where the cart track leaves.
        camp = ((-4, 8, 0, "furnace"), (-3, 8, 0, "crafting_table"), (3, 9, 0, "bookshelf"), (3, 9, 1, "bookshelf"), (4, 9, 0, "barrel"), (6, 5, 0, "crafting_table"), (7, 5, 0, "furnace"), (-7, 4, 0, "barrel"))
        for x, y, dz, name in camp:
            z = floor
            while self.get(x, cy + y, z) and z < floor + 4:
                z += 1
            if self.get(x, cy + y, z) == 0:  # only where the cave really is
                self.set(x, cy + y, z + dz, name)

    def _tunnel(self):
        """Carve the pumpkin's tunnel step by step, remembering what each step removes
        so those blocks can be shown until the pumpkin digs them."""

        def carve(cells, at):
            got = []
            for x, y, z in cells:
                block = self.get(x, y, z)
                if block:
                    got.append((x, y, z, block))
                    self.set(x, y, z, 0)
            self.tunnel.append({"at": at, "cells": got})

        for x, z in tunnel_steps():
            carve([(cx, y, cz) for cx in (x - 1, x) for y in (0, 1) for cz in (z, z + 1)], (float(x), 1.0, float(z)))

    def _dungeon(self):
        """The homestead's cavern: a dome of stone with lumpy walls, highest in the middle,
        a shaft in its roof to the daylight far above, grass on the floor where that light
        falls and stone and gravel elsewhere. A tunnel in the south wall, the portal frame
        in the east wall."""
        d = self.dungeon
        (x0, x1), (y0, y1), floor, height = d["x"], d["y"], d["floor"], d["height"]
        lo, hi = (x0 - 3, y0 - 3, floor - 2), (x1 + 3, y1 + 3, floor + height + 9)
        self.box((lo[0], hi[0]), (lo[1], hi[1]), (lo[2], hi[2]), "stone")
        shell = self.grid[lo[0] - self.x0 : hi[0] + 1 - self.x0, lo[1] : hi[1] + 1, lo[2] - self.z0 : hi[2] + 1 - self.z0]
        X, Y, Z = np.meshgrid(*(np.arange(n, dtype=np.float64) for n in shell.shape), indexing="ij")
        wx, wy, wz = X + lo[0] + 0.5, Y + lo[1] + 0.5, Z + lo[2] + 0.5
        # The room: a flattened dome over the floor's box, its walls pushed in and out by noise.
        cx, cy = (x0 + x1 + 1) / 2, (y0 + y1 + 1) / 2
        rx, ry = (x1 + 1 - x0) / 2, (y1 + 1 - y0) / 2
        across = np.sqrt(((wx - cx) / rx) ** 4 + ((wy - cy) / ry) ** 4) ** 0.5  # a rounded box: 1 at its walls
        lump = (noise3(wx, wy, wz, 4.5, 611) - 0.5) * 0.34
        roof = floor + height * np.sqrt(np.clip(1 - across**2.2, 0, 1)) + (noise3(wx, wy, wz, 3.0, 612) - 0.5) * 2.2
        air = (across + lump < 1.0) & (wz > floor) & (wz < roof)
        # The shaft: straight up through the roof to the daylight.
        (sx0, sx1), (sy0, sy1) = d["shaft"]
        air |= (wx > sx0) & (wx < sx1 + 1) & (wy > sy0) & (wy < sy1 + 1) & (wz > floor) & (wz < floor + height + 8)
        # The tunnel friends come in by, from the south.
        t0, t1 = d["tunnel"]
        air |= (wx > t0) & (wx < t1 + 1) & (wy > y0 - 4) & (wy < y0 + 2) & (wz > floor) & (wz < floor + 3)
        shell[air] = 0
        # What the walls are made of: stone, with andesite, tuff and moss in patches.
        solid = shell != 0
        kind = noise3(wx, wy, wz, 5.0, 613)
        shell[solid & (kind > 0.62)] = self.b["andesite"]
        shell[solid & (kind < 0.3)] = self.b["tuff"]
        shell[solid & (noise3(wx, wy, wz, 3.2, 614) > 0.7)] = self.b["mossy_cobblestone"]
        shell[solid & (_hash(X, Y, Z, 615) < 0.012)] = self.b["glowstone"]  # a few glints in the dark
        # The floor: grass in the shaft's light (spreading in a soft patch), dirt under it; gravel paths elsewhere.
        top = floor - 1 - lo[2]
        fx, fy = wx[:, :, 0], wy[:, :, 0]
        sun = np.sqrt(((fx - (sx0 + sx1 + 1) / 2) / 9.5) ** 2 + ((fy - (sy0 + sy1 + 1) / 2) / 10.5) ** 2) + (noise2(fx, fy, 3.0, 616) - 0.5) * 0.5
        floor_cells = shell[:, :, top]
        grass = (sun < 1.0)
        floor_cells[grass] = self.b["grass"]
        shell[:, :, top - 1][grass] = self.b["dirt"]
        floor_cells[~grass & (noise2(fx, fy, 2.4, 617) > 0.62)] = self.b["gravel"]
        # The field beside the yard: tilled.
        (fx0, fx1), (fy0, fy1) = d["field"]
        floor_cells[(fx > fx0) & (fx < fx1 + 1) & (fy > fy0) & (fy < fy1 + 1)] = self.b["farmland"]
        shell[:, :, top] = floor_cells
        # The portal frame, set into the east wall.
        p0, p1 = d["portal"]
        px = x1 + 1
        self.box((px, px + 1), (p0 - 1, p1 + 1), (floor - 1, floor + 3), "obsidian")
        self.box((px, px + 1), (p0, p1), (floor, floor + 2), 0)
        self.box((x1 - 1, px - 1), (p0 - 2, p1 + 2), (floor, floor + 4), 0)  # a bay before it, so it stands clear of the wall's lumps
        self.set(px, p0 - 1, floor + 3, "crying_obsidian")
        self.set(px, p1 + 1, floor + 1, "crying_obsidian")

    def _rail(self):
        """The cart tunnel: three wide and three high along the track, a block taller on
        the slope, with pit props set into the walls."""
        (rx, ry, rz), (_, turn_y, _), (slope_x, _, _), (foot_x, _, low), _ = SPOTS["rail"]
        cx, cy, floor, low = int(np.floor(rx)), int(np.floor(turn_y)), int(rz), int(low)
        slope_x, foot_x = int(slope_x), int(foot_x)
        door = SPOTS["dungeon"]["x"][0] - 1
        self.box((cx - 1, cx + 1), (int(np.floor(ry)), cy + 1), (floor, floor + 2), 0)  # north out of the cave
        self.box((cx - 1, slope_x - 1), (cy - 1, cy + 1), (floor, floor + 2), 0)  # the turn east
        self.box((cx + 2, cx + 2), (cy - 2, cy - 2), (floor, floor + 2), 0)  # the inside of the turn, cut back for the camera
        for x in range(slope_x, foot_x):  # the slope: one block down per block
            z = floor - 1 - (x - slope_x)
            self.box((x, x), (cy - 1, cy + 1), (z, z + 3), 0)
        self.box((foot_x, door), (cy - 1, cy + 1), (low, low + 2), 0)  # the run in, through the dungeon wall
        # Pit props: posts in the walls and a beam in the roof.
        for y in (10, 14):
            for x in (cx - 2, cx + 2):
                self.box((x, x), (y, y), (floor, floor + 2), "log")
            self.box((cx - 2, cx + 2), (y, y), (floor + 3, floor + 3), "planks")
        for x in (slope_x + 2, foot_x + 1):
            z = floor - 1 - (x - slope_x) if x < foot_x else low
            for y in (cy - 2, cy + 2):
                self.box((x, x), (y, y), (z, z + 3), "log")
            self.box((x, x), (cy - 2, cy + 2), (z + 4 if x < foot_x else z + 3, z + 4 if x < foot_x else z + 3), "planks")

    def _front_slab(self):
        """Ground in front of the cut face: each column repeats the column behind it."""
        f = SPOTS["front"]
        (xa, xb), depth, bottom = f["x"], f["depth"], f["bottom"]
        top = int(max(self.top(x, 0) for x in range(xa, xb)))
        grid = np.zeros((xb - xa, depth, top - bottom + 1), np.uint8)
        water, sand, air = self.b["water"], self.b["sand"], 0
        for i, x in enumerate(range(xa, xb)):
            h = self.top(x, 0)
            column = self.front_before[x - self.x0, 0, bottom - self.z0 : h - self.z0 + 1].copy()
            now = self.grid[x - self.x0, 0, bottom - self.z0 : h - self.z0 + 1]
            column[now == water] = sand  # the pond's near bank
            column[now == sand] = sand
            grid[i, :, : len(column)] = column[None, :]
        self.front = ((xa, -depth, bottom), grid)

    def _ore_patch(self, center, radius, ore):
        """Turn exposed stone near `center` into ore so the cave walls sparkle."""
        cx, cy, cz = center
        r = int(np.ceil(radius))
        plain = {self.b["stone"], self.b["andesite"], self.b["gravel"]}
        for x in range(cx - r, cx + r + 1):
            for y in range(max(cy - r, 0), cy + r + 1):
                for z in range(cz - r, cz + r + 1):
                    if (x - cx) ** 2 + (y - cy) ** 2 + (z - cz) ** 2 > radius**2 or self.get(x, y, z) not in plain:
                        continue
                    exposed = any(self.get(x + dx, y + dy, z + dz) == 0 for dx, dy, dz in ((1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)))
                    if exposed and _hash(np.int64(x), np.int64(y), np.int64(z), 51) < 0.62:
                        self.set(x, y, z, ore)

    def _pond(self):
        """A funnel of sand holding water, as in the reference cross-section."""
        (cx, cy), (rx, ry) = SPOTS["pond"]["center"], SPOTS["pond"]["radius"]
        for x in range(int(cx - rx) - 3, int(cx + rx) + 4):
            for y in range(0, int(cy + ry) + 4):
                e = float(np.hypot((x - cx) / rx, (y - cy) / ry))
                top = self.top(x, y)
                if e < 1:
                    deep = 1 + int(2.6 * (1 - e) ** 0.8)
                    self.box((x, x), (y, y), (top - deep + 1, top), "water")
                    self.box((x, x), (y, y), (top - deep - 1, top - deep), "sand")
                elif top == 0 and e < 1.42 + _hash(np.int64(x), np.int64(y), np.int64(0), 83) * 0.25:
                    self.box((x, x), (y, y), (top - 1, top), "sand")  # beach only at water level
        # Clumps on the left bank as in the reference, and a few on the far bank where the shot sees them.
        # A gap is left on the far bank for the friend who fishes from there.
        for x, y, tall in ((-14, 0, 2), (-13, 0, 4), (-13, 1, 3), (-14, 2, 2), (-13, 3, 3), (-12, 4, 2), (-11, 5, 3), (-10, 5, 3), (-12, 6, 2), (-10, 6, 2), (-11, 6, 3)):
            base = self.top(x, y) + 1
            self.sprites["cane"] += [(x, y, base + k) for k in range(tall)]
        self.sprites["lily"] += [(-8, 2, 1.02), (-10, 1, 1.02), (-7, 3, 1.02)]

    def _blossom(self):
        """Green crown dotted with pink flowers, plus a few all-pink clusters."""
        x, y = SPOTS["blossom"]
        z = self.top(x, y) + 1
        t = SPOTS["blossom_trunk"]
        self.box((x, x), (y, y), (z, z + t), "log")
        self._blob(x, y, z + t + 2, 3.6, 2.7, "leaves_blossom")
        self._blob(x - 2, y - 1, z + t + 2, 2.2, 1.7, "leaves_blossom")
        self._blob(x + 2, y + 1, z + t + 1, 2.5, 1.9, "leaves_blossom")
        green, pink = self.b["leaves_blossom"], self.b["leaves_cherry"]
        sl = (slice(x - 7 - self.x0, x + 8 - self.x0), slice(max(y - 7, 0), y + 8), slice(z - self.z0, z + 14 - self.z0))
        view = self.grid[sl]
        X, Y, Z = np.meshgrid(*(np.arange(n) for n in view.shape), indexing="ij")
        view[(view == green) & (_hash(X // 2, Y // 2, Z // 2, 87) < 0.2)] = pink
        for dx in range(-5, 6):
            for dy in range(-5, 6):
                gx, gy = x + dx, y + dy
                top = self.top(gx, gy)
                if gy >= 0 and self.get(gx, gy, top) == self.b["grass"] and self.get(gx, gy, top + 1) == 0:
                    if _hash(np.int64(gx), np.int64(gy), np.int64(0), 88) < 0.3 - 0.035 * float(np.hypot(dx, dy)):
                        self.sprites["petals"].append((gx, gy, top + 1.02))

    def _blob(self, cx, cy, cz, rx, rz, leaf):
        """A lumpy ellipsoid of leaves, flatter underneath. Fills air only."""
        R, H = int(np.ceil(rx)) + 1, int(np.ceil(rz)) + 1
        X, Y, Z = np.meshgrid(np.arange(cx - R, cx + R + 1), np.arange(cy - R, cy + R + 1), np.arange(cz - H, cz + H + 1), indexing="ij")
        dz = (Z - cz) / rz
        dz = np.where(dz < 0, dz * 1.9, dz)
        lump = (_hash(X, Y, Z, 77) - 0.5) * 0.42
        inside = ((X - cx) / rx) ** 2 + ((Y - cy) / rx) ** 2 + dz**2 < 1 + lump
        ix, iz = X - self.x0, Z - self.z0
        ok = inside & (ix >= 0) & (ix < self.shape[0]) & (Y >= 0) & (Y < self.depth) & (iz >= 0) & (iz < self.shape[2])
        sel = (ix[ok], Y[ok], iz[ok])
        cur = self.grid[sel]
        self.grid[sel] = np.where(cur == 0, self.b[leaf], cur)

    def tree(self, x, y, leaf, trunk=None, radius=None, thick=1, place=True):
        """Round, chunky canopy on a dark trunk: the trailer tree. With `place` off it only
        makes its random draws, so the trees after it come out as they would have."""
        z = self.top(x, y) + 1
        radius = radius or float(self.rng.uniform(2.3, 4.3))
        trunk = trunk or int(3 + radius * 1.2 + self.rng.integers(0, 3))
        offsets = [(self.rng.uniform(-0.7, 0.7, 2) * radius).astype(int) for _ in range(2)]
        if not place:
            return
        self.box((x, x + thick - 1), (y, y + thick - 1), (z, z + trunk), "log")
        top = z + trunk
        self._blob(x, y, top, radius, radius * 0.82, leaf)
        for ox, oy in offsets:
            self._blob(x + ox, y + oy, top - int(radius * 0.35), radius * 0.68, radius * 0.6, leaf)

    def spruce(self, x, y, place=True):
        z = self.top(x, y) + 1
        trunk = int(self.rng.integers(8, 14))
        if not place:
            return
        self.box((x, x), (y, y), (z, z + trunk), "spruce_log")
        layers = trunk - 1
        for k in range(layers):
            left = layers - k
            half = 0 if left <= 1 else 1 if left <= 3 or k % 2 else 2 if left <= 7 else 3
            self.box((x - half, x + half), (y - half, y + half), (z + 2 + k, z + 2 + k), "leaves_spruce", only_air=True)
        self.set(x, y, z + trunk + 1, "leaves_spruce")

    def _trees(self):
        autumn = ("leaves_orange", "leaves_orange", "leaves_amber", "leaves_red", "leaves_red", "leaves_crimson", "leaves_amber", "leaves_orange", "leaves_green")
        # Giants either side of the meadow: they frame the hero and the landing.
        self.tree(-23, 15, "leaves_crimson", trunk=16, radius=6.4, thick=2)
        self.tree(24, 17, "leaves_orange", trunk=15, radius=6.0, thick=2)
        self.tree(-27, 7, "leaves_orange", trunk=11, radius=4.6)
        self.tree(25, 6, "leaves_amber", trunk=10, radius=4.2)
        (px, py), (prx, pry) = SPOTS["pond"]["center"], SPOTS["pond"]["radius"]
        for gx in range(self.x0 + 5, self.x1 - 5, 7):
            for gy in range(6, self.depth - 6, 7):
                x = gx + int(self.rng.integers(-2, 3))
                y = gy + int(self.rng.integers(-2, 3))
                roll = self.rng.random()
                top = self.top(x, y)
                near_landing = abs(x + 1) < 31 and y < 34
                in_pond = ((x - px) / (prx + 4)) ** 2 + ((y - py) / (pry + 4)) ** 2 < 1
                dense = 0.3 + float(noise2(np.array([[float(x)]]), np.array([[float(y)]]), 40, 91)[0, 0]) * 0.75
                if near_landing or in_pond or top > 25 or roll > dense:
                    continue
                if self.get(x, y, top) != self.b["grass"]:
                    continue
                # The low sun stands behind this stretch of the ridge, seen from the landing: a tree
                # there turns black in its glare, a hole in the middle of the shot.
                place = not (abs(x + 5) <= 5 and 32 <= y <= 70)
                if self.rng.random() < (0.1 + max(top - 8, 0) * 0.03):
                    self.spruce(x, y, place=place)
                else:
                    self.tree(x, y, autumn[int(self.rng.integers(len(autumn)))], place=place)

    def _pumpkins(self):
        # Decoration only, kept clear of the cast and of the address on the ridge.
        for x, y in ((-16, 3), (-19, 2), (9, 3), (-15, 12), (11, 12)):
            self.set(x, y, self.top(x, y) + 1, "pumpkin", only_air=True)
        self.set(-3, 3, self.top(-3, 3) + 1, "barrel", only_air=True)  # the fisherman's barrel, one block: a scale cue

    def _grass_tufts(self):
        grass = self.b["grass"]
        for x in range(-34, 35):
            for y in range(0, 40):
                top = self.top(x, y)
                if self.get(x, y, top) == grass and self.get(x, y, top + 1) == 0:
                    roll = _hash(np.int64(x), np.int64(y), np.int64(0), 61)
                    stage = abs(x + 1) < 16 and y < 14  # keep the cast, caption and address readable
                    if stage and (-9 <= x <= 6 or roll > 0.07):
                        continue
                    if roll < 0.2:
                        self.sprites["grass"].append((x, y, top + 1))
                    elif roll < 0.235 and y < 30:
                        self.sprites["flowers"].append((x, y, top + 1, int(roll * 4000) % 4))
