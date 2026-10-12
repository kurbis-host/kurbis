"""Kurbis Block: our own chunky voxel display face for in-world type.

Glyphs are 8 rows tall. Strokes are two cells thick and counters one cell, so
letters read as solid slabs with thin cuts. Lowercase maps to uppercase.

Below it, a small pixel face for the interface (chat, tooltips, toasts): five
columns by seven rows, with lower case, set proportionally.
"""

import bpy
import numpy as np

from . import core, voxel

ROWS = 8

_G = {
    "A": ["######", "######", "##..##", "######", "######", "##..##", "##..##", "##..##"],
    "B": ["#####.", "######", "##..##", "#####.", "######", "##..##", "######", "#####."],
    "C": ["######", "######", "##....", "##....", "##....", "##....", "######", "######"],
    "D": ["#####.", "######", "##..##", "##..##", "##..##", "##..##", "######", "#####."],
    "E": ["######", "######", "##....", "#####.", "#####.", "##....", "######", "######"],
    "F": ["######", "######", "##....", "#####.", "#####.", "##....", "##....", "##...."],
    "G": ["######", "######", "##....", "##.###", "##.###", "##..##", "######", "######"],
    "H": ["##..##", "##..##", "##..##", "######", "######", "##..##", "##..##", "##..##"],
    "I": ["##", "##", "##", "##", "##", "##", "##", "##"],
    "J": ["....##", "....##", "....##", "....##", "....##", "##..##", "######", "######"],
    "K": ["##..##", "##..##", "##.###", "#####.", "#####.", "##.###", "##..##", "##..##"],
    "L": ["##....", "##....", "##....", "##....", "##....", "##....", "######", "######"],
    "M": ["##....##", "###..###", "########", "########", "##.##.##", "##....##", "##....##", "##....##"],
    "N": ["##...##", "###..##", "####.##", "#######", "##.####", "##..###", "##...##", "##...##"],
    "O": ["######", "######", "##..##", "##..##", "##..##", "##..##", "######", "######"],
    "P": ["######", "######", "##..##", "######", "######", "##....", "##....", "##...."],
    "Q": ["######", "######", "##..##", "##..##", "##..##", "##.###", "######", "#####."],
    "R": ["######", "######", "##..##", "######", "#####.", "##.###", "##..##", "##..##"],
    "S": ["######", "######", "##....", "######", "######", "....##", "######", "######"],
    "T": ["######", "######", "..##..", "..##..", "..##..", "..##..", "..##..", "..##.."],
    "U": ["##..##", "##..##", "##..##", "##..##", "##..##", "##..##", "######", "######"],
    "V": ["##..##", "##..##", "##..##", "##..##", "##..##", "######", ".####.", "..##.."],
    "W": ["##....##", "##....##", "##....##", "##.##.##", "########", "########", "###..###", "##....##"],
    "X": ["##..##", "##..##", "######", ".####.", ".####.", "######", "##..##", "##..##"],
    "Y": ["##..##", "##..##", "##..##", "######", ".####.", "..##..", "..##..", "..##.."],
    "Z": ["######", "######", "...###", "..###.", ".###..", "###...", "######", "######"],
    "0": ["######", "######", "##..##", "##.###", "###.##", "##..##", "######", "######"],
    "1": [".##.", "###.", "###.", ".##.", ".##.", ".##.", "####", "####"],
    "2": ["######", "######", "....##", "######", "######", "##....", "######", "######"],
    "3": ["######", "######", "....##", ".#####", ".#####", "....##", "######", "######"],
    "4": ["##..##", "##..##", "##..##", "######", "######", "....##", "....##", "....##"],
    "5": ["######", "######", "##....", "######", "######", "....##", "######", "######"],
    "6": ["######", "######", "##....", "######", "######", "##..##", "######", "######"],
    "7": ["######", "######", "....##", "...##.", "..##..", "..##..", "..##..", "..##.."],
    "8": ["######", "######", "##..##", "######", "######", "##..##", "######", "######"],
    "9": ["######", "######", "##..##", "######", "######", "....##", "######", "######"],
    ".": ["..", "..", "..", "..", "..", "..", "##", "##"],
    ",": ["..", "..", "..", "..", "..", "##", "##", "#."],
    "!": ["##", "##", "##", "##", "##", "..", "##", "##"],
    "?": ["######", "######", "....##", "..####", "..###.", "......", "..##..", "..##.."],
    "'": ["##", "##", "#.", "..", "..", "..", "..", ".."],
    ":": ["..", "##", "##", "..", "..", "##", "##", ".."],
    "-": ["....", "....", "....", "####", "####", "....", "....", "...."],
    "_": ["......", "......", "......", "......", "......", "......", "######", "######"],
    "/": ["...##", "...##", "..##.", "..##.", ".##..", ".##..", "##...", "##..."],
    "+": ["......", "..##..", "..##..", "######", "######", "..##..", "..##..", "......"],
    ">": ["##....", "###...", ".###..", "..###.", "..###.", ".###..", "###...", "##...."],
    "\u2193": ["..##..", "..##..", "..##..", "..##..", "######", ".####.", "..##..", "......"],  # down arrow
    " ": ["...", "...", "...", "...", "...", "...", "...", "..."],
}
GLYPHS = {k: np.array([[c == "#" for c in row] for row in rows], bool) for k, rows in _G.items()}


