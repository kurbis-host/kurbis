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
    mesh = core.mesh_from_quads(name, quads, np.zeros((count, 4, 2), np.float32))
    # For life.drift: every corner knows its flake's centre and the flake's own phase.
    core.point_attribute(mesh, "centre", np.repeat(np.asarray(centers, np.float32), 4, axis=0))
    core.point_attribute(mesh, "phase", np.repeat(rng.uniform(0.0, 6.283, count).astype(np.float32), 4))
    obj = bpy.data.objects.new(name, mesh)
    obj.data.materials.append(material)
    core.note(obj, kind="flakes", lo=[float(v) for v in lo], hi=[float(v) for v in hi], count=int(count), size=list(size), seed=seed)
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


def snapshot_block(name, blocks, material, col, size=1.0, door=False):
    """The saved world as one block: 5x5x5 tiny blocks, grass over dirt over stone, with a
    hollow at the front holding a tiny lit pumpkin. Centred on the origin. `door` adds a
    one-cell opening in the hollow's back wall, for the camera to fly into."""
    n = 5
    g = np.full((n, n, n), blocks["stone"], np.uint8)
    g[:, :, 4] = blocks["grass"]
    g[:, :, 3] = blocks["dirt"]
    for x, y, z, ore in ((0, 0, 0, "diamond_ore"), (4, 0, 1, "gold_ore"), (0, 0, 2, "redstone_ore"), (4, 3, 0, "diamond_ore"), (4, 0, 0, "coal_ore")):
        g[x, y, z] = blocks[ore]
    g[1:4, 0:3, 1:3] = 0  # the cave
    g[2, 1, 1] = blocks["kurbis_awake" if "kurbis_awake" in blocks.names else "jack_o_lantern"]
    if door:
        g[1, 3, 1] = 0
    s = size / n
    obj = voxel.grid_object(name, g, blocks, material, col, scale=s, origin=(-size / 2,) * 3)
    # Our pumpkin is its own kind of block, so it meshes separately and rides along.
    tiny = voxel.grid_object(f"{name}_pumpkin", g, blocks, material, col, group="mascot", scale=s, origin=(-size / 2,) * 3)
    if tiny is not None:
        tiny.parent = obj
    return obj


def _scrim_material():
    """Black, see-through, and fading out toward every edge of the card. How soft each
    pair of edges is comes from the object's colour (red across, green up), and the
    strength from its alpha, so every card can share the one material."""
    mat = bpy.data.materials.get("Scrim")
    if mat is not None:
        return mat
    mat = bpy.data.materials.new("Scrim")
    mat.use_nodes = True
    nt = mat.node_tree
    nt.nodes.clear()
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    coord = nt.nodes.new("ShaderNodeTexCoord")
    sep = nt.nodes.new("ShaderNodeSeparateXYZ")
    nt.links.new(coord.outputs["UV"], sep.inputs[0])
    info = nt.nodes.new("ShaderNodeObjectInfo")
    soft = nt.nodes.new("ShaderNodeSeparateColor")
    nt.links.new(info.outputs["Color"], soft.inputs[0])

    def math(op, a, b):
        node = nt.nodes.new("ShaderNodeMath")
        node.operation = op
        for socket, value in zip(node.inputs, (a, b)):
            if isinstance(value, (int, float)):
                socket.default_value = value
            else:
                nt.links.new(value, socket)
        return node.outputs[0]

    def edges(axis, share):
        """1 in the middle of the card, falling to 0 at both edges along one axis."""
        result = None
        for lo, hi in ((0.0, share), (1.0, math("SUBTRACT", 1.0, share))):
            node = nt.nodes.new("ShaderNodeMapRange")
            node.interpolation_type = "SMOOTHSTEP"
            for socket, value in ((node.inputs["From Min"], lo), (node.inputs["From Max"], hi)):
                if isinstance(value, float):
                    socket.default_value = value
                else:
                    nt.links.new(value, socket)
            nt.links.new(sep.outputs[axis], node.inputs["Value"])
            result = node.outputs["Result"] if result is None else math("MULTIPLY", result, node.outputs["Result"])
        return result

    mask = math("MULTIPLY", edges("X", soft.outputs["Red"]), edges("Y", soft.outputs["Green"]))
    clear = nt.nodes.new("ShaderNodeBsdfTransparent")
    dark = nt.nodes.new("ShaderNodeEmission")
    dark.inputs["Color"].default_value = (0.0, 0.0, 0.0, 1.0)
    mix = nt.nodes.new("ShaderNodeMixShader")
    nt.links.new(math("MULTIPLY", mask, info.outputs["Alpha"]), mix.inputs[0])
    nt.links.new(clear.outputs[0], mix.inputs[1])
    nt.links.new(dark.outputs[0], mix.inputs[2])
    nt.links.new(mix.outputs[0], out.inputs["Surface"])
    mat.surface_render_method = "BLENDED"
    core.note(mat, kind="scrim")
    return mat


