"""Writing the film out for the browser.

The page draws the same world with its own renderer and plays its own, TypeScript
choreography. What it needs from here is everything that is a thing rather than a
behaviour:

  records   the big voxel meshes as one small record per block face, with light
            baked in (see `light`). No decoder: the bytes are the buffers.
  models    everything small as a glb, through Blender's own exporter, with names
            and hierarchy and without images.
  layout    per frame shape: the camera's keys, where type and flakes stand, lights,
            haze, and the notes the toolkit left on things (`core.note`).
  scene     what both shapes share: sky, palettes, glyphs, constants.
  reference camera and pumpkin sampled along the scroll, for the page's parity tests.

Everything drawn from the texture pack lands in web/public/landing, which git ignores.
"""

import gzip
import json
import shutil
import struct

import bpy
import numpy as np
from mathutils import Vector

from . import anim, core, font, voxel
from . import blocks as blk

OUT = core.ROOT.parents[1] / "public" / "landing"
CHUNK = 32  # faces are grouped into cubes this many blocks across, to cull and to load by
BIG = ("Terrain", "Leaves", "Water", "Nether")  # written as records, not into the glb
AO = np.array((0.5, 0.66, 0.82, 1.0), np.float32)  # how dark a corner is with 3, 2, 1, 0 neighbours standing in it


