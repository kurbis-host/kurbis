"""The landing world: a hilly autumn surface cut open at y = 0 to show what is below.

Grid axes are (x, y, z) with z up and y running away from the viewer, so the
cross-section the page scrolls down is the y = 0 face. Deterministic.
"""

import numpy as np

SPOTS = {
    "landing": (-2.0, 1.0, 1.0),  # where the pumpkin lands, top of the shaft
    "shaft": {"x": (-3, -2), "y": (0, 1), "bottom": -17},  # two wide, so the one-block pumpkin can tumble
    "cave": {"center": (3.0, 0.0, -21.5), "radius": (13.0, 12.0, 6.0), "floor": -25},
    # Cut open by the y = 0 face, so the cross-section shows water over a sand basin.
    "pond": {"center": (-8.5, 0.8), "radius": (3.6, 3.3)},
    "blossom": (6, 7),  # the flowering tree, on its one-block mound
    "chest": (3, -8),  # a niche in the cut face with a buried chest
    # Two more rooms on the way down, each reached by the shaft continuing.
    "vault": {"x": (-9, 10), "y": (0, 8), "floor": -44, "top": -39, "shaft": (1, 2)},
    "nether": {"x": (-8, 9), "y": (0, 9), "floor": -60, "top": -50, "shaft": (0, 1), "portal": (3, 5)},
    "blossom_trunk": 4,
}


