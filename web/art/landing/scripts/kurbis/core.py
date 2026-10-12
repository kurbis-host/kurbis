"""Paths, scene reset and small Blender helpers shared by the toolkit."""

import json
from pathlib import Path

import bpy
import numpy as np
from mathutils import Euler, Matrix

ROOT = Path(__file__).resolve().parents[2]
# POC only: third-party trailer-style pack. Replaced by our own textures before shipping.
TEX = ROOT / "ref" / "pack" / "assets" / "minecraft" / "textures"
EXPORT = ROOT / "export"
RENDERS = ROOT / "renders"
BLEND = ROOT / "blend"

COLLECTIONS = ("World", "Type", "Characters", "Props", "FX", "Lights", "Cameras")


def reset_scene(name="KurbisLanding"):
    """Empty the scene by hand. read_factory_settings would drop the MCP add-on."""
    scene = bpy.context.scene
    for obj in list(bpy.data.objects):
        bpy.data.objects.remove(obj, do_unlink=True)
    for coll in (
        bpy.data.collections,
        bpy.data.meshes,
        bpy.data.materials,
        bpy.data.lights,
        bpy.data.cameras,
        bpy.data.worlds,
        bpy.data.node_groups,
        bpy.data.actions,
    ):
        for item in list(coll):
            coll.remove(item)
    for img in list(bpy.data.images):
        if img.type not in {"RENDER_RESULT", "COMPOSITING"}:
            bpy.data.images.remove(img)
    scene.name = name
    for col in COLLECTIONS:
        scene.collection.children.link(bpy.data.collections.new(col))
    for path in (EXPORT, RENDERS, BLEND):
        path.mkdir(parents=True, exist_ok=True)
    return scene


def link(obj, col):
    bpy.data.collections[col].objects.link(obj)
    return obj


def empty(name, col, location=(0, 0, 0), parent=None):
    obj = link(bpy.data.objects.new(name, None), col)
    obj.empty_display_size = 0.2
    obj.location = location
    if parent is not None:
        obj.parent = parent
    return obj


def pixels(rel, frame=0):
    """Texture as float RGBA, row 0 at the bottom. Animated strips give one frame."""
    img = bpy.data.images.load(str(TEX / rel), check_existing=True)
    w, h = img.size
    buf = np.empty(w * h * 4, dtype=np.float32)
    img.pixels.foreach_get(buf)
    buf = buf.reshape(h, w, 4)
    if h > w:
        buf = buf[h - w * (frame + 1) : h - w * frame]
    return buf.copy()


def image_from_pixels(name, buf, path=None):
    h, w, _ = buf.shape
    img = bpy.data.images.new(name, w, h, alpha=True)
    img.pixels.foreach_set(np.ascontiguousarray(buf, dtype=np.float32).ravel())
    if path is not None:
        img.filepath_raw = str(path)
        img.file_format = "PNG"
        img.save()
    return img


def note(block, **what):
    """Write down on a datablock what it is. The web exporter reads these notes rather
    than take node trees and modifiers apart. Notes add up."""
    said = json.loads(block.get("kurbis", "{}"))
    said.update(what)
    block["kurbis"] = json.dumps(said)
    return block


def noted(block):
    return json.loads(block.get("kurbis", "{}"))


def mesh_from_quads(name, verts, uvs):
    """verts: (n, 4, 3), uvs: (n, 4, 2). Unshared corners, so shading stays flat."""
    n = len(verts)
    me = bpy.data.meshes.new(name)
    me.vertices.add(n * 4)
    me.vertices.foreach_set("co", np.ascontiguousarray(verts, dtype=np.float32).ravel())
    me.loops.add(n * 4)
    me.loops.foreach_set("vertex_index", np.arange(n * 4, dtype=np.int32))
    me.polygons.add(n)
    me.polygons.foreach_set("loop_start", np.arange(0, n * 4, 4, dtype=np.int32))
    uv = me.uv_layers.new(name="UVMap")
    uv.data.foreach_set("uv", np.ascontiguousarray(uvs, dtype=np.float32).ravel())
    me.update(calc_edges=True)
    me.validate()
    return me


def point_attribute(mesh, name, values):
    """Store one value per vertex (floats, or vectors if `values` has three columns)."""
    values = np.ascontiguousarray(values, dtype=np.float32)
    vector = values.ndim == 2
    attribute = mesh.attributes.new(name, "FLOAT_VECTOR" if vector else "FLOAT", "POINT")
    attribute.data.foreach_set("vector" if vector else "value", values.ravel())
    return attribute


def world_matrix(obj, turns=None):
    """Where an object is, worked out from its own placement and its parents' (no scene
    update needed, so it is cheap to ask on every frame). `turns` maps objects to
    rotations (radians) to use instead of the ones they have."""
    basis = obj.matrix_basis
    if turns and obj in turns:
        basis = Matrix.LocRotScale(obj.location, Euler(turns[obj]), obj.scale)
    if obj.parent is None:
        return basis.copy()
    return world_matrix(obj.parent, turns) @ obj.matrix_parent_inverse @ basis
