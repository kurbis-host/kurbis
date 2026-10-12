"""Box models with Minecraft-style box UVs: players, held items, small props.

Characters are rigid parts under pivots (no skinning), which exports to glTF as
plain animated nodes. Limbs are split at the elbow and knee so they can bend.
Models face -Y; the character's right side is -X.
"""

import math

import bpy
import numpy as np

from . import core
from .blocks import NX, NY, NZ, PX, PY, PZ
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