DETAIL_DEPTH = 30


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
    def __init__(self, blocks, x0=-128, x1=128, depth=224, z0=-68, z1=72):
        self.b = blocks
        self.x0, self.x1, self.depth, self.z0, self.z1 = x0, x1, depth, z0, z1
        self.shape = (x1 - x0, depth, z1 - z0)
        self.grid = np.zeros(self.shape, np.uint8)
        self.height = np.zeros(self.shape[:2], int)
        self.sprites = {"grass": [], "vines": [], "cane": [], "lily": [], "petals": [], "flowers": []}
        self.plugs = {}  # shaft name -> (origin, blocks it will dig out): shown until that dig happens
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

    def restore_front(self, xs, zs):
        """Undo the cut-face relief in a rectangle, so type has a flat wall behind it.
        Only refills air, so the shaft, caves, niches and the pond stay as they are."""
        x0, x1 = int(np.floor(xs[0])) - self.x0, int(np.ceil(xs[1])) - self.x0
        z0, z1 = int(np.floor(zs[0])) - self.z0, int(np.ceil(zs[1])) - self.z0
        now, before = self.grid[x0:x1, :2, z0:z1], self.front_before[x0:x1, :, z0:z1]
        keep = np.zeros(now.shape, bool)
        (sx0, sx1), (sy0, sy1) = SPOTS["shaft"]["x"], SPOTS["shaft"]["y"]
        cx, cz = SPOTS["chest"]
        keeps = [((sx0, sx1), (self.z0, self.z1)), ((cx, cx + 1), (cz, cz))]
        keeps += [(SPOTS[k]["shaft"], (self.z0, self.z1)) for k in ("vault", "nether")]
        for (ax0, ax1), (az0, az1) in keeps:
            a0, a1 = max(ax0 - self.x0, x0) - x0, min(ax1 - self.x0 + 1, x1) - x0
            b0, b1 = max(az0 - self.z0, z0) - z0, min(az1 - self.z0 + 1, z1) - z0
            if a0 < a1 and b0 < b1:
                keep[a0:a1, :, b0:b1] = True
        fill = (now == 0) & (before != 0) & ~keep
        now[fill] = before[fill]

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

        # Relief on the cut face: knock out patches of the front one or two
        # layers so a raking light finds edges instead of a flat wall.
        self._vault()
        self._nether()
        self.front_before = g[:, :2, :].copy()
        XZ, ZX = X, Z[0]
        relief = noise2(XZ, ZX, 6, 71) * 0.7 + noise2(XZ, ZX, 2.5, 72) * 0.3
        under = ZX < h[:, :1]
        g[:, 0, :][(relief > 0.61) & under] = 0
        g[:, 1, :][(relief > 0.76) & under] = 0
        cx, cz = SPOTS["chest"]
        self.box((cx, cx + 1), (0, 0), (cz, cz), 0)
        self._pond()
        self._trees()
        self._blossom()
        self._pumpkins()
        self._grass_tufts()
        return self

    def _carve_cave(self, X3, Y3, Z):
        b, g = self.b, self.grid[:, : Y3.shape[1]]
        (cx, cy, cz), (rx, ry, rz) = SPOTS["cave"]["center"], SPOTS["cave"]["radius"]
        floor = SPOTS["cave"]["floor"]
        wobble = (noise3(X3, Y3, Z, 4, 9) - 0.5) * 0.5
        e = ((X3 - cx) / rx) ** 2 + ((Y3 - cy) / ry) ** 2 + ((Z - cz) / rz) ** 2
        cave = (e < 1 + wobble) & (Z >= floor)
        g[cave] = 0

        # The shaft the pumpkin digs: open on the cut face so the fall is visible.
        shaft = SPOTS["shaft"]
        (sx0, sx1), (sy0, sy1) = shaft["x"], shaft["y"]
        self._dig("shaft", (sx0, sx1), (sy0, sy1), (shaft["bottom"], 2))

        # Lush corner on the right: moss on the floor and ceiling, a pool, vines.
        mossy = noise2(X3[:, :, 0], Y3[:, :, 0], 5, 41) > 0.42
        for x in range(int(cx - rx) - 1, int(cx + rx) + 2):
            for y in range(0, int(ry) + 2):
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
                            if _hash(np.int64(x), np.int64(y), np.int64(0), 43) < 0.2 and y > 0 and x > 8:
                                self.sprites["vines"].append((x, y, z - 1, 1 + int(_hash(np.int64(x), np.int64(y), np.int64(1), 44) * 3)))
                        break
        for x in range(9, 14):
            for y in range(2, 7):
                if self.get(x, y, floor) == 0 and (x - 11) ** 2 / 6.5 + (y - 4) ** 2 / 6.5 < 1:
                    self.set(x, y, floor - 1, "water")

        self._ore_patch((-8, 3, floor + 1), 2.4, "diamond_ore")
        self._ore_patch((-9, 1, -20), 2.2, "redstone_ore")
        self._ore_patch((2, 11, -19), 2.6, "gold_ore")
        self._ore_patch((15, 3, -21), 2.2, "lapis_ore")
        self._ore_patch((-5, 9, -17), 2.0, "iron_ore")

        # A low outcrop of diamond ore near the front: something to mine, and a stack of
        # single blocks right beside a two-block player.
        for x, y, tall in ((-5, 4, 3), (-4, 4, 2), (-5, 5, 2), (-4, 5, 1), (-5, 6, 1), (-6, 5, 2)):
            self.box((x, x), (y, y), (floor, floor + tall - 1), "stone")
        for x, y, dz in ((-4, 4, 1), (-5, 4, 2), (-4, 5, 0), (-5, 5, 1), (-4, 4, 0)):
            self.set(x, y, floor + dz, "diamond_ore")

        # A small base camp against the back wall: somebody lives here.
        # The crafting table and barrel near the front are one-block scale cues beside the cast.
        camp = ((-3, 8, 0, "furnace"), (-2, 8, 0, "crafting_table"), (1, 9, 0, "bookshelf"), (1, 9, 1, "bookshelf"), (2, 9, 0, "barrel"), (5, 4, 0, "crafting_table"), (6, 4, 0, "furnace"), (-7, 4, 0, "barrel"))
        for x, y, dz, name in camp:
            z = floor
            while self.get(x, y, z) and z < floor + 4:
                z += 1
            if self.get(x, y, z) == 0:  # only where the cave really is: never climb out to the surface
                self.set(x, y, z + dz, name)

    def _dig(self, name, xs, ys, zs):
        """Carve a shaft, keeping what was there as a plug for the scenes before the dig."""
        view = self.grid[xs[0] - self.x0 : xs[1] - self.x0 + 1, ys[0] : ys[1] + 1, zs[0] - self.z0 : zs[1] - self.z0 + 1]
        self.plugs[name] = ((xs[0], ys[0], zs[0]), view.copy())
        view[...] = 0

    def _room(self, name, spec, wall, carve_to):
        """A walled room open to the cut face, plus the shaft down into it."""
        (x0, x1), (y0, y1), floor, top = spec["x"], spec["y"], spec["floor"], spec["top"]
        for x in range(x0 - 1, x1 + 2):
            for y in range(0, y1 + 2):
                for z in range(floor - 1, top + 2):
                    inside = x0 <= x <= x1 and y <= y1 and floor <= z <= top
                    self.set(x, y, z, 0 if inside else wall(x, y, z))
        self._dig(name, spec["shaft"], (0, 1), (top + 1, carve_to))

    def _vault(self):
        """The treasure room: a mossy cobblestone dungeon, as in the reference cross-section."""
        v = SPOTS["vault"]

        def wall(x, y, z):
            return "mossy_cobblestone" if _hash(np.int64(x), np.int64(y), np.int64(z), 301) < 0.42 else "cobblestone"

        self._room("vault", v, wall, SPOTS["cave"]["floor"] - 1)
        (x0, x1), (y0, y1), top = v["x"], v["y"], v["top"]
        for x in range(x0 + 2, x1, 5):  # warm lamps set into the ceiling
            for y in (2, 6):
                self.set(x, y, top + 1, "froglight")

    def _nether(self):
        """The portal chamber: blackstone, veins of netherrack, lava either side."""
        n = SPOTS["nether"]

        def wall(x, y, z):
            # Netherrack in veins, not a checkerboard: follow a smooth noise field.
            vein = float(noise3(np.float64(x), np.float64(y), np.float64(z), 4.5, 313))
            if vein > 0.6:
                return "netherrack"
            return "gilded_blackstone" if _hash(np.int64(x), np.int64(y), np.int64(z), 311) < 0.05 else "blackstone"

        self._room("nether", n, wall, SPOTS["vault"]["floor"] - 1)
        (x0, x1), (y0, y1), floor = n["x"], n["y"], n["floor"]
        for x in range(x0, x1 + 1):  # lava channels along both side walls, magma at the rim
            for y in range(0, y1 + 1):
                edge = min(x - x0, x1 - x)
                wob = _hash(np.int64(x), np.int64(y), np.int64(0), 312)
                if edge < 2 + (wob > 0.6):
                    self.set(x, y, floor - 1, "lava")
                elif edge < 3 + (wob > 0.5):
                    self.set(x, y, floor - 1, "magma")
        px, py = n["portal"]  # obsidian frame, 4 wide and 5 tall, on a low obsidian step
        self.box((px - 1, px + 4), (py - 1, py + 1), (floor, floor), "obsidian")
        self.box((px, px + 3), (py, py), (floor + 1, floor + 5), "obsidian")
        self.box((px + 1, px + 2), (py, py), (floor + 2, floor + 4), 0)
        self.set(px, py, floor + 3, "crying_obsidian")
        self.set(px + 3, py, floor + 1, "crying_obsidian")

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
                    exposed = any(self.get(x + dx, y + dy, z + dz) == 0 for dx, dy, dz in ((1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)) if y + dy >= 0)
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
        for x, y, tall in ((-14, 0, 2), (-13, 0, 4), (-13, 1, 3), (-14, 2, 2), (-13, 3, 3), (-12, 4, 2), (-11, 5, 3), (-10, 5, 3), (-9, 5, 2), (-8, 5, 3), (-7, 5, 2), (-6, 4, 3)):
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

    def tree(self, x, y, leaf, trunk=None, radius=None, thick=1):
        """Round, chunky canopy on a dark trunk: the trailer tree."""
        z = self.top(x, y) + 1
        radius = radius or float(self.rng.uniform(2.3, 4.3))
        trunk = trunk or int(3 + radius * 1.2 + self.rng.integers(0, 3))
        self.box((x, x + thick - 1), (y, y + thick - 1), (z, z + trunk), "log")
        top = z + trunk
        self._blob(x, y, top, radius, radius * 0.82, leaf)
        for _ in range(2):
            ox, oy = (self.rng.uniform(-0.7, 0.7, 2) * radius).astype(int)
            self._blob(x + ox, y + oy, top - int(radius * 0.35), radius * 0.68, radius * 0.6, leaf)

    def spruce(self, x, y):
        z = self.top(x, y) + 1
        trunk = int(self.rng.integers(8, 14))
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
                if self.rng.random() < (0.1 + max(top - 8, 0) * 0.03):
                    self.spruce(x, y)
                else:
                    self.tree(x, y, autumn[int(self.rng.integers(len(autumn)))])

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
