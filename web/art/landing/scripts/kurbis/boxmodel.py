"""Box models with Minecraft-style box UVs: players, held items, small props.

Characters are rigid parts under pivots (no skinning), which exports to glTF as
plain animated nodes. Limbs are split at the elbow and knee so they can bend.
Models face -Y; the character's right side is -X.
"""

import math

import bpy
import numpy as np

from . import core
from .blocks import NX, NY, NZ, PX, PY, PZ, candle, flat_material
from mathutils import Matrix, Vector

from .voxel import QUADS

PX_M = 1.0 / 16.0


def tex_material(name, rel, clip=True, tint=None, emit=0.0, emit_rel=None, roughness=0.9):
    mat = bpy.data.materials.new(name)
    mat.use_nodes = True
    nt = mat.node_tree
    bsdf = nt.nodes["Principled BSDF"]
    bsdf.inputs["Roughness"].default_value = roughness
    bsdf.inputs["Specular IOR Level"].default_value = 0.15
    tex = nt.nodes.new("ShaderNodeTexImage")
    tex.image = bpy.data.images.load(str(core.TEX / rel), check_existing=True)
    tex.interpolation = "Closest"
    color = tex.outputs["Color"]
    if tint is not None:
        mul = nt.nodes.new("ShaderNodeVectorMath")
        mul.operation = "MULTIPLY"
        mul.inputs[1].default_value = tint
        nt.links.new(color, mul.inputs[0])
        color = mul.outputs["Vector"]
    nt.links.new(color, bsdf.inputs["Base Color"])
    if clip:
        cut = nt.nodes.new("ShaderNodeMath")
        cut.operation = "GREATER_THAN"
        cut.inputs[1].default_value = 0.5
        nt.links.new(tex.outputs["Alpha"], cut.inputs[0])
        nt.links.new(cut.outputs[0], bsdf.inputs["Alpha"])
    if emit:
        src = color
        if emit_rel is not None:
            glow = nt.nodes.new("ShaderNodeTexImage")
            glow.image = bpy.data.images.load(str(core.TEX / emit_rel), check_existing=True)
            glow.interpolation = "Closest"
            src = glow.outputs["Color"]
        nt.links.new(src, bsdf.inputs["Emission Color"])
        bsdf.inputs["Emission Strength"].default_value = emit
    core.note(mat, kind="tex", image=rel, clip=clip, tint=list(tint) if tint is not None else None, emit=emit, emit_image=emit_rel)
    return mat


class BoxMesh:
    """Accumulates textured cuboids in pixel units."""

    def __init__(self, tex_w=64, tex_h=64):
        self.w, self.h = tex_w, tex_h
        self.verts, self.uvs = [], []

    def _uv(self, region, flip_v=False):
        u, v, w, h = region
        e = 0.02
        u0, u1 = (u + e) / self.w, (u + w - e) / self.w
        top, bottom = 1 - (v + e) / self.h, 1 - (v + h - e) / self.h
        if flip_v:
            top, bottom = bottom, top
        return [(u0, bottom), (u1, bottom), (u1, top), (u0, top)]

    def faces(self, lo, size, regions, flip_v=False):
        """`regions` maps a face index to a (u, v, w, h) pixel rect."""
        lo = np.asarray(lo, np.float32)
        size = np.asarray(size, np.float32)
        for k, region in regions.items():
            self.verts.append(lo + QUADS[k] * size)
            self.uvs.append(self._uv(region, flip_v))
        return self

    def box(self, lo, size, uv, v_off=0, layout=None, inflate=0.0, flipped=False):
        """Standard box unwrap at `uv`. `layout` is the unsplit box size when this
        cuboid is a vertical slice of it, `v_off` the slice's offset from the top.
        `flipped` handles textures authored for an upside-down model (chests)."""
        w, d, h = size
        lw, ld, _ = layout or size
        u, v = uv
        side_v = v + ld + v_off
        regions = {
            NY: (u + ld, side_v, lw, h),
            NX: (u, side_v, ld, h),
            PX: (u + ld + lw, side_v, ld, h),
            PY: (u + ld + lw + ld, side_v, lw, h),
            PZ: (u + ld, v, lw, ld),
            NZ: (u + ld + lw, v, lw, ld),
        }
        if flipped:
            regions[PZ], regions[NZ] = regions[NZ], regions[PZ]
        lo = np.asarray(lo, np.float32) - inflate
        size = np.asarray(size, np.float32) + inflate * 2
        return self.faces(lo, size, regions, flip_v=flipped)

    def turned(self, quarters=1):
        """Turn everything so far by quarter turns about the vertical axis."""
        for _ in range(quarters % 4):
            self.verts = [np.stack([-v[:, 1], v[:, 0], v[:, 2]], axis=1) for v in self.verts]
        return self

    def add(self, other):
        self.verts += other.verts
        self.uvs += other.uvs
        return self

    def object(self, name, material, col, pivot=(0, 0, 0), parent=None, parent_pivot=(0, 0, 0)):
        pivot = np.asarray(pivot, np.float32)
        verts = (np.asarray(self.verts, np.float32) - pivot) * PX_M
        obj = bpy.data.objects.new(name, core.mesh_from_quads(name, verts, np.asarray(self.uvs, np.float32)))
        obj.data.materials.append(material)
        core.link(obj, col)
        obj.location = (pivot - np.asarray(parent_pivot, np.float32)) * PX_M
        if parent is not None:
            obj.parent = parent
        return obj


