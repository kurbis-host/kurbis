"""Texture atlas, block registry and the shared voxel material.

One colour atlas plus one emission atlas keeps the whole world on a single
material, which is what the three.js runtime wants (one draw call per group).
"""

import bpy
import numpy as np

from . import core

TILE = 16
# Face order used everywhere: +X, -X, +Y (back), -Y (front, faces the viewer), +Z, -Z
PX, NX, PY, NY, PZ, NZ = range(6)


class Atlas:
    def __init__(self, tiles=32):
        self.n = tiles
        size = tiles * TILE
        self.color = np.zeros((size, size, 4), np.float32)
        self.emit = np.zeros((size, size, 4), np.float32)
        self.emit[..., 3] = 1.0
        self.index = {}
        inset = 0.02 / size
        idx = np.arange(tiles * tiles)
        col, row = idx % tiles, idx // tiles
        self.rects = np.stack(
            [col / tiles + inset, row / tiles + inset, (col + 1) / tiles - inset, (row + 1) / tiles - inset],
            axis=1,
        ).astype(np.float32)
        self.images = None

    def add(self, key, color, emit=None):
        if key in self.index:
            return self.index[key]
        i = len(self.index)
        if i >= self.n * self.n:
            raise RuntimeError("atlas full")
        row, col = divmod(i, self.n)
        ys, xs = slice(row * TILE, (row + 1) * TILE), slice(col * TILE, (col + 1) * TILE)
        self.color[ys, xs] = color
        if emit is not None:
            self.emit[ys, xs, :3] = emit[..., :3]
        self.index[key] = i
        return i

    def bake(self):
        self.images = (
            core.image_from_pixels("atlas_color", self.color, core.EXPORT / "atlas_color.png"),
            core.image_from_pixels("atlas_emit", self.emit, core.EXPORT / "atlas_emit.png"),
        )
        return self.images


def solid(rgb, alpha=1.0):
    px = np.empty((TILE, TILE, 4), np.float32)
    px[..., :3] = rgb
    px[..., 3] = alpha
    return px


class Blocks:
    """Block ids start at 1; 0 is air."""

    def __init__(self):
        self.atlas = Atlas()
        self.names = ["air"]
        self.tiles = [[0] * 6]
        self.opaque = [False]
        self.groups = ["none"]

    # -- tiles -------------------------------------------------------------
    def tex(self, name, tint=None, emit=0.0, emit_mask=None, folder="block"):
        """Atlas tile from a pack texture. `emit_mask(px) -> bool array` limits the glow."""
        key = (name, tint, emit, emit_mask.__name__ if emit_mask else None)
        if key in self.atlas.index:
            return self.atlas.index[key]
        px = core.pixels(f"{folder}/{name}.png")
        if tint is not None:
            px[..., :3] *= np.asarray(tint, np.float32)
        glow = None
        if emit:
            glow = px.copy()
            glow[..., :3] *= emit
            if emit_mask is not None:
                glow[~emit_mask(px)] = 0.0
        return self.atlas.add(key, px, glow)

    def flat(self, rgb, emit=0.0):
        key = ("solid", tuple(rgb), emit)
        px = solid(rgb)
        glow = solid(np.asarray(rgb) * emit) if emit else None
        return self.atlas.add(key, px, glow)

    def custom(self, key, px, glow=None):
        return self.atlas.add(("custom", key), px, glow)

    # -- blocks ------------------------------------------------------------
    def cube(self, name, all=None, top=None, bottom=None, side=None, front=None, opaque=True, group="opaque"):
        side = all if side is None else side
        top = all if top is None else top
        bottom = all if bottom is None else bottom
        faces = [side, side, side, side if front is None else front, top, bottom]
        self.names.append(name)
        self.tiles.append(faces)
        self.opaque.append(opaque)
        self.groups.append(group)
        return len(self.names) - 1

    def id(self, name):
        return self.names.index(name)

    def __getitem__(self, name):
        return self.id(name)

    # -- lookup tables for the mesher --------------------------------------
    @property
    def tile_lut(self):
        return np.asarray(self.tiles, np.int32)

    @property
    def opaque_lut(self):
        return np.asarray(self.opaque, bool)

    def group_lut(self, group):
        return np.asarray([g == group for g in self.groups], bool)


def saturated(px):
    """Mask for ore flecks: the coloured pixels, not the grey stone around them."""
    rgb = px[..., :3]
    return (rgb.max(-1) - rgb.min(-1)) > 0.16


def bright(px):
    return px[..., :3].max(-1) > 0.75


