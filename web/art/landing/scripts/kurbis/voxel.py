"""Numpy voxel mesher: a 3D id grid becomes culled, atlas-mapped quads."""

import bpy
import numpy as np

from . import core
from .blocks import NX, NY, NZ, PX, PY, PZ

DIRS = ((1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1))
# Corner order is counter-clockwise from outside, starting bottom-left as seen
# from outside with Z up, so textures are upright and unmirrored on every side.
QUADS = np.array(
    [
        [(1, 0, 0), (1, 1, 0), (1, 1, 1), (1, 0, 1)],
        [(0, 1, 0), (0, 0, 0), (0, 0, 1), (0, 1, 1)],
        [(1, 1, 0), (0, 1, 0), (0, 1, 1), (1, 1, 1)],
        [(0, 0, 0), (1, 0, 0), (1, 0, 1), (0, 0, 1)],
        [(0, 0, 1), (1, 0, 1), (1, 1, 1), (0, 1, 1)],
        [(0, 1, 0), (1, 1, 0), (1, 0, 0), (0, 0, 0)],
    ],
    dtype=np.float32,
)


def quads(grid, blocks, group="opaque", closed=()):
    """Visible faces of `group` blocks. `closed` lists directions whose outer
    world boundary is treated as solid (hidden), e.g. the sides and the bottom."""
    nx, ny, nz = grid.shape
    pad = np.pad(grid, 1)
    select = blocks.group_lut(group)[grid]
    opaque = blocks.opaque_lut
    tiles = blocks.tile_lut
    rects = blocks.atlas.rects
    verts, uvs = [], []
    for k, (dx, dy, dz) in enumerate(DIRS):
        nb = pad[1 + dx : 1 + dx + nx, 1 + dy : 1 + dy + ny, 1 + dz : 1 + dz + nz]
        vis = select & ~opaque[nb] & (nb != grid)
        if k in closed:
            edge = [slice(None)] * 3
            edge[k // 2] = -1 if k % 2 == 0 else 0
            vis[tuple(edge)] = False
        idx = np.argwhere(vis)
        if not len(idx):
            continue
        verts.append(idx[:, None, :].astype(np.float32) + QUADS[k][None])
        r = rects[tiles[grid[vis], k]]
        uvs.append(np.stack([r[:, [0, 1]], r[:, [2, 1]], r[:, [2, 3]], r[:, [0, 3]]], axis=1))
    if not verts:
        return np.zeros((0, 4, 3), np.float32), np.zeros((0, 4, 2), np.float32)
    return np.concatenate(verts), np.concatenate(uvs)


def grid_object(name, grid, blocks, material, col, group="opaque", scale=1.0, origin=(0, 0, 0), closed=()):
    """Mesh object for one material group. Vertices are `index * scale + origin`."""
    verts, uvs = quads(grid, blocks, group, closed)
    if not len(verts):
        return None
    verts = verts * np.asarray(scale, np.float32) + np.asarray(origin, np.float32)
    obj = bpy.data.objects.new(name, core.mesh_from_quads(name, verts, uvs))
    obj.data.materials.append(material)
    return core.link(obj, col)


ALL_CLOSED_BUT_FRONT_AND_TOP = (PX, NX, PY, NZ)
__all__ = ["quads", "grid_object", "ALL_CLOSED_BUT_FRONT_AND_TOP", "PX", "NX", "PY", "NY", "PZ", "NZ"]