class Rig:
    """Named parts with pivots. `pose` takes degrees per part (XYZ euler)."""

    def __init__(self, root):
        self.root = root
        self.parts = {}
        self.pivots = {}

    def pose(self, **angles):
        for key, rot in angles.items():
            self.parts[key].rotation_euler = tuple(math.radians(a) for a in rot)
        return self

    def place(self, location, yaw=0.0, scale=None):
        self.root.location = location
        self.root.rotation_euler = (0, 0, math.radians(yaw))
        if scale is not None:
            self.root.scale = (scale,) * 3
        return self


def player(name, skin, slim=False, col="Characters"):
    """Classic 32px biped, exactly two blocks tall. Parts: body, head, arm_r, hand_r,
    arm_l, hand_l, leg_r, foot_r, leg_l, foot_l."""
    folder = "slim" if slim else "wide"
    mat = tex_material(f"{name}_skin", f"entity/player/{folder}/{skin}.png")
    root = core.empty(name, col)
    rig = Rig(root)
    aw = 3 if slim else 4

    def part(key, parent, pivot, lo, size, uv, v_off=0, layout=None, overlay=None):
        mesh = BoxMesh().box(lo, size, uv, v_off=v_off, layout=layout)
        if overlay is not None:
            mesh.box(lo, size, overlay, v_off=v_off, layout=layout, inflate=0.5)
        parent_obj = rig.parts[parent] if parent else root
        obj = mesh.object(f"{name}_{key}", mat, col, pivot, parent_obj, rig.pivots.get(parent, (0, 0, 0)))
        rig.parts[key], rig.pivots[key] = obj, pivot
        return obj

    part("body", None, (0, 0, 12), (-4, -2, 12), (8, 4, 12), (16, 16), overlay=(16, 32))
    part("head", "body", (0, 0, 24), (-4, -4, 24), (8, 8, 8), (0, 0), overlay=(32, 0))
    limbs = (
        ("arm_r", "hand_r", "body", (-4 - aw, -2, 12), aw, (-4 - aw / 2, 0, 22), (40, 16), (40, 32)),
        ("arm_l", "hand_l", "body", (4, -2, 12), aw, (4 + aw / 2, 0, 22), (32, 48), (48, 48)),
        ("leg_r", "foot_r", None, (-4, -2, 0), 4, (-2, 0, 12), (0, 16), (0, 32)),
        ("leg_l", "foot_l", None, (0, -2, 0), 4, (2, 0, 12), (16, 48), (0, 48)),
    )
    for upper, lower, parent, lo, width, pivot, uv, overlay in limbs:
        x, y, z = lo
        full = (width, 4, 12)
        part(upper, parent, pivot, (x, y, z + 6), (width, 4, 6), uv, 0, full, overlay)
        part(lower, upper, (pivot[0], 0, z + 6), (x, y, z), (width, 4, 6), uv, 6, full, overlay)
    return rig


def pumpkin_faces(blocks):
    """The atlas tiles of a pumpkin worn on the head (see `pumpkin`), front, side and top:
    carved, and lit (a jack o'lantern, its candle shining). Ask before the atlas is baked."""
    side, top = blocks.tex("pumpkin_side"), blocks.tex("pumpkin_top")
    return {"carved": (blocks.tex("carved_pumpkin"), side, top), "lit": (blocks.tex("jack_o_lantern", emit=1.0, emit_mask=candle), side, top)}