def line_mask(text, tracking=1):
    parts = []
    for ch in text:
        glyph = GLYPHS.get(ch.upper(), GLYPHS["?"])
        parts.append(glyph)
        parts.append(np.zeros((ROWS, tracking), bool))
    return np.concatenate(parts[:-1], axis=1) if parts else np.zeros((ROWS, 0), bool)


def text_mask(lines, tracking=1, leading=2, align="center"):
    """2D mask, row 0 at the top."""
    masks = [line_mask(line, tracking) for line in lines]
    width = max(m.shape[1] for m in masks)
    rows = []
    for i, m in enumerate(masks):
        gap = width - m.shape[1]
        left = {"left": 0, "right": gap}.get(align, gap // 2)
        rows.append(np.pad(m, ((0, 0), (left, gap - left))))
        if i < len(masks) - 1:
            rows.append(np.zeros((leading, width), bool))
    return np.concatenate(rows, axis=0)


def text_object(name, lines, blocks, material, col="Type", palette="gold", cell=0.25, depth=3, **layout):
    """Extruded type. Origin at the bottom centre of the front face; front faces -Y."""
    if isinstance(lines, str):
        lines = [lines]
    mask = text_mask(lines, **layout)
    rows, cols = mask.shape
    grid = np.zeros((cols, depth, rows), np.uint8)
    filled = np.broadcast_to(mask[::-1].T[:, None, :], grid.shape)
    grid[filled] = blocks[f"type_{palette}"]
    obj = voxel.grid_object(name, grid, blocks, material, col, group="type", scale=cell, origin=(-cols * cell / 2, 0, 0))
    core.note(obj, kind="type", lines=list(lines), palette=palette, cell=cell, depth=depth, layout=layout, material=material.name)
    return obj


def keyline(name, lines, material, col="Type", cell=0.25, width=0.2, **layout):
    """A thin dark rim round every letter of some type, for where the ground is as
    bright as the letters themselves. One flat piece that lies just behind the
    letters' faces: every run of cells in a row, grown by `width` cells all round.
    Same origin as `text_object`, so it can simply be its child."""
    if isinstance(lines, str):
        lines = [lines]
    mask = text_mask(lines, **layout)[::-1]  # row 0 at the bottom
    rows, cols = mask.shape
    quads = []
    for z in range(rows):
        edges = np.flatnonzero(np.diff(np.concatenate(([False], mask[z], [False])).astype(np.int8)))
        for x0, x1 in zip(edges[::2], edges[1::2]):
            quads.append([(x0 - width, 0.0, z - width), (x1 + width, 0.0, z - width), (x1 + width, 0.0, z + 1 + width), (x0 - width, 0.0, z + 1 + width)])
    verts = (np.asarray(quads, np.float32) + np.array((-cols / 2, 0.06, 0.0), np.float32)) * cell
    obj = bpy.data.objects.new(name, core.mesh_from_quads(name, verts, np.zeros((len(quads), 4, 2), np.float32)))
    obj.data.materials.append(material)
    obj.visible_shadow = False
    core.note(obj, kind="rim", lines=list(lines), cell=cell, width=width, layout=layout)
    return core.link(obj, col)


# -- the small pixel face ----------------------------------------------------------
# The classic five-by-seven dot-matrix shapes, one byte per column, top row in the
# lowest bit. A starting point for our own interface face.
_PIXEL = (
    "00 00 00 00 00|00 00 5F 00 00|00 07 00 07 00|14 7F 14 7F 14|24 2A 7F 2A 12|23 13 08 64 62|36 49 55 22 50|00 05 03 00 00|"
    "00 1C 22 41 00|00 41 22 1C 00|08 2A 1C 2A 08|08 08 3E 08 08|00 50 30 00 00|08 08 08 08 08|00 60 60 00 00|20 10 08 04 02|"
    "3E 51 49 45 3E|00 42 7F 40 00|42 61 51 49 46|21 41 45 4B 31|18 14 12 7F 10|27 45 45 45 39|3C 4A 49 49 30|01 71 09 05 03|"
    "36 49 49 49 36|06 49 49 29 1E|00 36 36 00 00|00 56 36 00 00|00 08 14 22 41|14 14 14 14 14|41 22 14 08 00|02 01 51 09 06|"
    "32 49 79 41 3E|7E 11 11 11 7E|7F 49 49 49 36|3E 41 41 41 22|7F 41 41 22 1C|7F 49 49 49 41|7F 09 09 01 01|3E 41 41 51 32|"
    "7F 08 08 08 7F|00 41 7F 41 00|20 40 41 3F 01|7F 08 14 22 41|7F 40 40 40 40|7F 02 04 02 7F|7F 04 08 10 7F|3E 41 41 41 3E|"
    "7F 09 09 09 06|3E 41 51 21 5E|7F 09 19 29 46|46 49 49 49 31|01 01 7F 01 01|3F 40 40 40 3F|1F 20 40 20 1F|7F 20 18 20 7F|"
    "63 14 08 14 63|03 04 78 04 03|61 51 49 45 43|00 00 7F 41 41|02 04 08 10 20|41 41 7F 00 00|04 02 01 02 04|40 40 40 40 40|"
    "00 01 02 04 00|20 54 54 54 78|7F 48 44 44 38|38 44 44 44 20|38 44 44 48 7F|38 54 54 54 18|08 7E 09 01 02|08 14 54 54 3C|"
    "7F 08 04 04 78|00 44 7D 40 00|20 40 44 3D 00|00 7F 10 28 44|00 41 7F 40 00|7C 04 18 04 78|7C 08 04 04 78|38 44 44 44 38|"
    "7C 14 14 14 08|08 14 14 18 7C|7C 08 04 04 08|48 54 54 54 20|04 3F 44 40 20|3C 40 40 20 7C|1C 20 40 20 1C|3C 40 30 40 3C|"
    "44 28 10 28 44|0C 50 50 50 3C|44 64 54 4C 44|00 08 36 41 00|00 00 7F 00 00|00 41 36 08 00|08 04 08 10 08"
)
PIXEL_ROWS = 7


def _pixel_glyphs():
    glyphs = {}
    for code, columns in enumerate(_PIXEL.split("|"), start=32):
        grid = np.array([[(int(column, 16) >> row) & 1 for column in columns.split()] for row in range(PIXEL_ROWS)], bool)
        used = np.flatnonzero(grid.any(axis=0))
        glyphs[chr(code)] = grid[:, used[0] : used[-1] + 1] if len(used) else np.zeros((PIXEL_ROWS, 3), bool)
    return glyphs


PIXEL = _pixel_glyphs()


def pixel_mask(text):
    """One line in the small face: a mask seven rows high, row 0 at the top."""
    parts = []
    for ch in text:
        parts.append(PIXEL.get(ch, PIXEL["?"]))
        parts.append(np.zeros((PIXEL_ROWS, 1), bool))
    return np.concatenate(parts[:-1], axis=1) if parts else np.zeros((PIXEL_ROWS, 0), bool)
