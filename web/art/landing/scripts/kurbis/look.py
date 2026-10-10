"""Trailer look: gradient sky, low warm sun, haze with light shafts, bloom.

Property names move between Blender versions, so settings are applied
defensively and anything unavailable is reported instead of raising.
"""

import math

import bpy
from mathutils import Vector

from . import core


def _try(obj, report, **props):
    for key, value in props.items():
        try:
            setattr(obj, key, value)
        except Exception as exc:
            report[key] = f"unavailable ({type(exc).__name__})"


def render_settings(scene, width=1920, height=1080, samples=64, view="Standard", view_look="None", exposure=0.0):
    report = {}
    r = scene.render
    r.engine = "BLENDER_EEVEE"
    r.resolution_x, r.resolution_y, r.resolution_percentage = width, height, 100
    r.image_settings.file_format = "PNG"
    r.image_settings.color_mode = "RGB"
    r.film_transparent = False
    scene.view_settings.view_transform = view
    _try(scene.view_settings, report, look=view_look, exposure=exposure)
    _try(
        scene.eevee,
        report,
        taa_render_samples=samples,
        use_raytracing=True,
        ray_tracing_method="SCREEN",
        use_shadows=True,
        shadow_ray_count=2,
        shadow_step_count=8,
        use_fast_gi=True,
        fast_gi_distance=6.0,
        volumetric_start=0.3,
        volumetric_end=420.0,
        volumetric_tile_size="4",
        volumetric_samples=96,
        volumetric_sample_distribution=0.85,
        use_volumetric_shadows=True,
        volumetric_shadow_samples=16,
    )
    return report


def sky(scene, sun_dir, stops, glow, glow_power=10.0, glow_gain=1.0, disc=6.0, disc_power=900.0, stars=0.0, strength=1.0):
    """Vertical gradient (`stops`: elevation -1..1 -> rgb), a glow and a disc
    around the sun, and optional stars high up."""
    world = bpy.data.worlds.new("Sky")
    world.use_nodes = True
    nt = world.node_tree
    nt.nodes.clear()
    out = nt.nodes.new("ShaderNodeOutputWorld")
    bg = nt.nodes.new("ShaderNodeBackground")
    bg.inputs["Strength"].default_value = strength
    coord = nt.nodes.new("ShaderNodeTexCoord")
    norm = nt.nodes.new("ShaderNodeVectorMath")
    norm.operation = "NORMALIZE"
    nt.links.new(coord.outputs["Generated"], norm.inputs[0])
    sep = nt.nodes.new("ShaderNodeSeparateXYZ")
    nt.links.new(norm.outputs["Vector"], sep.inputs[0])
    remap = nt.nodes.new("ShaderNodeMapRange")
    remap.inputs["From Min"].default_value = -1.0
    remap.inputs["From Max"].default_value = 1.0
    nt.links.new(sep.outputs["Z"], remap.inputs["Value"])
    ramp = nt.nodes.new("ShaderNodeValToRGB")
    cr = ramp.color_ramp
    stops = sorted(stops)
    cr.elements[0].position, cr.elements[0].color = stops[0][0] * 0.5 + 0.5, (*stops[0][1], 1)
    cr.elements[1].position, cr.elements[1].color = stops[-1][0] * 0.5 + 0.5, (*stops[-1][1], 1)
    for elevation, color in stops[1:-1]:
        cr.elements.new(elevation * 0.5 + 0.5).color = (*color, 1)
    nt.links.new(remap.outputs["Result"], ramp.inputs["Fac"])

    dot = nt.nodes.new("ShaderNodeVectorMath")
    dot.operation = "DOT_PRODUCT"
    dot.inputs[1].default_value = Vector(sun_dir).normalized()
    nt.links.new(norm.outputs["Vector"], dot.inputs[0])
    clamp = nt.nodes.new("ShaderNodeClamp")
    nt.links.new(dot.outputs["Value"], clamp.inputs["Value"])

    def math(op, a, b):
        n = nt.nodes.new("ShaderNodeMath")
        n.operation = op
        for socket, value in zip(n.inputs, (a, b)):
            if isinstance(value, (int, float)):
                socket.default_value = value
            else:
                nt.links.new(value, socket)
        return n.outputs[0]

    lobes = math("ADD", math("MULTIPLY", math("POWER", clamp.outputs[0], glow_power), glow_gain), math("MULTIPLY", math("POWER", clamp.outputs[0], disc_power), disc))
    tint = nt.nodes.new("ShaderNodeVectorMath")
    tint.operation = "SCALE"
    tint.inputs[0].default_value = glow
    nt.links.new(lobes, tint.inputs["Scale"])
    add = nt.nodes.new("ShaderNodeVectorMath")
    add.operation = "ADD"
    nt.links.new(ramp.outputs["Color"], add.inputs[0])
    nt.links.new(tint.outputs["Vector"], add.inputs[1])
    color = add.outputs["Vector"]

    if stars:
        cells = nt.nodes.new("ShaderNodeTexVoronoi")
        cells.inputs["Scale"].default_value = 130.0
        nt.links.new(norm.outputs["Vector"], cells.inputs["Vector"])
        speck = math("LESS_THAN", cells.outputs["Distance"], 0.045)
        high = nt.nodes.new("ShaderNodeMapRange")
        high.inputs["From Min"].default_value = 0.16
        high.inputs["From Max"].default_value = 0.6
        nt.links.new(sep.outputs["Z"], high.inputs["Value"])
        starlight = nt.nodes.new("ShaderNodeVectorMath")
        starlight.operation = "SCALE"
        starlight.inputs[0].default_value = (1.0, 0.92, 0.82)
        nt.links.new(math("MULTIPLY", math("MULTIPLY", speck, high.outputs["Result"]), stars), starlight.inputs["Scale"])
        more = nt.nodes.new("ShaderNodeVectorMath")
        more.operation = "ADD"
        nt.links.new(color, more.inputs[0])
        nt.links.new(starlight.outputs["Vector"], more.inputs[1])
        color = more.outputs["Vector"]

    nt.links.new(color, bg.inputs["Color"])
    nt.links.new(bg.outputs[0], out.inputs["Surface"])
    scene.world = world
    return world