def pumpkin(rig, blocks, faces, material, col="Characters"):
    """A pumpkin on a player's head, as the game draws the one a mob wears on Halloween:
    the block at 10/16 (ten pixels a side), centred on the head, over its hat layer, and
    turning with it. One for each of `faces` (see `pumpkin_faces`), `<rig>_pumpkin_<face>`."""
    # From the head's pivot at the neck: the head is 8 pixels a side, 24 to 32 up.
    verts = (np.asarray((-5, -5, 23), np.float32) + QUADS * 10.0 - np.asarray((0, 0, 24), np.float32)) * PX_M
    worn = {}
    for key, (front, side, top) in faces.items():
        rects = blocks.atlas.rects[[side, side, side, front, top, top]]  # +X, -X, +Y (its back), -Y (its face), +Z, -Z
        uvs = np.stack([rects[:, [0, 1]], rects[:, [2, 1]], rects[:, [2, 3]], rects[:, [0, 3]]], axis=1)
        name = f"{rig.root.name}_pumpkin_{key}"
        obj = core.link(bpy.data.objects.new(name, core.mesh_from_quads(name, verts, uvs)), col)
        obj.data.materials.append(material)
        obj.parent = rig.parts["head"]
        worn[key] = obj
    return worn


def item(name, rel, col="Props", thickness=1.0):
    """A held item: every opaque pixel of a 16px sprite becomes a small cube."""
    px = core.pixels(rel)
    solid = px[..., 3] > 0.5  # (row from bottom, column)
    pad = np.pad(solid, 1)
    mesh = BoxMesh(16, 16)
    for z, x in np.argwhere(solid):
        region = (x + 0.25, 15 - z + 0.25, 0.5, 0.5)
        show = {NY: True, PY: True, PX: not pad[z + 1, x + 2], NX: not pad[z + 1, x], PZ: not pad[z + 2, x + 1], NZ: not pad[z, x + 1]}
        mesh.faces((x - 8, -thickness / 2, z - 8), (1, thickness, 1), {k: region for k, on in show.items() if on})
    return mesh.object(name, tex_material(f"{name}_mat", rel, clip=False), col)


def torch(name, col="Props", lit=True):
    if lit:
        mat = bpy.data.materials.get("Torch") or tex_material("Torch", "block/torch.png", clip=False, emit=5.0, emit_rel="block/torch_e.png")
    else:
        mat = bpy.data.materials.get("TorchOut") or tex_material("TorchOut", "block/torch.png", clip=False, tint=(0.5, 0.45, 0.45))
    side = (7, 6, 2, 10)
    mesh = BoxMesh(16, 16).faces((-1, -1, 0), (2, 2, 10), {PX: side, NX: side, PY: side, NY: side, PZ: (7, 6, 2, 2), NZ: (7, 14, 2, 2)})
    return mesh.object(name, mat, col)


def lantern(name, col="Props", lit=True):
    if lit:
        mat = bpy.data.materials.get("Lantern") or tex_material("Lantern", "block/lantern.png", clip=False, emit=6.0, emit_rel="block/lantern_e.png")
    else:
        mat = bpy.data.materials.get("LanternOut") or tex_material("LanternOut", "block/lantern.png", clip=False, tint=(0.45, 0.42, 0.45))
    body, cap = (0, 2, 6, 7), (1, 0, 4, 2)
    mesh = BoxMesh(16, 16)
    mesh.faces((-3, -3, 0), (6, 6, 7), {PX: body, NX: body, PY: body, NY: body, PZ: (0, 9, 6, 6), NZ: (0, 9, 6, 6)})
    mesh.faces((-2, -2, 7), (4, 4, 2), {PX: cap, NX: cap, PY: cap, NY: cap, PZ: (1, 10, 4, 4)})
    return mesh.object(name, mat, col)