def scrim(name, width, height, col="Type", strength=0.6, feather=0.9):
    """A soft dark card to sit just behind a block of type, so the letters stand out
    whatever is behind them. `width` and `height` are the size of the type; the card
    reaches `feather` further out on every side and is fully dark only inside the
    type's own box. Faces -Y, centred on its origin."""
    w, h = width / 2 + feather, height / 2 + feather
    quad = np.array([[(-w, 0, -h), (w, 0, -h), (w, 0, h), (-w, 0, h)]], np.float32)
    uv = np.array([[(0, 0), (1, 0), (1, 1), (0, 1)]], np.float32)
    obj = bpy.data.objects.new(name, core.mesh_from_quads(name, quad, uv))
    obj.data.materials.append(_scrim_material())
    obj.color = (min(1.3 * feather / (2 * w), 0.5), min(1.3 * feather / (2 * h), 0.5), 0.0, strength)
    obj.visible_shadow = False
    core.note(obj, kind="scrim", width=width, height=height, feather=feather, strength=strength)
    return core.link(obj, col)


def fading_material(name, rgb, strength=1.0):
    """Flat light whose opacity is the object's colour alpha, so each object can fade
    by itself while they all share the material."""
    mat = bpy.data.materials.get(name)
    if mat is not None:
        return mat
    mat = bpy.data.materials.new(name)
    mat.use_nodes = True
    nt = mat.node_tree
    nt.nodes.clear()
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    info = nt.nodes.new("ShaderNodeObjectInfo")
    clear = nt.nodes.new("ShaderNodeBsdfTransparent")
    light = nt.nodes.new("ShaderNodeEmission")
    light.inputs["Color"].default_value = (*rgb, 1.0)
    light.inputs["Strength"].default_value = strength
    mix = nt.nodes.new("ShaderNodeMixShader")
    nt.links.new(info.outputs["Alpha"], mix.inputs[0])
    nt.links.new(clear.outputs[0], mix.inputs[1])
    nt.links.new(light.outputs[0], mix.inputs[2])
    nt.links.new(mix.outputs[0], out.inputs["Surface"])
    mat.surface_render_method = "BLENDED"
    core.note(mat, kind="fading", rgb=list(rgb), strength=strength)
    return mat


def ring(name, material, col="FX", thick=0.1):
    """A flat square ring one block across, lying level: a ripple, the blocky way."""
    h, t = 0.5, thick
    quads = [
        [(-h, -h, 0), (h, -h, 0), (h, -h + t, 0), (-h, -h + t, 0)],
        [(-h, h - t, 0), (h, h - t, 0), (h, h, 0), (-h, h, 0)],
        [(-h, -h + t, 0), (-h + t, -h + t, 0), (-h + t, h - t, 0), (-h, h - t, 0)],
        [(h - t, -h + t, 0), (h, -h + t, 0), (h, h - t, 0), (h - t, h - t, 0)],
    ]
    obj = bpy.data.objects.new(name, core.mesh_from_quads(name, np.asarray(quads, np.float32), np.zeros((4, 4, 2), np.float32)))
    obj.data.materials.append(material)
    obj.color = (1.0, 1.0, 1.0, 0.0)
    obj.visible_shadow = False
    return core.link(obj, col)