def voxel_material(name, atlas, emit_strength=4.0, self_illum=0.0, roughness=0.9):
    """Closest-filtered atlas, emission from the emit atlas, optional self-illumination.

    `self_illum` adds base colour to emission so type stays graphic in dark shots.
    """
    color_img, emit_img = atlas.images
    mat = bpy.data.materials.new(name)
    mat.use_nodes = True
    nt = mat.node_tree
    nt.nodes.clear()
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    bsdf = nt.nodes.new("ShaderNodeBsdfPrincipled")
    bsdf.inputs["Roughness"].default_value = roughness
    bsdf.inputs["Specular IOR Level"].default_value = 0.15
    base = nt.nodes.new("ShaderNodeTexImage")
    base.image = color_img
    base.interpolation = "Closest"
    glow = nt.nodes.new("ShaderNodeTexImage")
    glow.image = emit_img
    glow.interpolation = "Closest"
    nt.links.new(base.outputs["Color"], bsdf.inputs["Base Color"])

    scale = nt.nodes.new("ShaderNodeVectorMath")
    scale.operation = "SCALE"
    scale.name = scale.label = "EmitStrength"
    scale.inputs["Scale"].default_value = emit_strength
    nt.links.new(glow.outputs["Color"], scale.inputs[0])
    illum = nt.nodes.new("ShaderNodeVectorMath")
    illum.operation = "SCALE"
    illum.name = illum.label = "SelfIllum"
    illum.inputs["Scale"].default_value = self_illum
    nt.links.new(base.outputs["Color"], illum.inputs[0])
    add = nt.nodes.new("ShaderNodeVectorMath")
    add.operation = "ADD"
    nt.links.new(scale.outputs["Vector"], add.inputs[0])
    nt.links.new(illum.outputs["Vector"], add.inputs[1])
    nt.links.new(add.outputs["Vector"], bsdf.inputs["Emission Color"])
    bsdf.inputs["Emission Strength"].default_value = 1.0
    nt.links.new(bsdf.outputs[0], out.inputs["Surface"])
    return mat


def flat_material(name, rgb, alpha=1.0, emit=0.0, roughness=0.6, emit_rgb=None, dither=False):
    mat = bpy.data.materials.new(name)
    mat.use_nodes = True
    bsdf = mat.node_tree.nodes["Principled BSDF"]
    bsdf.inputs["Base Color"].default_value = (*rgb, 1.0)
    bsdf.inputs["Roughness"].default_value = roughness
    bsdf.inputs["Alpha"].default_value = alpha
    bsdf.inputs["Emission Color"].default_value = (*(emit_rgb or rgb), 1.0)
    bsdf.inputs["Emission Strength"].default_value = emit
    if alpha < 1.0:
        mat.surface_render_method = "DITHERED" if dither else "BLENDED"
    return mat


LEAF_TINTS = {
    "leaves_orange": (0.96, 0.47, 0.09),
    "leaves_amber": (1.0, 0.68, 0.13),
    "leaves_red": (0.80, 0.15, 0.09),
    "leaves_crimson": (0.58, 0.09, 0.08),
    "leaves_green": (0.42, 0.62, 0.20),
    "leaves_spruce": (0.20, 0.36, 0.17),
}

# Extruded type: bright face, darker sides, darkest underside (as in the trailers).
TYPE_PALETTES = {
    "gold": {"front": (1.0, 0.80, 0.11), "top": (1.0, 0.63, 0.12), "side": (0.93, 0.47, 0.07), "bottom": (0.66, 0.24, 0.05)},
    "cream": {"front": (1.0, 0.96, 0.86), "top": (0.98, 0.84, 0.66), "side": (0.78, 0.55, 0.38), "bottom": (0.45, 0.27, 0.20)},
    "plum": {"front": (0.33, 0.13, 0.25), "top": (0.46, 0.19, 0.30), "side": (0.22, 0.08, 0.18), "bottom": (0.13, 0.05, 0.12)},
    "ember": {"front": (1.0, 0.52, 0.16), "top": (0.95, 0.38, 0.10), "side": (0.70, 0.22, 0.06), "bottom": (0.38, 0.10, 0.04)},
}