def chest(name, col="Props", open_deg=0.0):
    """Returns (base, lid). The lid hinges at the back edge."""
    mat = bpy.data.materials.get("Chest") or tex_material("Chest", "entity/chest/normal.png", clip=False)
    # From December 24 to 26 every chest in the game is a present: the page swaps this in.
    bpy.data.materials.get("ChestChristmas") or tex_material("ChestChristmas", "entity/chest/christmas.png", clip=False)
    base = BoxMesh().box((-7, -7, 0), (14, 14, 10), (0, 19), flipped=True).object(f"{name}_base", mat, col)
    lid_mesh = BoxMesh().box((-7, -7, 9), (14, 14, 5), (0, 0), flipped=True)
    lid_mesh.box((-1, -8, 7), (2, 1, 4), (0, 0), flipped=True)
    lid = lid_mesh.object(f"{name}_lid", mat, col, pivot=(0, 7, 9), parent=base)
    lid.rotation_euler = (math.radians(open_deg), 0, 0)
    return base, lid


def cross_sprites(name, rel, positions, col="Props", tint=None, emit=0.0, emit_rel=None, size=1.0, rows=1):
    """Crossed quads (grass, wheat, vines). `rows` stacks the sprite downward for hanging plants."""
    mat = tex_material(f"{name}_mat", rel, clip=True, tint=tint, emit=emit, emit_rel=emit_rel)
    verts, uvs, sway = [], [], []
    a, b = 0.5 - 0.5 * size * 0.92, 0.5 + 0.5 * size * 0.92
    uv = [(0, 0), (1, 0), (1, 1), (0, 1)]
    # A plant stacked over several blocks (cane) is rooted where its lowest block stands;
    # one that hangs (vines) is rooted at the top of its highest.
    roots = {}
    for x, y, z in positions:
        roots[(x, y)] = max(roots.get((x, y), z + size), z + size) if rows > 1 else min(roots.get((x, y), z), z)
    for x, y, z in positions:
        for r in range(rows):
            zb = z - r * size
            for (x0, y0), (x1, y1) in (((a, a), (b, b)), ((a, b), (b, a))):
                verts.append([(x + x0, y + y0, zb), (x + x1, y + y1, zb), (x + x1, y + y1, zb + size), (x + x0, y + y0, zb + size)])
                uvs.append(uv)
                far = [abs(h - roots[(x, y)]) ** 0.7 for h in (zb, zb, zb + size, zb + size)]
                sway.append(far)
    mesh = core.mesh_from_quads(name, np.asarray(verts, np.float32), np.asarray(uvs, np.float32))
    core.point_attribute(mesh, "sway", np.asarray(sway, np.float32).ravel())  # for life.wind
    obj = bpy.data.objects.new(name, mesh)
    obj.data.materials.append(mat)
    return core.link(obj, col)


def flat_sprites(name, rel, positions, col="Props", tint=None, size=1.0, seed=0):
    """Sprites lying flat on a surface (petals, lily pads), turned in quarter steps."""
    mat = tex_material(f"{name}_mat", rel, clip=True, tint=tint)
    rng = np.random.default_rng(seed)
    corners = [(0, 0), (1, 0), (1, 1), (0, 1)]
    verts, uvs = [], []
    for x, y, z in positions:
        turn = int(rng.integers(4))
        verts.append([(x + cx * size, y + cy * size, z) for cx, cy in corners])
        uvs.append(corners[turn:] + corners[:turn])
    obj = bpy.data.objects.new(name, core.mesh_from_quads(name, np.asarray(verts, np.float32), np.asarray(uvs, np.float32)))
    obj.data.materials.append(mat)
    return core.link(obj, col)


def head(name, skin, slim=False, col="Props", scale=1.0):
    """A player head on its own (half a block), with its hat layer."""
    folder = "slim" if slim else "wide"
    mat = bpy.data.materials.get(f"{skin}_head") or tex_material(f"{skin}_head", f"entity/player/{folder}/{skin}.png")
    mesh = BoxMesh().box((-4, -4, 0), (8, 8, 8), (0, 0))
    mesh.box((-4, -4, 0), (8, 8, 8), (32, 0), inflate=0.5)
    obj = mesh.object(name, mat, col)
    obj.scale = (scale,) * 3
    return obj


def cage(name, rel="block/spawner.png", col="Props"):
    """A full block with a see-through texture on every face (the spawner)."""
    mat = bpy.data.materials.get("Cage") or tex_material("Cage", rel, clip=True)
    full = (0, 0, 16, 16)
    mesh = BoxMesh(16, 16).faces((-8, -8, 0), (16, 16, 16), {k: full for k in range(6)})
    return mesh.object(name, mat, col)