# -- faces ---------------------------------------------------------------------------
def faces(grid, blocks, group="opaque", closed=()):
    """The visible faces of `group` blocks, as the mesher finds them, but as
    (cell, direction, tile) instead of corners and atlas coordinates."""
    nx, ny, nz = grid.shape
    pad = np.pad(grid, 1)
    select = blocks.group_lut(group)[grid]
    opaque, tiles = blocks.opaque_lut, blocks.tile_lut
    cells, which, tile = [], [], []
    for k, (dx, dy, dz) in enumerate(voxel.DIRS):
        nb = pad[1 + dx : 1 + dx + nx, 1 + dy : 1 + dy + ny, 1 + dz : 1 + dz + nz]
        vis = select & ~opaque[nb] & (nb != grid)
        if k in closed:
            edge = [slice(None)] * 3
            edge[k // 2] = -1 if k % 2 == 0 else 0
            vis[tuple(edge)] = False
        idx = np.argwhere(vis)
        cells.append(idx)
        which.append(np.full(len(idx), k, np.int16))
        tile.append(tiles[grid[vis], k])
    return np.concatenate(cells).astype(np.int32), np.concatenate(which), np.concatenate(tile).astype(np.int32)


# -- light ---------------------------------------------------------------------------
def _spread(level, passes, dims, steps=15):
    """Let light flow from cell to cell, losing one level a step (and `dims` more where a
    cell is leaves), through every cell that `passes` light."""
    cost = (1 + dims).astype(np.int16)
    level = level.astype(np.int16)
    for _ in range(steps):
        best = level.copy()
        for axis in range(3):
            for back in (False, True):
                moved = np.zeros_like(level)
                src = [slice(None)] * 3
                dst = [slice(None)] * 3
                src[axis], dst[axis] = (slice(1, None), slice(None, -1)) if back else (slice(None, -1), slice(1, None))
                moved[tuple(dst)] = level[tuple(src)]
                np.maximum(best, moved - cost, out=best)
        best[~passes] = 0
        if np.array_equal(best, level):
            break
        level = best
    return level.astype(np.uint8)


def levels(grid, blocks, sky=True, front=None):
    """Two fields of light over a grid, each 0..15 a cell, as the game counts it:
    daylight coming down from the open sky (and, with `front`, in through the cut
    face: front(z) gives the level standing outside it at each height), and the glow
    of blocks that shine. Leaves let light through but dim it."""
    opaque = blocks.opaque_lut[grid]
    leafy = blocks.group_lut("leaves")[grid]
    passes = ~opaque
    day = np.zeros(grid.shape, np.uint8)
    if sky:
        roofed = np.maximum.accumulate(opaque[:, :, ::-1], axis=2)[:, :, ::-1]
        day[~roofed] = 15
        if front is not None:
            outside = np.asarray([front(z) for z in range(grid.shape[2])], np.uint8)
            day[:, 0, :] = np.maximum(day[:, 0, :], np.where(passes[:, 0, :], np.maximum(outside[None, :], 1) - 1, 0))
        day = _spread(day, passes, leafy)
    emit = blocks.atlas.emit[..., :3].max(axis=2)  # how much each tile shines
    n = blocks.atlas.n
    shine = emit.reshape(n, blk.TILE, n, blk.TILE).mean(axis=(1, 3)).ravel()
    bright = shine[blocks.tile_lut].max(axis=1) > 0.2  # the blocks that are lamps: lava, glowstone, lanterns
    glow = np.where(bright[grid], 15, 0).astype(np.uint8)
    lit = _spread(glow, passes | bright[grid], leafy)
    lit[bright[grid]] = 15
    return day, lit, opaque | leafy


def _corners(k):
    """For each corner of a face in direction k: the two sideways steps (in the plane in
    front of the face) that lead to the cells meeting at that corner."""
    axis = k // 2
    out = []
    for corner in voxel.QUADS[k]:
        steps = []
        for t in range(3):
            if t != axis:
                step = [0, 0, 0]
                step[t] = 1 if corner[t] > 0.5 else -1
                steps.append(step)
        out.append(steps)
    return out


def light(cells, which, day, lit, solid, sun=None, key=None, sun_floor=None):
    """Smooth light for every corner of every face: (n, 16) bytes, four each of
    daylight, glow, direct sun, and the key suns. A corner takes the mean of the four
    cells that meet there in front of the face, and is darkened by how many of them are
    solid, the way the game shades its corners. `sun` and `key` are directions to a
    sun: corners that face it and see it get that byte (leaves in the way dim it).
    The previs's key suns all stand in much the same place, so one direction does for
    the three of them."""
    pad = lambda field: np.pad(field, 2)  # noqa: E731
    day_p, lit_p, solid_p = pad(day).astype(np.float32), pad(lit).astype(np.float32), pad(solid)
    out = np.zeros((len(cells), 16), np.uint8)
    dirs = np.asarray(voxel.DIRS, np.int32)
    for k in range(6):
        pick = np.flatnonzero(which == k)
        if not len(pick):
            continue
        front = cells[pick] + dirs[k] + 2  # the cell the face looks into, in padded indices
        fx, fy, fz = front.T
        d0, l0 = day_p[fx, fy, fz], lit_p[fx, fy, fz]
        for j, (a, b) in enumerate(_corners(k)):
            a, b = np.asarray(a), np.asarray(b)
            s1, s2, sc = front + a, front + b, front + a + b
            o1, o2 = solid_p[s1[:, 0], s1[:, 1], s1[:, 2]], solid_p[s2[:, 0], s2[:, 1], s2[:, 2]]
            oc = solid_p[sc[:, 0], sc[:, 1], sc[:, 2]] | (o1 & o2)
            shade = AO[3 - (o1.astype(np.int8) + o2 + oc)]
            for field, here, column in ((day_p, d0, j), (lit_p, l0, 4 + j)):
                v1 = np.where(o1, here, field[s1[:, 0], s1[:, 1], s1[:, 2]])
                v2 = np.where(o2, here, field[s2[:, 0], s2[:, 1], s2[:, 2]])
                vc = np.where(oc, here, field[sc[:, 0], sc[:, 1], sc[:, 2]])
                out[pick, column] = np.clip((here + v1 + v2 + vc) / 4.0 / 15.0 * shade * 255.0, 0, 255).astype(np.uint8)
    if sun is not None:
        out[:, 8:12] = _sun(cells, which, solid, np.asarray(sun, np.float32), sun_floor)
    if key is not None:
        out[:, 12:16] = _sun(cells, which, solid, np.asarray(key, np.float32), sun_floor, reach=160)
    return out


def _sun(cells, which, solid, sun, floor, reach=520):
    """Which corners see the sun: a ray from each, a little in from the corner and off
    the face, marched through the grid half a block at a time."""
    opaque_or_leaf = solid
    shape = np.asarray(solid.shape)
    out = np.zeros((len(cells), 4), np.uint8)
    dirs = np.asarray(voxel.DIRS, np.float32)
    for k in range(6):
        facing = float(dirs[k] @ sun)
        if facing <= 0.02:
            continue
        pick = np.flatnonzero((which == k) & ((cells[:, 2] >= floor) if floor is not None else True))
        if not len(pick):
            continue
        centre = cells[pick].astype(np.float32) + voxel.QUADS[k].mean(axis=0)
        for j, corner in enumerate(voxel.QUADS[k]):
            at = cells[pick].astype(np.float32) + corner
            at = at + (centre - at) * 0.12 + dirs[k] * 0.05
            seen = np.ones(len(pick), np.float32)
            alive = np.ones(len(pick), bool)
            for step in range(1, reach):
                p = at + sun * (0.5 * step)
                cell = np.floor(p).astype(np.int32)
                inside = np.all((cell >= 0) & (cell < shape), axis=1) & alive
                if not inside.any():
                    break
                hit = np.zeros(len(pick), bool)
                c = cell[inside]
                hit[inside] = opaque_or_leaf[c[:, 0], c[:, 1], c[:, 2]]
                seen[hit] *= 0.45
                alive &= inside & (seen > 0.02)
            out[pick, j] = np.clip(seen * 255.0, 0, 255).astype(np.uint8)
    return out


# -- records -------------------------------------------------------------------------
def write_records(path, cells, which, tile, shade, origin):
    """One file of faces: a short JSON header, then three plain arrays the page hands
    to the GPU as they are.

        'KVX1', header length (uint32), header (JSON, padded to four bytes)
        int16  x, y, z, direction      per face (block coordinates in the world)
        uint16 tile, 0                 per face
        uint8  daylight x4, glow x4, sun x4, key suns x4   per face, one per corner

    Faces are sorted by the CHUNK-sized cube they are in; the header lists each cube's
    bounds and its run of faces.

    The file is written packed with gzip (a tenth the size): a server compresses the
    text it sends but not bytes like these, so the page unpacks them itself."""
    world = cells + np.asarray(origin, np.int32)
    cube = np.floor_divide(world, CHUNK)
    order = np.lexsort((tile, which, cube[:, 0], cube[:, 1], cube[:, 2]))
    world, which, tile, shade, cube = world[order], which[order], tile[order], shade[order], cube[order]
    change = np.flatnonzero(np.any(np.diff(cube, axis=0) != 0, axis=1)) + 1
    starts = np.concatenate(([0], change, [len(world)]))
    chunks = []
    for first, last in zip(starts[:-1], starts[1:]):
        if last > first:
            lo, hi = world[first:last].min(axis=0), world[first:last].max(axis=0) + 1
            chunks.append([*map(int, lo), *map(int, hi), int(first), int(last - first)])
    a = np.zeros((len(world), 4), "<i2")
    a[:, :3], a[:, 3] = world, which
    b = np.zeros((len(world), 2), "<u2")
    b[:, 0] = tile
    header = json.dumps({"count": int(len(world)), "chunk": CHUNK, "chunks": chunks}, separators=(",", ":")).encode()
    header += b" " * (-len(header) % 4)
    path.parent.mkdir(parents=True, exist_ok=True)
    data = b"KVX1" + struct.pack("<I", len(header)) + header + a.tobytes() + b.tobytes() + np.ascontiguousarray(shade, np.uint8).tobytes()
    path.write_bytes(gzip.compress(data, compresslevel=9, mtime=0))
    return {"file": path.name, "count": int(len(world)), "chunks": len(chunks), "bytes": path.stat().st_size, "unpacked": len(data)}


# -- models --------------------------------------------------------------------------
def skipped(obj):
    """Things the glb leaves out: the big meshes (records), what the page builds itself
    (type, flakes, haze), and the previs's own interface and covers."""
    if obj.name in BIG or obj.name.startswith(("Hud", "Cover_", "Probe_")):
        return True
    return core.noted(obj).get("kind") in ("type", "rim", "scrim", "flakes", "fog")


def write_glb(path, objects):
    """Meshes and empties, with their names, parents and notes, and no images: the page
    gives every material its texture by the material's name."""
    for obj in bpy.context.view_layer.objects:
        obj.select_set(False)
    for obj in objects:
        obj.hide_set(False)
        obj.select_set(True)
        # How far each point of a plant is from its root (life.wind) travels as "_sway":
        # the exporter only takes attributes whose names begin with an underscore.
        if obj.type == "MESH" and "sway" in obj.data.attributes and "_sway" not in obj.data.attributes:
            values = np.zeros(len(obj.data.vertices), np.float32)
            obj.data.attributes["sway"].data.foreach_get("value", values)
            obj.data.attributes.new("_sway", "FLOAT", "POINT").data.foreach_set("value", values)
    path.parent.mkdir(parents=True, exist_ok=True)
    bpy.ops.export_scene.gltf(
        filepath=str(path), export_format="GLB", use_selection=True, export_yup=False, export_apply=False,
        export_materials="PLACEHOLDER", export_animations=False, export_extras=True, export_cameras=False, export_lights=False,
        export_texcoords=True, export_normals=True, export_attributes=True,
    )
    return {"file": path.name, "objects": len(objects), "bytes": path.stat().st_size}


# -- describing things ---------------------------------------------------------------
def _list(value):
    return [round(float(v), 5) for v in value]


def node(obj):
    """Where an object stands under its parent, and what the toolkit said about it."""
    out = {"name": obj.name, "at": _list(obj.location), "turn": _list(obj.rotation_euler), "size": _list(obj.scale)}
    if obj.parent is not None:
        out["parent"] = obj.parent.name
    if tuple(obj.color) != (1.0, 1.0, 1.0, 1.0):
        out["color"] = _list(obj.color)
    said = core.noted(obj)
    if said:
        out["note"] = said
    if obj.type == "MESH" and obj.data.materials and obj.data.materials[0] is not None:
        out["material"] = obj.data.materials[0].name
    return out


def lamp(obj):
    data = obj.data
    out = {"name": obj.name, "kind": data.type.lower(), "at": _list(obj.matrix_world.translation), "color": _list(data.color), "energy": round(float(data.energy), 4)}
    if data.type == "POINT":
        out["radius"] = round(float(data.shadow_soft_size), 4)
    if data.type in ("SUN", "AREA", "SPOT"):
        out["toward"] = _list(obj.matrix_world.to_3x3() @ Vector((0.0, 0.0, -1.0)))
    if data.type == "AREA":
        out["size"] = round(float(data.size), 4)
    if obj.parent is not None:
        out["parent"] = obj.parent.name
        out["at"] = _list(obj.location)
    return out


def track(t):
    return {"t": _list(t.t), "v": [_list(v) for v in t.v], "ends": getattr(t, "ends", "rest")}


def shot(s):
    return {"s0": s.s0, "s1": s.s1, "tracks": {name: track(t) for name, t in s.tracks.items()}}


def materials():
    return {mat.name: core.noted(mat) for mat in bpy.data.materials if core.noted(mat)}


def glyphs():
    """Both faces as rows of '#' and '.', top row first."""
    big = {ch: ["".join("#" if on else "." for on in row) for row in grid] for ch, grid in font.GLYPHS.items()}
    small = {ch: ["".join("#" if on else "." for on in row) for row in grid] for ch, grid in font.PIXEL.items()}
    return {"block": {"rows": font.ROWS, "glyphs": big}, "pixel": {"rows": font.PIXEL_ROWS, "glyphs": small}}


def plain(value):
    """Constants as JSON: vectors and tuples become lists, dict keys become strings."""
    if isinstance(value, dict):
        return {str(k): plain(v) for k, v in value.items()}
    if isinstance(value, (list, tuple, Vector, np.ndarray)):
        return [plain(v) for v in value]
    if isinstance(value, (np.floating, float)):
        return round(float(value), 6)
    if isinstance(value, (np.integer, int)) and not isinstance(value, bool):
        return int(value)
    return value


def copy_textures(used):
    """The pictures the materials name, side by side under tex/: pack textures by their
    own path with the slashes flattened, pictures made in code by their name."""
    out = OUT / "tex"
    out.mkdir(parents=True, exist_ok=True)
    names = {}
    for said in used.values():
        for key in ("image", "emit_image"):
            rel = said.get(key)
            if rel:
                names[rel] = rel.replace("/", "_")
                shutil.copyfile(core.TEX / rel, out / names[rel])
        if said.get("made"):
            names["@" + said["made"]] = said["made"] + ".png"
            shutil.copyfile(core.EXPORT / (said["made"] + ".png"), out / (said["made"] + ".png"))
    return names


# -- reference -----------------------------------------------------------------------
def reference(shots, actors, end, step=0.02):
    """The camera and the actors sampled along the scroll, straight from this timeline:
    what the page's own must reproduce."""
    shots = sorted(shots, key=lambda s: s.s0)
    out = {"step": step, "camera": [], "actors": {name: [] for name in actors}}
    count = int(round(end / step)) + 1
    for i in range(count):
        s = min(i * step, end)
        state = anim._shot_at(shots, s)(s)
        quat = anim.look_quat(state["eye"], state["target"], state["up"], state["roll"])
        out["camera"].append([round(s, 4), *_list(state["eye"]), *_list(quat), round(float(state["lens"]), 4)])
        for name, actor in actors.items():
            p = actor.state(s)
            out["actors"][name].append([round(s, 4), *_list(p["pos"]), round(float(p["yaw"]), 4), *_list(p["tilt"]), round(float(p["squash"]), 5), round(float(p["grow"]), 5), 1 if p["calm"] else 0])
    return out
