"""Legibility audit: is every line of in-world type readable at the stop it belongs to?

Run inside Blender after `film.build(...)`, on a session that rests at the stops:

    report = audit.run(scene, [("surface", frame), ...])

For each stop it takes every type object the camera can see on that frame and checks
  framing    the whole of it lies inside the safe area (5% in from every edge)
  size       a line of it is tall enough on a screen 1080 pixels on its short side
  occlusion  rays from the lens reach its letters without hitting something else
  contrast   its letters stand out from what is behind them: the frame is rendered
             with the type and without, and the two are compared under the letters.
             (A rim round the letters helps the eye but not this figure.)
Type that is wholly out of frame, or wholly behind a wall, belongs to another stop
and is skipped.
"""

import tempfile
from pathlib import Path

import bpy
import numpy as np
from bpy_extras.object_utils import world_to_camera_view
from mathutils import Vector

SAFE = 0.05  # margin on every side, as a share of the frame
MIN_LINE = 22.0  # pixels a line must be tall, on a screen 1080 on its short side
MAX_HIDDEN = 0.06  # share of a text's letters that may be covered
MIN_CONTRAST = 3.0  # luminance ratio of letters to their ground (the figure WCAG gives for large text)


def _type_objects():
    """Type that carries copy: not the scrims behind it or the rims round it, nor the
    sleeper's drifting Zs."""
    skip = ("Scrim", "Rim", "Zzz", "Dig_Sign")  # (the old sign in the dirt is a find, read through its tooltip)
    return [obj for obj in bpy.data.collections["Type"].objects if obj.type == "MESH" and not any(word in obj.name for word in skip)]


def _corners(obj):
    return [obj.matrix_world @ Vector(corner) for corner in obj.bound_box]