def aim(obj, direction):
    obj.rotation_euler = Vector(direction).to_track_quat("-Z", "Y").to_euler()


def sun(name, toward_sun, energy, color, angle_deg=2.5, volume=1.0, col="Lights"):
    data = bpy.data.lights.new(name, "SUN")
    data.energy, data.color, data.angle = energy, color, math.radians(angle_deg)
    _try(data, {}, volume_factor=volume)
    obj = core.link(bpy.data.objects.new(name, data), col)
    aim(obj, -Vector(toward_sun))
    return obj


def point(name, location, energy, color, radius=0.25, shadow=True, volume=1.0, col="Lights"):
    data = bpy.data.lights.new(name, "POINT")
    data.energy, data.color, data.shadow_soft_size = energy, color, radius
    data.use_shadow = shadow
    _try(data, {}, volume_factor=volume)
    obj = core.link(bpy.data.objects.new(name, data), col)
    obj.location = location
    return obj


def spot(name, location, target, energy, color, angle_deg=40.0, blend=0.6, radius=0.3, volume=1.0, col="Lights"):
    data = bpy.data.lights.new(name, "SPOT")
    data.energy, data.color, data.shadow_soft_size = energy, color, radius
    data.spot_size, data.spot_blend = math.radians(angle_deg), blend
    _try(data, {}, volume_factor=volume)
    obj = core.link(bpy.data.objects.new(name, data), col)
    obj.location = location
    aim(obj, Vector(target) - Vector(location))
    return obj


def area(name, location, target, energy, color, size=6.0, shadow=False, col="Lights"):
    data = bpy.data.lights.new(name, "AREA")
    data.energy, data.color, data.size = energy, color, size
    data.use_shadow = shadow
    _try(data, {}, volume_factor=0.0)
    obj = core.link(bpy.data.objects.new(name, data), col)
    obj.location = location
    aim(obj, Vector(target) - Vector(location))
    return obj