def minecart(name, col="Props"):
    """An open cart, its long axis along Y, the origin under its middle at rail height."""
    mat = bpy.data.materials.get("Minecart") or tex_material("Minecart", "entity/minecart/minecart.png", clip=False)
    # Laid out along X, as the texture is drawn, then turned to run along Y.
    mesh = BoxMesh(64, 32)
    mesh.faces((-10, -8, 0), (20, 16, 2), {PZ: (2, 12, 20, 16), NZ: (24, 12, 20, 16), NY: (2, 10, 20, 2), PY: (22, 10, 20, 2), PX: (0, 12, 2, 16), NX: (22, 12, 2, 16)})
    inside, outside, rim, edge = (2, 2, 16, 8), (20, 2, 16, 8), (2, 0, 16, 2), (0, 2, 2, 8)

    def wall(part, y, near):
        """One wall along X; `near` says whether it is the one on the -Y side."""
        part.faces((-8, y, 2), (16, 2, 8), {NY: outside if near else inside, PY: inside if near else outside, PZ: rim, PX: edge, NX: edge})

    wall(mesh, -8, True)
    wall(mesh, 6, False)
    ends = BoxMesh(64, 32)
    wall(ends, -10, True)
    wall(ends, 8, False)
    mesh.add(ends.turned())
    return mesh.turned().object(name, mat, col)


def lever(name, col="Props", on=True):
    """A lever on its stone base, to stand on top of a block. Returns (base, handle);
    the handle leans 40 degrees to one side or the other about its foot."""
    stone = bpy.data.materials.get("LeverBase") or tex_material("LeverBase", "block/cobblestone.png", clip=False)
    stick = bpy.data.materials.get("Lever") or tex_material("Lever", "block/lever.png", clip=False)
    side, end = (5, 6, 8, 3), (5, 6, 6, 3)
    base = BoxMesh(16, 16).faces((-4, -3, 0), (8, 6, 3), {PZ: (4, 5, 8, 6), NY: side, PY: side, PX: end, NX: end}).object(f"{name}_base", stone, col)
    shaft = (7, 6, 2, 10)
    handle = BoxMesh(16, 16).faces((-1, -1, 1), (2, 2, 10), {PX: shaft, NX: shaft, PY: shaft, NY: shaft, PZ: (7, 6, 2, 2)}).object(f"{name}_handle", stick, col, pivot=(0, 0, 2), parent=base)
    handle.rotation_euler = (0, math.radians(40 if on else -40), 0)
    return base, handle


def sign(name, col="Props", post=True):
    """A sign: a board 24 by 12 pixels on a post, the origin at the foot. Faces -Y.
    The board glows a little, so what is written on it reads in a dim room."""
    mat = bpy.data.materials.get("Sign") or tex_material("Sign", "entity/signs/oak.png", clip=False, emit=0.3)
    mesh = BoxMesh(64, 32).box((-12, -1, 14 if post else 0), (24, 2, 12), (0, 0))
    if post:
        mesh.box((-1, -1, 0), (2, 2, 14), (0, 14))
    return mesh.object(name, mat, col)


def item_frame(name, col="Props"):
    """An item frame holding a filled map, as the game models one (`item_frame_map`): a
    leather back with a birch rim a pixel deep, the map over the whole of its face. Its
    origin is the middle of the face of the block it hangs on; it faces -Y.

    The map is `{name}_Map`, a block-wide square a little in front of the rim, noted
    `kind="map"`: the page draws what it shows (web/lib/landing/film/maps.ts); here it is
    blank paper."""
    root = core.empty(name, col)
    back = bpy.data.materials.get("ItemFrame_back") or tex_material("ItemFrame_back", "block/item_frame.png", clip=False)
    wood = bpy.data.materials.get("ItemFrame_wood") or tex_material("ItemFrame_wood", "block/birch_planks.png", clip=False)
    BoxMesh(16, 16).faces((-7, -1, -7), (14, 1, 14), {NY: (1, 1, 14, 14), PY: (1, 1, 14, 14)}).object(f"{name}_Back", back, col, parent=root)
    rim = BoxMesh(16, 16)
    rim.faces((-8, -1, -8), (16, 1, 1), {NY: (0, 15, 16, 1), NZ: (0, 0, 16, 1), PZ: (0, 15, 16, 1), NX: (15, 15, 1, 1), PX: (0, 15, 1, 1)})
    rim.faces((-8, -1, 7), (16, 1, 1), {NY: (0, 0, 16, 1), NZ: (0, 0, 16, 1), PZ: (0, 15, 16, 1), NX: (15, 0, 1, 1), PX: (0, 0, 1, 1)})
    rim.faces((-8, -1, -7), (1, 1, 14), {NY: (15, 1, 1, 14), NX: (15, 1, 1, 14), PX: (0, 1, 1, 14)})
    rim.faces((7, -1, -7), (1, 1, 14), {NY: (0, 1, 1, 14), NX: (15, 1, 1, 14), PX: (0, 1, 1, 14)})
    rim.object(f"{name}_Rim", wood, col, parent=root)
    paper = bpy.data.materials.get("Map") or flat_material("Map", (0.84, 0.79, 0.55))
    face = BoxMesh(128, 128).faces((-8, -1.15, -8), (16, 0, 16), {NY: (0, 0, 128, 128)}).object(f"{name}_Map", paper, col, parent=root)
    core.note(face, kind="map")
    return root