def check(scene, depsgraph, cam, obj):
    """One text object on the current frame: None if it is not in this picture at all,
    otherwise what was measured and what is wrong with it."""
    world = obj.matrix_world
    if min(abs(axis.length) for axis in world.to_3x3().col) < 1e-4:
        return None  # scaled away: not showing
    seen = [world_to_camera_view(scene, cam, point) for point in _corners(obj)]
    if all(v.z <= 0 for v in seen):
        return None
    xs, ys = [v.x for v in seen], [v.y for v in seen]
    if max(xs) < 0 or min(xs) > 1 or max(ys) < 0 or min(ys) > 1:
        return None
    problems = []
    if min(v.z for v in seen) <= 0 or min(xs) < SAFE or max(xs) > 1 - SAFE or min(ys) < SAFE or max(ys) > 1 - SAFE:
        problems.append("framing")

    # Its letters: the front faces (type is built facing -Y), where they look back at the
    # lens. One line of type is eight cells high, and its face tiles are one cell, so a
    # face's height tells the line's.
    mesh = obj.evaluated_get(depsgraph).data
    eye = cam.matrix_world.translation
    turn = world.to_3x3().normalized()
    faces = [poly for poly in mesh.polygons if poly.normal.y < -0.9 and (turn @ poly.normal).dot(eye - world @ poly.center) > 0.0]
    if not faces:
        return {"problems": ["faces away"], "line_px": 0.0, "hidden": 1.0, "by": []}
    sample = faces[:: max(len(faces) // 60, 1)]
    cell = None
    for poly in sample[:8]:
        points = [world_to_camera_view(scene, cam, world @ mesh.vertices[i].co) for i in poly.vertices]
        tall = (max(p.y for p in points) - min(p.y for p in points)) * (1080.0 if scene.render.resolution_x >= scene.render.resolution_y else 1920.0)
        cell = tall if cell is None else min(cell, tall)
    line_px = 8.0 * (cell or 0.0)
    if line_px < MIN_LINE:
        problems.append("small")
    hidden, by = 0, {}
    for poly in sample:
        point = world @ poly.center
        other = _first_hit(scene, depsgraph, eye, point, obj)
        if other is not None:
            hidden += 1
            by[other] = by.get(other, 0) + 1
    share = hidden / len(sample)
    if share > 0.95:
        return None  # wholly behind a wall: it belongs to a place the camera is not in
    if share > MAX_HIDDEN:
        problems.append("covered")
    return {"problems": problems, "line_px": round(line_px, 1), "hidden": round(share, 2), "by": sorted(by, key=by.get, reverse=True)[:3]}


def _passes(other):
    """Things a ray may go through: flakes on the wind and other small effects, and
    interface cards that are not showing."""
    if other.name.startswith("Hud_"):
        return other.color[3] < 0.05 or other.name == "Hud_pointer"  # the pointer is the previs's, not the page's
    return "FX" in [c.name for c in other.users_collection]


def _first_hit(scene, depsgraph, eye, point, obj):
    """The name of whatever stands between the lens and a point of `obj`, or None."""
    start, reach = eye, (point - eye)
    aim = reach.normalized()
    for _ in range(8):
        left = (point - start).length - 0.02
        if left <= 0:
            return None
        hit, where, _normal, _index, other, _matrix = scene.ray_cast(depsgraph, start, aim, distance=left)
        if not hit or other.original == obj or (point - where).length < 0.06:  # (letters side by side in one plane)
            return None
        if not _passes(other.original):
            return other.original.name
        start = where + aim * 0.01
    return None


def _luminance(rgb):
    """Relative luminance of sRGB pixels."""
    linear = np.where(rgb <= 0.04045, rgb / 12.92, ((rgb + 0.055) / 1.055) ** 2.4)
    return linear @ np.array((0.2126, 0.7152, 0.0722))


def _render(scene, path):
    scene.render.filepath = str(path)
    bpy.ops.render.render(write_still=True)
    image = bpy.data.images.load(str(path), check_existing=False)
    w, h = image.size
    buf = np.empty(w * h * 4, np.float32)
    image.pixels.foreach_get(buf)
    bpy.data.images.remove(image)
    return buf.reshape(h, w, 4)[:, :, :3]


def contrast(scene, cam, names, scale=25):
    """How far each text's letters stand out from their ground on the current frame, as
    a luminance ratio: {name: ratio}. The frame is rendered twice, with the type and
    without it (scrims stay); where the two differ are the letters, and what the second
    shows there is their ground. A text too small to measure at this scale gets no
    figure. (Measuring against the pixels that border the letters was tried and read
    low for everything: at these sizes the border is mostly anti-aliased edge.)"""
    render = scene.render
    kept = (render.resolution_percentage, render.filepath, render.image_settings.file_format, render.use_motion_blur, scene.eevee.taa_render_samples)
    render.resolution_percentage, render.image_settings.file_format, render.use_motion_blur, scene.eevee.taa_render_samples = scale, "PNG", False, 4
    letters = _type_objects() + [obj for obj in bpy.data.collections["Type"].objects if "Rim" in obj.name]
    hidden = [obj.hide_render for obj in letters]
    folder = Path(tempfile.gettempdir()) / "kurbis_audit"
    folder.mkdir(exist_ok=True)
    try:
        with_type = _render(scene, folder / "with.png")
        for obj in letters:
            obj.hide_render = True
        without = _render(scene, folder / "without.png")
    finally:
        for obj, was in zip(letters, hidden):
            obj.hide_render = was
        render.resolution_percentage, render.filepath, render.image_settings.file_format, render.use_motion_blur, scene.eevee.taa_render_samples = kept
    h, w = with_type.shape[:2]
    ratios = {}
    for name in names:
        seen = [world_to_camera_view(scene, cam, corner) for corner in _corners(bpy.data.objects[name])]
        x0, x1 = (int(np.clip(f(v.x for v in seen), 0, 1) * w) for f in (min, max))
        y0, y1 = (int(np.clip(f(v.y for v in seen), 0, 1) * h) for f in (min, max))
        x0, y0 = max(x0 - 3, 0), max(y0 - 3, 0)
        a, b = with_type[y0 : y1 + 4, x0 : x1 + 4], without[y0 : y1 + 4, x0 : x1 + 4]
        drawn = np.abs(a - b).max(axis=2) > 0.08
        if drawn.sum() < 10:
            continue
        ink, ground = _luminance(a[drawn]), float(np.median(_luminance(b[drawn])))
        face = float(np.percentile(ink, 70 if np.median(ink) > ground else 30))  # the letters' faces, not their sides
        ratios[name] = round((max(face, ground) + 0.05) / (min(face, ground) + 0.05), 1)
    return ratios


def run(scene, stops, contrast_scale=25):
    """`stops` is [(name, frame)]. Returns {stop: {object: finding}} for everything in
    frame. `contrast_scale` is the size of the renders the contrast is measured on, in
    percent of the frame (small type needs them large; 0 leaves contrast out)."""
    cam = scene.camera
    report = {}
    for name, frame in stops:
        scene.frame_set(frame)
        depsgraph = bpy.context.evaluated_depsgraph_get()
        found = {}
        for obj in _type_objects():
            finding = check(scene, depsgraph, cam, obj)
            if finding is not None:
                found[obj.name] = finding
        if contrast_scale and found:
            for text, ratio in contrast(scene, cam, list(found), contrast_scale).items():
                found[text]["contrast"] = ratio
                if ratio < MIN_CONTRAST:
                    found[text]["problems"].append("faint")
        report[name] = found
    return report


def summary(report):
    """The findings that need work, as short lines."""
    lines = []
    for stop, found in report.items():
        bad = {name: f for name, f in found.items() if f["problems"]}
        lines.append(f"{stop}: {len(found)} texts in frame, {len(bad)} with problems")
        for name, f in bad.items():
            lines.append(f"  {name}: {', '.join(f['problems'])} (line {f['line_px']} px, {int(f['hidden'] * 100)}% covered{' by ' + ', '.join(f['by']) if f['by'] else ''}{', contrast ' + str(f['contrast']) if 'contrast' in f else ''})")
    return lines