def streaks(name, material, col, lo, hi, count, size=(0.07, 1.1), seed=0):
    """Thin upright strokes facing the viewer: rain. Like `leaves`, each knows its centre
    and phase, so life.drift can let it fall for ever."""
    rng = np.random.default_rng(seed)
    centers = rng.uniform(lo, hi, (count, 3)).astype(np.float32)
    w, h = size[0] / 2, size[1] / 2
    quads = np.array([[c + (-w, 0, -h), c + (w, 0, -h), c + (w, 0, h), c + (-w, 0, h)] for c in centers], np.float32)
    mesh = core.mesh_from_quads(name, quads, np.zeros((count, 4, 2), np.float32))
    core.point_attribute(mesh, "centre", np.repeat(centers, 4, axis=0))
    core.point_attribute(mesh, "phase", np.repeat(rng.uniform(0.0, 6.283, count).astype(np.float32), 4))
    obj = bpy.data.objects.new(name, mesh)
    obj.data.materials.append(material)
    obj.visible_shadow = False
    core.note(obj, kind="flakes", streaks=True, lo=[float(v) for v in lo], hi=[float(v) for v in hi], count=int(count), size=list(size), seed=seed)
    return core.link(obj, col)


def crack_material(stages=10):
    """The cracks a block shows as it is dug: ten stages side by side in one image, and
    the object's colour (red, 0..1) says which one shows. So every cracking block can
    share the material and still be at its own stage."""
    mat = bpy.data.materials.get("Cracks")
    if mat is not None:
        return mat
    strip = np.concatenate([core.pixels(f"block/destroy_stage_{i}.png") for i in range(stages)], axis=1)
    image = core.image_from_pixels("cracks", strip, core.EXPORT / "cracks.png")
    mat = bpy.data.materials.new("Cracks")
    mat.use_nodes = True
    nt = mat.node_tree
    nt.nodes.clear()
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    coord = nt.nodes.new("ShaderNodeTexCoord")
    sep = nt.nodes.new("ShaderNodeSeparateXYZ")
    nt.links.new(coord.outputs["UV"], sep.inputs[0])
    info = nt.nodes.new("ShaderNodeObjectInfo")
    stage = nt.nodes.new("ShaderNodeSeparateColor")
    nt.links.new(info.outputs["Color"], stage.inputs[0])

    def math(op, a, b):
        node = nt.nodes.new("ShaderNodeMath")
        node.operation = op
        for socket, value in zip(node.inputs, (a, b)):
            if isinstance(value, (int, float)):
                socket.default_value = value
            else:
                nt.links.new(value, socket)
        return node.outputs[0]

    which = math("FLOOR", math("MULTIPLY", stage.outputs["Red"], float(stages)), 0.0)
    u = math("DIVIDE", math("ADD", math("FRACT", sep.outputs["X"], 0.0), which), float(stages))
    join = nt.nodes.new("ShaderNodeCombineXYZ")
    nt.links.new(u, join.inputs["X"])
    nt.links.new(sep.outputs["Y"], join.inputs["Y"])
    tex = nt.nodes.new("ShaderNodeTexImage")
    tex.image = image
    tex.interpolation = "Closest"
    nt.links.new(join.outputs[0], tex.inputs["Vector"])
    clear = nt.nodes.new("ShaderNodeBsdfTransparent")
    dark = nt.nodes.new("ShaderNodeBsdfDiffuse")
    nt.links.new(tex.outputs["Color"], dark.inputs["Color"])
    mix = nt.nodes.new("ShaderNodeMixShader")
    nt.links.new(math("GREATER_THAN", tex.outputs["Alpha"], 0.3), mix.inputs[0])
    nt.links.new(clear.outputs[0], mix.inputs[1])
    nt.links.new(dark.outputs[0], mix.inputs[2])
    nt.links.new(mix.outputs[0], out.inputs["Surface"])
    mat.surface_render_method = "DITHERED"
    core.note(mat, kind="crack", stages=stages, made="cracks")
    return mat