def tiles(name, rel, quads, uvs=None, col="Props", **material):
    """Flat textured tiles from explicit corners (rails and the like)."""
    mat = bpy.data.materials.get(f"{name}_mat") or tex_material(f"{name}_mat", rel, **material)
    quads = np.asarray(quads, np.float32)
    if uvs is None:
        uvs = [[(0, 0), (1, 0), (1, 1), (0, 1)]] * len(quads)
    obj = bpy.data.objects.new(name, core.mesh_from_quads(name, quads, np.asarray(uvs, np.float32)))
    obj.data.materials.append(mat)
    return core.link(obj, col)


def fish(name, kind="salmon", col="Props"):
    """A fish about a block long, nose toward -Y, the origin at its middle."""
    mat = bpy.data.materials.get(f"Fish_{kind}") or tex_material(f"Fish_{kind}", f"entity/fish/{kind}.png", clip=False)
    mesh = BoxMesh(32, 32)
    mesh.box((-1.5, -8, -2.5), (3, 8, 5), (0, 0))
    mesh.box((-1.5, 0, -2.5), (3, 8, 5), (0, 13))
    mesh.box((-1, -11, -2), (2, 3, 4), (22, 0))
    return mesh.object(name, mat, col)


def hold(rig, thing, grip=(-6, -6), scale=1.0, hand="hand_r"):
    """Put an item (see `item`) in a hand, held by the pixel `grip` of its sprite, standing
    up out of the fist. Returns the point of the sprite's far corner in the item's own
    space, which for a rod or a tool is its tip."""
    thing.parent = rig.parts[hand]
    turn = Matrix.Rotation(math.pi, 3, "X") @ Matrix.Rotation(math.pi / 2, 3, "Z")
    thing.rotation_euler = turn.to_euler()
    thing.scale = (scale,) * 3
    thing.location = Vector((0.0, -0.1, -0.34)) - turn @ (Vector((grip[0] / 16, 0, grip[1] / 16)) * scale)
    return Vector((7.5 / 16, 0, 7.5 / 16))


def image_tiles(name, image, quads, uvs=None, col="Props", emit=0.0):
    """Flat tiles showing an image made in code (see core.image_from_pixels), cut out
    where it is clear."""
    mat = bpy.data.materials.new(f"{name}_mat")
    mat.use_nodes = True
    nt = mat.node_tree
    bsdf = nt.nodes["Principled BSDF"]
    bsdf.inputs["Roughness"].default_value = 0.9
    bsdf.inputs["Specular IOR Level"].default_value = 0.1
    tex = nt.nodes.new("ShaderNodeTexImage")
    tex.image = image
    tex.interpolation = "Closest"
    nt.links.new(tex.outputs["Color"], bsdf.inputs["Base Color"])
    cut = nt.nodes.new("ShaderNodeMath")
    cut.operation = "GREATER_THAN"
    cut.inputs[1].default_value = 0.5
    nt.links.new(tex.outputs["Alpha"], cut.inputs[0])
    nt.links.new(cut.outputs[0], bsdf.inputs["Alpha"])
    if emit:
        nt.links.new(tex.outputs["Color"], bsdf.inputs["Emission Color"])
        bsdf.inputs["Emission Strength"].default_value = emit
    core.note(mat, kind="tex", made=image.name, clip=True, emit=emit)  # (an image made in code, saved under export/)
    quads = np.asarray(quads, np.float32)
    if uvs is None:
        uvs = [[(0, 0), (1, 0), (1, 1), (0, 1)]] * len(quads)
    obj = bpy.data.objects.new(name, core.mesh_from_quads(name, quads, np.asarray(uvs, np.float32)))
    obj.data.materials.append(mat)
    return core.link(obj, col)


