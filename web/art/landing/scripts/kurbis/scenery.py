"""Set dressing that is not part of the block grid: clouds, shot collections."""

import bpy
import numpy as np

from . import core, voxel
from .voxel import QUADS


def shot_collection(scene, name):
    col = bpy.data.collections.new(name)
    scene.collection.children.link(col)
    return name


def _stepped_mask(rng, nx, ny, fill=4):
    mask = np.zeros((nx, ny), bool)
    mx, my = max(nx // 6, 1), max(ny // 5, 0)
    mask[mx : nx - mx, my : ny - my] = True
    for _ in range(fill):
        w = int(rng.integers(max(nx // 3, 1), max(nx * 2 // 3, 2) + 1))
        d = int(rng.integers(1, max(ny // 2, 1) + 1))
        x, y = int(rng.integers(0, nx - w + 1)), int(rng.integers(0, ny - d + 1))
        mask[x : x + w, y : y + d] = True
    return mask


def _erode(mask):
    pad = np.pad(mask, 1)
    return mask & pad[:-2, 1:-1] & pad[2:, 1:-1] & pad[1:-1, :-2] & pad[1:-1, 2:]


def cloud(name, blocks, material, center, cells, cell=(4.0, 4.0, 1.5), seed=0, col="World", profile=(0,), mask=None):
    """A flat slab with a stepped outline. `profile` gives, per layer from the
    bottom up, how many cells that layer is pulled in, e.g. (1, 0, 1) bulges in
    the middle. `center` is the middle of the footprint at the base."""
    rng = np.random.default_rng(seed)
    nx, ny = cells
    mask = _stepped_mask(rng, nx, ny) if mask is None else mask
    grid = np.zeros((nx, ny, len(profile)), np.uint8)
    for k, inset in enumerate(profile):
        layer = mask
        for _ in range(inset):
            layer = _erode(layer)
        grid[:, :, k][layer] = blocks["cloud"]
    cx, cy, cz = center
    origin = (cx - nx * cell[0] / 2, cy - ny * cell[1] / 2, cz)
    return voxel.grid_object(name, grid, blocks, material, col, group="cloud", scale=cell, origin=origin)


def leaves(name, material, col, lo, hi, count, size=(0.1, 0.24), seed=0, keep_clear=()):
    """Small square flakes at random orientations: leaves on the wind.
    `keep_clear` lists (lo, hi) boxes no flake may sit in, e.g. in front of type."""
    rng = np.random.default_rng(seed)
    centers = rng.uniform(lo, hi, (count * 3, 3))
    for box_lo, box_hi in keep_clear:
        inside = np.all((centers >= np.asarray(box_lo)) & (centers <= np.asarray(box_hi)), axis=1)
        centers = centers[~inside]
    centers = centers[:count]
    count = len(centers)
    sizes = rng.uniform(size[0], size[1], count)
    quads = []
    for c, s in zip(centers, sizes):
        a = rng.normal(size=3)
        a /= np.linalg.norm(a)
        b = np.cross(a, rng.normal(size=3))
        b /= np.linalg.norm(b)
        quads.append([c - a * s - b * s, c + a * s - b * s, c + a * s + b * s, c - a * s + b * s])
    quads = np.asarray(quads, np.float32)
    obj = bpy.data.objects.new(name, core.mesh_from_quads(name, quads, np.zeros((count, 4, 2), np.float32)))
    obj.data.materials.append(material)
    return core.link(obj, col)


def boxes_object(name, boxes, material, col):
    """Plain untextured cuboids from (lo, size) pairs."""
    verts = [np.asarray(lo, np.float32) + QUADS[k] * np.asarray(size, np.float32) for lo, size in boxes for k in range(6)]
    verts = np.asarray(verts, np.float32)
    obj = bpy.data.objects.new(name, core.mesh_from_quads(name, verts, np.zeros((len(verts), 4, 2), np.float32)))
    obj.data.materials.append(material)
    return core.link(obj, col)


def frame_cube(name, size, thickness, material, col):
    """The twelve edges of a cube centred on the origin."""
    h, t = size / 2, thickness
    boxes = []
    for a in (-h, h - t):
        for c in (-h, h - t):
            boxes.append(((-h, a, c), (size, t, t)))
            boxes.append(((a, -h, c), (t, size, t)))
            boxes.append(((a, c, -h), (t, t, size)))
    return boxes_object(name, boxes, material, col)


def diorama(name, world, blocks, material, col, xs, ys, zs, size):
    """A slice of the world as a miniature, centred on the origin: the snapshot."""
    x0, x1 = xs[0] - world.x0, xs[1] - world.x0
    z0, z1 = zs[0] - world.z0, zs[1] - world.z0
    sub = world.grid[x0:x1, ys[0] : ys[1], z0:z1]
    s = size / max(sub.shape)
    origin = tuple(-n * s / 2 for n in sub.shape)
    return voxel.grid_object(name, sub, blocks, material, col, scale=s, origin=origin)


def strand(name, a, b, thickness, material, col):
    """A thin two-plane line between two points (fishing line, chains)."""
    a, b = np.asarray(a, np.float32), np.asarray(b, np.float32)
    axis = b - a
    side = np.cross(axis, (0.0, 0.0, 1.0))
    side = side / max(np.linalg.norm(side), 1e-6) * thickness / 2
    up = np.cross(side, axis)
    up = up / max(np.linalg.norm(up), 1e-6) * thickness / 2
    quads = np.asarray([[a - side, a + side, b + side, b - side], [a - up, a + up, b + up, b - up]], np.float32)
    obj = bpy.data.objects.new(name, core.mesh_from_quads(name, quads, np.zeros((2, 4, 2), np.float32)))
    obj.data.materials.append(material)
    return core.link(obj, col)


def debris(name, blocks, material, col, kinds, center, count, spread, size=(0.16, 0.3), seed=0):
    """Small tumbling cubes of the given block kinds: what the pumpkin knocks loose."""
    rng = np.random.default_rng(seed)
    made = []
    for i in range(count):
        grid = np.full((1, 1, 1), blocks[kinds[int(rng.integers(len(kinds)))]], np.uint8)
        s = float(rng.uniform(*size))
        obj = voxel.grid_object(f"{name}_{i}", grid, blocks, material, col, scale=s, origin=(-s / 2, -s / 2, -s / 2))
        obj.location = np.asarray(center) + rng.uniform(-1, 1, 3) * np.asarray(spread)
        obj.rotation_euler = tuple(rng.uniform(0, 6.283, 3))
        made.append(obj)
    return made


def snapshot_block(name, blocks, material, col, size=1.0):
    """The saved world as one block: 5x5x5 tiny blocks, grass over dirt over stone, with a
    hollow at the front holding a tiny lit pumpkin. Centred on the origin."""
    n = 5
    g = np.full((n, n, n), blocks["stone"], np.uint8)
    g[:, :, 4] = blocks["grass"]
    g[:, :, 3] = blocks["dirt"]
    for x, y, z, ore in ((0, 0, 0, "diamond_ore"), (4, 0, 1, "gold_ore"), (0, 0, 2, "redstone_ore"), (4, 3, 0, "diamond_ore"), (4, 0, 0, "coal_ore")):
        g[x, y, z] = blocks[ore]
    g[1:4, 0:3, 1:3] = 0  # the cave
    g[2, 1, 1] = blocks["jack_o_lantern"]
    s = size / n
    return voxel.grid_object(name, g, blocks, material, col, scale=s, origin=(-size / 2,) * 3)