def build_registry():
    b = Blocks()
    t = b.tex

    side_px = core.pixels("block/grass_block_side.png")
    grass_tint = tuple(float(v) for v in side_px[TILE - 1, 0, :3])

    b.cube("grass", top=t("grass_block_top", tint=grass_tint), side=t("grass_block_side"), bottom=t("dirt"))
    b.cube("dirt", all=t("dirt"))
    b.cube("stone", all=t("stone"))
    b.cube("andesite", all=t("andesite"))
    b.cube("gravel", all=t("gravel"))
    b.cube("tuff", all=t("tuff"))
    b.cube("cobblestone", all=t("cobblestone"))
    b.cube("mossy_cobblestone", all=t("mossy_cobblestone"))
    b.cube("moss", all=t("moss_block"))
    b.cube("deepslate", all=t("deepslate"), top=t("deepslate_top"), bottom=t("deepslate_top"))
    b.cube("bedrock", all=t("bedrock"))
    b.cube("sand", all=t("sand"))
    b.cube("snow", all=t("snow"))
    # Far mountains: flat tiles, because a 16px pattern turns to static at 200 m.
    b.cube("rock", all=b.flat((0.20, 0.17, 0.21)))
    b.cube("rock_dark", all=b.flat((0.13, 0.11, 0.15)))
    b.cube("snow_far", all=b.flat((0.90, 0.88, 0.94), emit=0.09))
    b.cube("log", side=t("oak_log"), top=t("oak_log_top"), bottom=t("oak_log_top"))
    b.cube("spruce_log", side=t("spruce_log"), top=t("spruce_log_top"), bottom=t("spruce_log_top"))
    b.cube("planks", all=t("oak_planks"))
    b.cube("obsidian", all=t("obsidian"))
    b.cube("crying_obsidian", all=t("crying_obsidian", emit=0.5, emit_mask=saturated))
    b.cube("stone_bricks", all=t("stone_bricks"))
    b.cube("mossy_stone_bricks", all=t("mossy_stone_bricks"))
    b.cube("cracked_stone_bricks", all=t("cracked_stone_bricks"))
    b.cube("chiseled_stone_bricks", all=t("chiseled_stone_bricks"))
    b.cube("blackstone", side=t("blackstone"), top=t("blackstone_top"), bottom=t("blackstone_top"))
    b.cube("polished_blackstone_bricks", all=t("polished_blackstone_bricks"))
    b.cube("gilded_blackstone", all=t("gilded_blackstone", emit=0.25, emit_mask=saturated))
    b.cube("deepslate_bricks", all=t("deepslate_bricks"))
    b.cube("nether_bricks", all=t("nether_bricks"))
    b.cube("netherrack", all=t("netherrack"))
    b.cube("magma", all=t("magma", emit=0.7, emit_mask=saturated))
    b.cube("gold_block", all=t("gold_block"))
    b.cube("lamp", all=t("redstone_lamp_on", emit=1.0))
    b.cube("froglight", side=t("ochre_froglight_side", emit=1.0), top=t("ochre_froglight_top", emit=1.0), bottom=t("ochre_froglight_top", emit=1.0))
    b.cube("command_block", side=t("command_block_side"), front=t("command_block_front", emit=0.35, emit_mask=saturated), top=t("command_block_back"), bottom=t("command_block_back"))
    b.cube("smithing_table", side=t("smithing_table_side"), front=t("smithing_table_front"), top=t("smithing_table_top"), bottom=t("smithing_table_bottom"))
    b.cube("bone", side=t("bone_block_side"), top=t("bone_block_top"), bottom=t("bone_block_top"))
    b.cube("crafting_table", side=t("crafting_table_side"), front=t("crafting_table_front"), top=t("crafting_table_top"), bottom=t("oak_planks"))
    b.cube("furnace", side=t("furnace_side"), front=t("furnace_front_on", emit=1.0, emit_mask=saturated), top=t("furnace_top"), bottom=t("furnace_top"))
    b.cube("bookshelf", side=t("bookshelf"), top=t("oak_planks"), bottom=t("oak_planks"))
    b.cube("barrel", side=t("barrel_side"), top=t("barrel_top"), bottom=t("barrel_bottom"))
    b.cube("glowstone", all=t("glowstone", emit=0.9))
    b.cube("lava", all=t("lava_still", emit=1.0))
    b.cube("pumpkin", side=t("pumpkin_side"), top=t("pumpkin_top"), bottom=t("pumpkin_top"))
    b.cube("jack_o_lantern", side=t("pumpkin_side"), front=t("jack_o_lantern", emit=1.0, emit_mask=bright), top=t("pumpkin_top"), bottom=t("pumpkin_top"))
    for ore, glow in (("coal", 0.0), ("iron", 0.0), ("copper", 0.06), ("gold", 0.22), ("redstone", 0.3), ("lapis", 0.2), ("diamond", 0.3)):
        b.cube(f"{ore}_ore", all=t(f"{ore}_ore", emit=glow, emit_mask=saturated))
    b.cube("deepslate_diamond_ore", all=t("deepslate_diamond_ore", emit=0.3, emit_mask=saturated))
    b.cube("deepslate_gold_ore", all=t("deepslate_gold_ore", emit=0.22, emit_mask=saturated))
    b.cube("deepslate_redstone_ore", all=t("deepslate_redstone_ore", emit=0.3, emit_mask=saturated))
    for name, tint in LEAF_TINTS.items():
        b.cube(name, all=t("oak_leaves", tint=tint))
    b.cube("leaves_blossom", all=t("flowering_azalea_leaves"))
    b.cube("leaves_cherry", all=t("cherry_leaves"))
    b.cube("cherry_log", side=t("cherry_log"), top=t("cherry_log_top"), bottom=t("cherry_log_top"))
    b.cube("water", all=b.flat((0.16, 0.42, 0.86)), opaque=False, group="water")
    b.cube("cloud", all=b.flat((1.0, 0.93, 0.90)), opaque=False, group="cloud")
    for name, pal in TYPE_PALETTES.items():
        b.cube(
            f"type_{name}",
            side=b.flat(pal["side"]),
            front=b.flat(pal["front"]),
            top=b.flat(pal["top"]),
            bottom=b.flat(pal["bottom"]),
            group="type",
        )
    return b