def lectern(name, col="Props"):
    """A lectern with its book open, a block tall, its origin at the foot and its book
    toward -Y: a slab of a base, a post, and the sloped top."""
    root = core.empty(name, col)

    def part(key, rel, lo, size, pivot=(0, 0, 0)):
        mat = bpy.data.materials.get(f"Lectern_{key}") or tex_material(f"Lectern_{key}", rel, clip=False)
        mesh = BoxMesh(16, 16).faces(lo, size, {k: (0, 0, 16, 16) for k in (NX, PX, NY, PY, NZ, PZ)})
        return mesh.object(f"{name}_{key}", mat, col, pivot, root)

    part("base", "block/lectern_base.png", (-8, -8, 0), (16, 16, 2))
    part("post", "block/lectern_front.png", (-4, -4, 2), (8, 8, 11))
    top = part("top", "block/lectern_top.png", (-8, -7, 13), (16, 14, 3), pivot=(0, 0, 14))
    top.rotation_euler = (math.radians(-22.5), 0.0, 0.0)  # sloped toward its reader
    book = part("book", "item/book.png", (-6, -5, 16), (12, 10, 1), pivot=(0, 0, 14))
    book.rotation_euler = top.rotation_euler
    return root


def creeper(name, col="Characters"):
    """A creeper, facing -Y: a head on a tall body on four short legs, a little over 1.6
    blocks tall. Returns a Rig with parts body, head, leg_fr, leg_fl, leg_br, leg_bl (the
    legs swing from the hip)."""
    mat = bpy.data.materials.get("Creeper") or tex_material("Creeper", "entity/creeper/creeper.png")
    root = core.empty(name, col)
    rig = Rig(root)
    for key, parent, lo, size, uv, pivot in (
        ("body", None, (-4, -2, 6), (8, 4, 12), (16, 16), (0, 0, 6)),
        ("head", "body", (-4, -4, 18), (8, 8, 8), (0, 0), (0, 0, 18)),
        ("leg_fr", None, (-4, -6, 0), (4, 4, 6), (0, 16), (-2, -4, 6)),
        ("leg_fl", None, (0, -6, 0), (4, 4, 6), (0, 16), (2, -4, 6)),
        ("leg_br", None, (-4, 2, 0), (4, 4, 6), (0, 16), (-2, 4, 6)),
        ("leg_bl", None, (0, 2, 0), (4, 4, 6), (0, 16), (2, 4, 6)),
    ):
        holder = rig.parts[parent] if parent else root
        rig.parts[key] = BoxMesh(64, 32).box(lo, size, uv).object(f"{name}_{key}", mat, col, pivot, holder, rig.pivots.get(parent, (0, 0, 0)))
        rig.pivots[key] = pivot
    return rig


def strider(name, col="Characters"):
    """A strider: a body on two long legs, about two blocks tall, facing -Y. Returns a Rig
    with parts body, leg_r and leg_l (the legs swing from the hip)."""
    mat = bpy.data.materials.get("Strider") or tex_material("Strider", "entity/strider/strider.png", clip=False)
    root = core.empty(name, col)
    rig = Rig(root)
    for key, lo, size, uv, pivot in (
        ("body", (-8, -8, 16), (16, 16, 14), (0, 0), (0, 0, 16)),
        ("leg_r", (-6, -2, 0), (4, 4, 16), (0, 32), (-4, 0, 16)),
        ("leg_l", (2, -2, 0), (4, 4, 16), (0, 55), (4, 0, 16)),
    ):
        rig.parts[key] = BoxMesh(64, 128).box(lo, size, uv).object(f"{name}_{key}", mat, col, pivot, root)
        rig.pivots[key] = pivot
    return rig


