"""Render a range of film frames headless, so several ranges can run side by side.

    Blender -b blend/film_<session>.blend -P scripts/render_range.py -- START END OUT_DIR [SCALE] [SAMPLES] [fill]

Frames are written as OUT_DIR/0001.jpg and so on. `film.py` must have built and
saved that session's blend file first.

With `fill`, frames that already exist are left alone and each frame is claimed
with a placeholder before it renders, so several processes can share one range
and split the work between them. Without it, every frame is rendered afresh.
"""

import sys

import bpy

args = sys.argv[sys.argv.index("--") + 1 :]
start, end, out = int(args[0]), int(args[1]), args[2]
scale = float(args[3]) if len(args) > 3 else 0.5
samples = int(args[4]) if len(args) > 4 else 12
fill = len(args) > 5 and args[5] == "fill"

scene = bpy.context.scene
render = scene.render
full = (1920, 1080) if render.resolution_x >= render.resolution_y else (1080, 1920)  # the blend file says which way up
render.resolution_x, render.resolution_y, render.resolution_percentage = int(full[0] * scale), int(full[1] * scale), 100
render.image_settings.file_format = "JPEG"
render.image_settings.quality = 92
render.filepath = out.rstrip("/") + "/"
render.use_overwrite = not fill
render.use_placeholder = fill
scene.eevee.taa_render_samples = samples
scene.frame_start, scene.frame_end = start, end
bpy.ops.render.render(animation=True)