def fog(name, lo, hi, density, color=(1.0, 0.86, 0.72), anisotropy=0.55, fade=None, col="FX"):
    """Box of haze between two corners. `fade=(z0, z1)` thins it out with height."""
    bpy.ops.mesh.primitive_cube_add(size=1)
    obj = bpy.context.active_object
    obj.name = name
    lo, hi = Vector(lo), Vector(hi)
    obj.location = (lo + hi) / 2
    obj.scale = hi - lo
    for c in obj.users_collection:
        c.objects.unlink(obj)
    core.link(obj, col)
    mat = bpy.data.materials.new(name)
    mat.use_nodes = True
    nt = mat.node_tree
    nt.nodes.clear()
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    scatter = nt.nodes.new("ShaderNodeVolumeScatter")
    scatter.inputs["Color"].default_value = (*color, 1)
    scatter.inputs["Density"].default_value = density
    scatter.inputs["Anisotropy"].default_value = anisotropy
    if fade is not None:
        geo = nt.nodes.new("ShaderNodeNewGeometry")
        sep = nt.nodes.new("ShaderNodeSeparateXYZ")
        nt.links.new(geo.outputs["Position"], sep.inputs[0])
        ramp = nt.nodes.new("ShaderNodeMapRange")
        ramp.inputs["From Min"].default_value = fade[0]
        ramp.inputs["From Max"].default_value = fade[1]
        ramp.inputs["To Min"].default_value = density
        ramp.inputs["To Max"].default_value = 0.0
        nt.links.new(sep.outputs["Z"], ramp.inputs["Value"])
        nt.links.new(ramp.outputs["Result"], scatter.inputs["Density"])
    nt.links.new(scatter.outputs[0], out.inputs["Volume"])
    obj.data.materials.append(mat)
    obj.display_type = "WIRE"
    return obj


def camera(name, location, target, lens=35.0, fstop=None, focus=None, col="Cameras"):
    data = bpy.data.cameras.new(name)
    data.lens = lens
    data.clip_start, data.clip_end = 0.1, 1200.0
    obj = core.link(bpy.data.objects.new(name, data), col)
    obj.location = location
    aim(obj, Vector(target) - Vector(location))
    if fstop is not None:
        data.dof.use_dof = True
        data.dof.aperture_fstop = fstop
        data.dof.focus_distance = (Vector(focus if focus is not None else target) - Vector(location)).length
    return obj


def compositor(scene, bloom=0.5, threshold=1.0, size=0.55, vignette=0.0, saturation=1.0, contrast=0.0):
    """Bloom, grade and vignette (Blender 5.x: node group on the scene, menu sockets)."""
    done = []
    try:
        ng = bpy.data.node_groups.new("KurbisComposite", "CompositorNodeTree")
        ng.interface.new_socket("Image", in_out="OUTPUT", socket_type="NodeSocketColor")
        layers = ng.nodes.new("CompositorNodeRLayers")
        out = ng.nodes.new("NodeGroupOutput")
        glare = ng.nodes.new("CompositorNodeGlare")
        glare.inputs["Type"].default_value = "Bloom"
        glare.inputs["Quality"].default_value = "High"
        glare.inputs["Threshold"].default_value = threshold
        glare.inputs["Strength"].default_value = bloom
        glare.inputs["Size"].default_value = size
        ng.links.new(layers.outputs["Image"], glare.inputs["Image"])
        image = glare.outputs["Image"]
        done.append("bloom")
    except Exception as exc:
        return f"compositor unavailable ({type(exc).__name__}: {exc})"
    try:
        if saturation != 1.0:
            hsv = ng.nodes.new("CompositorNodeHueSat")
            hsv.inputs["Saturation"].default_value = saturation
            ng.links.new(image, hsv.inputs["Image"])
            image = hsv.outputs["Image"]
        if contrast:
            bc = ng.nodes.new("CompositorNodeBrightContrast")
            bc.inputs["Contrast"].default_value = contrast
            ng.links.new(image, bc.inputs["Image"])
            image = bc.outputs["Image"]
        done.append("grade")
    except Exception as exc:
        done.append(f"grade failed ({exc})")
    try:
        if vignette:
            mask = ng.nodes.new("CompositorNodeEllipseMask")
            mask.inputs["Size"].default_value = (0.9, 0.86)
            blur = ng.nodes.new("CompositorNodeBlur")
            blur.inputs["Size"].default_value = (scene.render.resolution_x * 0.22, scene.render.resolution_x * 0.22)
            ng.links.new(mask.outputs["Mask"], blur.inputs["Image"])
            mix = ng.nodes.new("ShaderNodeMix")
            mix.data_type = "RGBA"
            mix.blend_type = "MULTIPLY"
            mix.inputs["Factor"].default_value = vignette
            ng.links.new(image, mix.inputs["A"])
            ng.links.new(blur.outputs["Image"], mix.inputs["B"])
            image = mix.outputs["Result"]
            done.append("vignette")
    except Exception as exc:
        done.append(f"vignette failed ({exc})")
    ng.links.new(image, out.inputs["Image"])
    scene.compositing_node_group = ng
    return ", ".join(done)