def chicken(name, col="Characters"):
    """A chicken, as the game models it, facing -Y: a head with its beak and red wattle, a
    body lying along its length, two wings and two legs, eleven pixels to its back and
    fifteen to the top of its head. Its picture draws each leg as the game does, as a stick
    down the back of the box and a foot under it (the rest is clear). Returns a Rig with
    parts body, head, leg_r, leg_l, wing_r, wing_l: the legs swing from the hip, the wings
    lift out from the shoulder (about Y)."""
    mat = bpy.data.materials.get("Chicken") or tex_material("Chicken", "entity/chicken/chicken_temperate.png")
    root = core.empty(name, col)
    rig = Rig(root)
    for key, boxes, pivot in (
        ("body", (((-3, -3, 4), (6, 6, 8), (0, 9)),), (0, 0, 8)),  # upright as the picture lays it out; turned to lie along Y below
        ("head", (((-2, -6, 9), (4, 3, 6), (0, 0)), ((-2, -8, 11), (4, 2, 2), (14, 0)), ((-1, -7, 9), (2, 2, 2), (14, 4))), (0, -4, 9)),  # and its beak and wattle
        ("leg_r", (((-3, -2, 0), (3, 3, 5), (26, 0)),), (-2, 1, 5)),
        ("leg_l", (((0, -2, 0), (3, 3, 5), (26, 0)),), (1, 1, 5)),
        ("wing_r", (((-4, -3, 7), (1, 6, 4), (24, 13)),), (-4, 0, 11)),
        ("wing_l", (((3, -3, 7), (1, 6, 4), (24, 13)),), (4, 0, 11)),
    ):
        mesh = BoxMesh(64, 32)
        for lo, size, uv in boxes:
            mesh.box(lo, size, uv)
        rig.parts[key] = mesh.object(f"{name}_{key}", mat, col, pivot, root)
        rig.pivots[key] = pivot
    rig.parts["body"].rotation_euler = (math.radians(90), 0.0, 0.0)
    return rig


def baby_zombie(name, col="Characters"):
    """A baby zombie, as the game models it since its babies got models of their own (26.1):
    a head six pixels a side on a body and limbs half a grown zombie's, fifteen pixels tall,
    facing -Y. Returns a Rig with parts body, head, arm_r, arm_l, leg_r, leg_l (the limbs
    swing from shoulder and hip)."""
    mat = bpy.data.materials.get("BabyZombie") or tex_material("BabyZombie", "entity/zombie/zombie_baby.png")
    root = core.empty(name, col)
    rig = Rig(root)
    for key, lo, size, uv, pivot in (
        ("body", (-2, -1, 4), (4, 2, 5), (16, 16), (0, 0, 6.5)),
        ("head", (-3, -3, 9), (6, 6, 6), (3, 3), (0, 0, 8.75)),
        ("arm_r", (-4, -1, 4), (2, 2, 5), (36, 16), (-3, 0, 8.5)),
        ("arm_l", (2, -1, 4), (2, 2, 5), (28, 16), (3, 0, 8.5)),
        ("leg_r", (-2, -1, 0), (2, 2, 4), (8, 16), (-1, 0, 4)),
        ("leg_l", (0, -1, 0), (2, 2, 4), (0, 16), (1, 0, 4)),
    ):
        mesh = BoxMesh(64, 64).box(lo, size, uv)
        if key == "head":
            mesh.box((-3, -3, 8.9), (6, 6, 6), (35, 3), inflate=0.25)  # its hat layer
        rig.parts[key] = mesh.object(f"{name}_{key}", mat, col, pivot, root)
        rig.pivots[key] = pivot
    return rig


def seat(rider, mount, at=(0.0, 1.6, 8.2)):
    """Sit a zombie on a mount as the game seats a rider: at the mount's passenger point
    less the rider's own (for a baby zombie on a chicken, 0.7 - 3/16 blocks up and 0.1
    back: `at`, in pixels from the mount's feet), so its legs rest on the mount's back.
    Legs forward and a little apart, as anything riding sits; arms out in front, as a
    zombie holds them. Turns are the game's order: about X, then up, then along."""
    rider.root.parent = mount.root
    rider.root.location = tuple(v * PX_M for v in at)
    for key, turn in (("leg_r", (-81, 4.5, -18)), ("leg_l", (-81, -4.5, 18)), ("arm_r", (-80, 2.9, 5.7)), ("arm_l", (-80, -2.9, -5.7))):
        part = rider.parts[key]
        part.rotation_mode = "XZY"
        part.rotation_euler = tuple(math.radians(a) for a in turn)
    return rider
