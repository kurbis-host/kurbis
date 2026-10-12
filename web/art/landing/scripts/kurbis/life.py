"""Life: what moves by itself, in real time, whatever the scroll does.

Wind, water and drifting flakes are geometry-node modifiers that read the scene's
clock, so they need no keys and never end. They are for the previs only: the
browser does the same with shaders. Creatures and character loops are keyed
(see anim.py), and do export.
"""

import math

import bpy
import numpy as np
from mathutils import Vector

from . import anim, core

# The wind blows a little north of east: slow gusts with a quick flutter on top.
WIND = (1.0, 0.35, 0.0)
GUST = ((0.35, 0.22, 0.10), 1.3)  # how the gusts lie in space, and how fast they pass
FLUTTER = ((1.3, -0.9, 0.7), 3.1)
# Two swells crossing each other: (direction and wavelength, speed, phase, share)
SWELLS = (((0.9, 0.6), 1.6, 0.0, 0.6), ((-0.5, 1.1), 2.3, 1.0, 0.4))


class _Tree:
    """Writes a node tree as nested calls: every method returns an output socket."""

    def __init__(self, name):
        self.tree = bpy.data.node_groups.new(name, "GeometryNodeTree")
        self.inputs = {}
        face = self.tree.interface
        face.new_socket("Geometry", in_out="INPUT", socket_type="NodeSocketGeometry")
        face.new_socket("Geometry", in_out="OUTPUT", socket_type="NodeSocketGeometry")
        self.enter = self.tree.nodes.new("NodeGroupInput")
        self.leave = self.tree.nodes.new("NodeGroupOutput")

    def param(self, name, default, kind="NodeSocketFloat"):
        socket = self.tree.interface.new_socket(name, in_out="INPUT", socket_type=kind)
        socket.default_value = default
        self.inputs[name] = socket.identifier
        return self.enter.outputs[name]

    def _set(self, socket, value):
        if hasattr(value, "is_linked") or isinstance(value, bpy.types.NodeSocket):
            self.tree.links.new(value, socket)
        else:
            socket.default_value = value

    def node(self, kind, outputs=0, **inputs):
        node = self.tree.nodes.new(kind)
        for key, value in inputs.items():
            self._set(node.inputs[key], value)
        return node.outputs[outputs]

    def math(self, op, a, b=0.0, c=None):
        node = self.tree.nodes.new("ShaderNodeMath")
        node.operation = op
        self._set(node.inputs[0], a)
        self._set(node.inputs[1], b)
        if c is not None:
            self._set(node.inputs[2], c)
        return node.outputs[0]

    def dot(self, a, b):
        node = self.tree.nodes.new("ShaderNodeVectorMath")
        node.operation = "DOT_PRODUCT"
        self._set(node.inputs[0], a)
        self._set(node.inputs[1], b)
        return node.outputs["Value"]

    def vec(self, op, a, b=None, scale=None):
        node = self.tree.nodes.new("ShaderNodeVectorMath")
        node.operation = op
        self._set(node.inputs[0], a)
        if b is not None:
            self._set(node.inputs[1], b)
        if scale is not None:
            self._set(node.inputs["Scale"], scale)
        return node.outputs["Vector"]

    def xyz(self, x=0.0, y=0.0, z=0.0):
        node = self.tree.nodes.new("ShaderNodeCombineXYZ")
        for socket, value in zip(node.inputs, (x, y, z)):
            self._set(socket, value)
        return node.outputs[0]

    def part(self, vector, axis):
        node = self.tree.nodes.new("ShaderNodeSeparateXYZ")
        self._set(node.inputs[0], vector)
        return node.outputs[axis]

    def attribute(self, name, kind="FLOAT"):
        node = self.tree.nodes.new("GeometryNodeInputNamedAttribute")
        node.data_type = kind
        node.inputs["Name"].default_value = name
        return node.outputs["Attribute"]

    def position(self):
        return self.tree.nodes.new("GeometryNodeInputPosition").outputs[0]

    def seconds(self):
        return self.tree.nodes.new("GeometryNodeInputSceneTime").outputs["Seconds"]

    def finish(self, position=None, offset=None):
        move = self.tree.nodes.new("GeometryNodeSetPosition")
        self.tree.links.new(self.enter.outputs["Geometry"], move.inputs["Geometry"])
        if position is not None:
            self.tree.links.new(position, move.inputs["Position"])
        if offset is not None:
            self.tree.links.new(offset, move.inputs["Offset"])
        self.tree.links.new(move.outputs["Geometry"], self.leave.inputs["Geometry"])
        return self.tree


class _Paint(_Tree):
    """The same writing for a material's own node tree. A material cannot read the
    scene's clock, so it gets a value the timeline keeps at the seconds passed."""

    def __init__(self, material):
        self.tree = material.node_tree

    def position(self):
        return self.tree.nodes.new("ShaderNodeNewGeometry").outputs["Position"]

    def normal(self):
        return self.tree.nodes.new("ShaderNodeNewGeometry").outputs["Normal"]

    def clock(self, rate):
        """Seconds passed, in steps: `rate` redraws a second, as the game animates a texture."""
        node = self.tree.nodes.new("ShaderNodeValue")
        node.name = node.label = "Clock"
        anim.live(node.outputs[0], "default_value", lambda seconds: seconds)
        return self.math("DIVIDE", self.math("FLOOR", self.math("MULTIPLY", node.outputs[0], rate)), rate)

    def pixel(self, value):
        """Snapped to the blocks' own pixel grid, sixteen to a block."""
        return self.math("DIVIDE", self.math("FLOOR", self.math("MULTIPLY", value, 16.0)), 16.0)

    def blend(self, share, a, b):
        """a where share is 0, b where it is 1."""
        return self.math("ADD", self.math("MULTIPLY", a, self.math("SUBTRACT", 1.0, share)), self.math("MULTIPLY", b, share))

    def between(self, value, lo, hi, out=(0.0, 1.0)):
        node = self.tree.nodes.new("ShaderNodeMapRange")
        for key, to in (("Value", value), ("From Min", lo), ("From Max", hi), ("To Min", out[0]), ("To Max", out[1])):
            self._set(node.inputs[key], to)
        return node.outputs["Result"]

    def noise(self, where, w, scale=1.0, detail=1.0):
        node = self.tree.nodes.new("ShaderNodeTexNoise")
        node.noise_dimensions = "4D"
        for key, to in (("Vector", where), ("W", w), ("Scale", scale), ("Detail", detail)):
            self._set(node.inputs[key], to)
        return node.outputs[0]


def shimmer(material, color=(1.0, 0.80, 0.66), strength=0.85, rate=8.0):
    """Light playing on water: thin bright streaks drawn on the pixel grid and redrawn a
    few times a second. They lie along X, drift slowly, and a second, slower pattern
    breaks them into dashes that come and go. On a face seen in section (the pond cut
    open) they lie along its length the same way."""
    t = _Paint(material)
    bsdf = material.node_tree.nodes["Principled BSDF"]
    now, pos = t.clock(rate), t.position()
    top = t.math("GREATER_THAN", t.math("ABSOLUTE", t.part(t.normal(), "Z")), 0.5)
    across, along = t.pixel(t.part(pos, "X")), t.pixel(t.blend(top, t.part(pos, "Z"), t.part(pos, "Y")))
    wave = material.node_tree.nodes.new("ShaderNodeTexWave")
    wave.wave_type, wave.bands_direction = "BANDS", "Y"
    for key, to in (("Vector", t.xyz(t.math("MULTIPLY", across, 0.13), t.math("MULTIPLY", along, 0.42))), ("Scale", 1.0), ("Distortion", 2.6), ("Detail", 1.0), ("Detail Scale", 7.0), ("Phase Offset", t.math("MULTIPLY", now, 0.75))):
        t._set(wave.inputs[key], to)
    dashes = t.noise(t.xyz(t.math("MULTIPLY", across, 0.8), t.math("MULTIPLY", along, 1.1)), t.math("MULTIPLY", now, 0.3))
    lit = t.math("MULTIPLY", t.between(wave.outputs["Fac"], 0.90, 0.95), t.between(dashes, 0.46, 0.54))
    alpha = bsdf.inputs["Alpha"].default_value
    bsdf.inputs["Emission Color"].default_value = (*color, 1.0)
    t._set(bsdf.inputs["Emission Strength"], t.math("MULTIPLY", lit, strength))
    t._set(bsdf.inputs["Alpha"], t.math("ADD", alpha, t.math("MULTIPLY", lit, (1.0 - alpha) * 0.7)))


def crawl(material, rate=6.0, run=1.5):
    """Lava is never still: its glow crawls in slow blotches on the pixel grid, and down
    the sides of a fall the blotches run. Returns the socket that sets how strongly the
    material glows over all, for the timeline to keep."""
    t = _Paint(material)
    tree = material.node_tree
    strength = tree.nodes["EmitStrength"].inputs["Scale"]
    glow = tree.nodes.new("ShaderNodeValue")
    glow.name = glow.label = "Glow"
    glow.outputs[0].default_value = strength.default_value
    now, pos = t.clock(rate), t.position()
    side = t.math("LESS_THAN", t.math("ABSOLUTE", t.part(t.normal(), "Z")), 0.5)
    down = t.math("ADD", t.part(pos, "Z"), t.math("MULTIPLY", t.math("MULTIPLY", now, run), side))  # only a fall shows its sides
    where = t.xyz(t.math("MULTIPLY", t.pixel(t.part(pos, "X")), 0.5), t.math("MULTIPLY", t.pixel(t.part(pos, "Y")), 0.5), t.math("MULTIPLY", t.pixel(down), 0.8))
    blotch = t.noise(where, t.math("MULTIPLY", now, 0.2), detail=2.0)
    tree.links.new(t.math("MULTIPLY", glow.outputs[0], t.between(blotch, 0.32, 0.68, (0.7, 1.35))), strength)
    return glow.outputs[0]


def picture(rows, ink):
    """A few pixels drawn in text: one string per row, top first, `ink` gives each
    letter its colour (anything else is clear). Returns float RGBA, row 0 at the bottom."""
    return np.array([[(*ink[c], 1.0) if c in ink else (0.0, 0.0, 0.0, 0.0) for c in row] for row in rows[::-1]], np.float32)


def flier(name, wing, size, home, reach, speed=1.0, beat=6.0, seed=0, body=None, col="FX", emit=0.0):
    """A small thing on two wings (a butterfly, a bat). The wings are flat tiles on a
    hinge that beat for ever; the whole wanders round `home`, never further than
    `reach`, on a path that does not quite repeat, and faces the way it flies.
    `wing` is the picture of one wing, hinge at its left edge; `body` is (girth,
    material) for one that has a body worth showing. Returns its root."""
    from . import boxmodel, scenery

    root = core.empty(name, col, home)
    span, length = size
    for side in (-1.0, 1.0):
        tile = boxmodel.image_tiles(f"{name}_{'l' if side < 0 else 'r'}", wing, [[(0.0, -length / 2, 0.0), (side * span, -length / 2, 0.0), (side * span, length / 2, 0.0), (0.0, length / 2, 0.0)]], col=col, emit=emit)
        tile.parent = root
        tile.visible_shadow = False
        anim.live(tile, "rotation_euler", lambda t, side=side: (0.0, -side * math.radians(48.0 + 38.0 * math.sin(t * 2 * math.pi * beat + seed)), 0.0))  # mostly held up: seen from the side they still show
    if body is not None:
        girth, material = body
        trunk = scenery.boxes_object(f"{name}_body", [((-girth / 2, -length / 2, -girth / 2), (girth, length, girth))], material, col)
        trunk.parent = root
        trunk.visible_shadow = False
    turns = [(0.71, 1.93), (0.53, 1.61), (0.97, 2.39)]  # two unrelated turns on each axis
    phases = [(seed * 1.7 + 2.1 * k, seed * 0.9 + 1.3 * k) for k in range(3)]

    def at(t):
        here = [reach[k] * (0.62 * math.sin(a * speed * t + p) + 0.38 * math.sin(b * speed * t + q)) for k, ((a, b), (p, q)) in enumerate(zip(turns, phases))]
        return Vector(home) + Vector(here) + Vector((0.0, 0.0, 0.05 * math.sin(t * 9.0 + seed)))

    def facing(t):
        going = at(t + 0.04) - at(t - 0.04)
        return (0.0, 0.0, math.atan2(going.y, going.x) - math.pi / 2)

    anim.live(root, "location", lambda t: tuple(at(t)))
    anim.live(root, "rotation_euler", facing)
    return root


def drip(name, top, floor, period, phase, drop, ring, gravity=14.0):
    """A drop that falls from the roof every `period` seconds and leaves a ring where it
    lands. `drop` and `ring` are the two objects to move (a splinter of water, a ring)."""
    fall = math.sqrt(2.0 * (top[2] - floor) / gravity)

    def falling(t):
        u = (t + phase) % period
        return (top[0], top[1], top[2] - 0.5 * gravity * u * u) if u < fall else (top[0], top[1], floor)

    anim.live(drop, "location", falling)
    anim.live(drop, "scale", lambda t: (1.0, 1.0, 1.0) if (t + phase) % period < fall else (0.0, 0.0, 0.0))
    ring.location = (top[0], top[1], floor + 0.02)
    spread = lambda t: ((t + phase) % period - fall) / 0.7  # noqa: E731
    anim.live(ring, "scale", lambda t: (0.25 + 0.7 * min(max(spread(t), 0.0), 1.0),) * 2 + (1.0,))
    anim.live(ring, "color", lambda t: (1.0, 1.0, 1.0, 0.7 * (1.0 - spread(t)) if 0.0 <= spread(t) <= 1.0 else 0.0))


def _apply(obj, name, build, **values):
    tree = bpy.data.node_groups.get(name)
    if tree is None:
        made = build(name)
        tree = made.tree
        tree["inputs"] = made.inputs
    mod = obj.modifiers.new(name, "NODES")
    mod.node_group = tree
    for key, value in values.items():
        getattr(mod.properties.inputs, tree["inputs"][key]).value = value  # Blender 5: no longer `mod[identifier]`
    return mod


def _wind(name):
    t = _Tree(name)
    strength, rooted = t.param("Strength", 0.1), t.param("Rooted", 0.0)
    pos, now = t.position(), t.seconds()
    gust = t.math("SINE", t.math("SUBTRACT", t.dot(pos, GUST[0]), t.math("MULTIPLY", now, GUST[1])))
    flutter = t.math("SINE", t.math("ADD", t.dot(pos, FLUTTER[0]), t.math("MULTIPLY", now, FLUTTER[1])))
    # A rooted thing moves by its "sway" attribute (how far it is from its root); a loose one moves whole.
    weight = t.math("ADD", 1.0, t.math("MULTIPLY", rooted, t.math("SUBTRACT", t.attribute("sway"), 1.0)))
    amount = t.math("MULTIPLY", strength, weight)
    push = t.math("MULTIPLY", t.math("ADD", gust, t.math("MULTIPLY", flutter, 0.3)), amount)
    lift = t.math("MULTIPLY", t.math("MULTIPLY", flutter, 0.15), amount)
    t.finish(offset=t.xyz(t.math("MULTIPLY", push, WIND[0]), t.math("MULTIPLY", push, WIND[1]), lift))
    return t


def wind(obj, strength=0.1, rooted=False):
    """Sway in the wind. The push depends only on where a point is, so corners that
    meet stay together. `rooted` things need a "sway" point attribute."""
    core.note(obj, wind={"strength": strength, "rooted": bool(rooted)})
    return _apply(obj, "Wind", _wind, Strength=strength, Rooted=1.0 if rooted else 0.0)


def _waves(name):
    t = _Tree(name)
    level, height = t.param("Level", 1.0), t.param("Height", 0.05)
    pos, now = t.position(), t.seconds()
    swell = None
    for (kx, ky), speed, phase, share in SWELLS:
        angle = t.math("ADD", t.math("SUBTRACT", t.dot(pos, (kx, ky, 0.0)), t.math("MULTIPLY", now, speed)), phase)
        one = t.math("MULTIPLY", t.math("SINE", angle), share)
        swell = one if swell is None else t.math("ADD", swell, one)
    surface = t.math("COMPARE", t.part(pos, "Z"), level, 0.04)  # on the surface, or lying on it
    t.finish(offset=t.xyz(0.0, 0.0, t.math("MULTIPLY", t.math("MULTIPLY", swell, height), surface)))
    return t


def waves(obj, level, height=0.05):
    """Swell on a water surface at `level`: only points on the surface (or lying on
    it, such as lily pads) move, and only up and down."""
    core.note(obj, waves=core.noted(obj).get("waves", []) + [{"level": level, "height": height}])
    return _apply(obj, "Waves", _waves, Level=level, Height=height)


def swell(x, y, tau, height=0.05):
    """The same swell in Python, for things that float (a bobber) and are keyed."""
    seconds = tau + 1.0 / anim.FPS  # the node's clock starts at frame 0, ours at frame 1
    return height * sum(share * math.sin(kx * x + ky * y - seconds * speed + phase) for (kx, ky), speed, phase, share in SWELLS)


def _drift(name):
    t = _Tree(name)
    velocity = t.param("Velocity", (0.0, 0.0, -1.0), "NodeSocketVector")
    lo, size = t.param("Low", (0.0, 0.0, 0.0), "NodeSocketVector"), t.param("Size", (1.0, 1.0, 1.0), "NodeSocketVector")
    sway = t.param("Sway", 0.2)
    pos, now, home = t.position(), t.seconds(), t.attribute("centre", "FLOAT_VECTOR")
    phase = t.attribute("phase")
    # Each flake travels with the wind and wraps round inside its box; its corners follow
    # its centre. MODULO keeps the sign of what it divides, so it is folded once more.
    travelled = t.vec("ADD", t.vec("SUBTRACT", home, lo), t.vec("SCALE", velocity, scale=now))
    wrapped = t.vec("MODULO", t.vec("ADD", t.vec("MODULO", travelled, size), size), size)
    centre = t.vec("ADD", wrapped, lo)
    wobble = t.math("MULTIPLY", t.math("SINE", t.math("ADD", t.math("MULTIPLY", now, 1.7), phase)), sway)
    side = t.xyz(wobble, t.math("MULTIPLY", wobble, 0.6), 0.0)
    t.finish(position=t.vec("ADD", t.vec("ADD", centre, t.vec("SUBTRACT", pos, home)), side))
    return t


def drift(obj, velocity, lo, hi, sway=0.2):
    """Flakes that travel for ever: each moves at `velocity`, wraps round inside the box
    lo..hi, and wavers sideways. Needs "centre" and "phase" point attributes."""
    size = tuple(b - a for a, b in zip(lo, hi))
    core.note(obj, drift={"velocity": [float(v) for v in velocity], "sway": sway})
    return _apply(obj, "Drift", _drift, Velocity=tuple(velocity), Low=tuple(lo), Size=size, Sway=sway)


# -- creatures and loops (keyed, one value per frame) --------------------------------
def _ripple(rings, events, u, at, level, tau, height):
    """Rings spread from `at` after each of `events` [(moment in the loop, which ring)].
    Returns {ring: (location, scale, colour)} for this moment."""
    out = {ring: ((at.x, at.y, level + 0.03), (0.0, 0.0, 0.0), (1.0, 1.0, 1.0, 0.0)) for ring in rings}
    for moment, k in events:
        age = u - moment
        if 0.0 <= age < 0.9:
            grow = 0.3 + 1.5 * age / 0.9
            out[rings[k]] = ((at.x, at.y, level + 0.03 + swell(at.x, at.y, tau, height)), (grow, grow, 1.0), (1.0, 1.0, 1.0, 0.75 * (1.0 - age / 0.9)))
    return out


def _arc(a, b, v, height):
    return a.lerp(b, v) + Vector((0.0, 0.0, 4.0 * height * v * (1.0 - v)))


class Fisher:
    """A friend fishing, for ever: wind up, cast, wait, a bite, the strike, and a fish
    that goes into the barrel (or back into the pond). Only the arm and the body are
    looped by hand; the bobber, the line, the fish and the ripples follow from where
    the rod's tip is on each frame."""

    PERIOD = 9.6
    WIND, CAST, LANDS, BITE, STRIKE, CAUGHT, AWAY = 0.45, 0.72, 1.35, 6.0, 6.6, 7.3, 7.95  # moments in the loop

    def __init__(self, rig, rod, tip, bobber, line, fish, rings, float_at, away, level, phase=0.0, height=0.045, keeps=True, size=0.6):
        """`tip` is the rod's tip in the rod's own space, `float_at` where the bobber
        floats, `away` where the fish ends up, and `keeps` whether that is a barrel
        (the fish is kept) or the pond (it is let go, with a splash)."""
        self.rig, self.rod, self.tip, self.bobber, self.line, self.fish, self.rings = rig, rod, tip, bobber, line, fish, rings
        self.float_at, self.away, self.level, self.phase, self.height, self.keeps = Vector(float_at), Vector(away), level, phase, height, keeps
        self.size = (size,) * 3

    def bake(self, session):
        P, c = self.PERIOD, self
        arm, body = self.rig.parts["arm_r"], self.rig.parts["body"]
        rest_arm = tuple(math.degrees(a) for a in arm.rotation_euler)
        rest_body = tuple(math.degrees(a) for a in body.rotation_euler)

        def lean(part, dx):
            return (part[0] + dx, part[1], part[2])

        arm_at = anim.cycle(P, [
            (0.0, rest_arm), (c.WIND, lean(rest_arm, -86)), (c.CAST, lean(rest_arm, 20)), (c.LANDS, rest_arm),
            (c.BITE - 0.05, rest_arm), (c.BITE + 0.06, lean(rest_arm, -12)), (c.BITE + 0.2, lean(rest_arm, 2)), (c.BITE + 0.36, lean(rest_arm, -12)), (c.BITE + 0.5, rest_arm),
            (c.STRIKE + 0.3, lean(rest_arm, -50)), (c.AWAY, lean(rest_arm, -40)), (8.7, rest_arm),
        ], self.phase)
        body_at = anim.cycle(P, [
            (0.0, rest_body), (c.WIND, lean(rest_body, -11)), (c.CAST, lean(rest_body, 7)), (c.LANDS, rest_body),
            (c.BITE, rest_body), (c.STRIKE + 0.3, lean(rest_body, -13)), (c.AWAY, lean(rest_body, -8)), (8.7, rest_body),
        ], self.phase)
        self.line.rotation_mode = "QUATERNION"
        rows = {key: [] for key in ("arm", "body", "bob", "line_at", "line_turn", "line_size", "fish_at", "fish_turn", "fish_size")}
        rings = {ring: ([], [], []) for ring in self.rings}
        events = [(c.LANDS, 0), (c.BITE + 0.05, 1), (c.BITE + 0.35, 0), (c.STRIKE, 2)] + ([] if self.keeps else [(c.AWAY, 1)])
        for f in range(session.frames):
            tau = f / anim.FPS
            u = (tau + self.phase) % P
            turn_arm = tuple(math.radians(a) for a in arm_at(tau))
            turn_body = tuple(math.radians(a) for a in body_at(tau))
            tip = core.world_matrix(self.rod, {arm: turn_arm, body: turn_body}) @ self.tip
            hang = tip + Vector((0.0, 0.0, -0.42))
            afloat = self.float_at + Vector((0.0, 0.0, self.level - 0.04 + swell(self.float_at.x, self.float_at.y, tau, self.height)))
            if c.BITE <= u < c.STRIKE:
                afloat.z -= 0.2 * max(math.sin((u - c.BITE) * 2 * math.pi / 0.3), 0.0)
            if u < c.CAST or u >= c.CAUGHT:
                bob = hang
            elif u < c.LANDS:
                bob = _arc(hang, afloat, anim.smooth((u - c.CAST) / (c.LANDS - c.CAST)), 0.9)
            elif u < c.STRIKE:
                bob = afloat
            else:
                bob = _arc(afloat, hang, anim.smooth((u - c.STRIKE) / (c.CAUGHT - c.STRIKE)), 0.6)
            reach = bob - tip
            rows["arm"].append(turn_arm)
            rows["body"].append(turn_body)
            rows["bob"].append(tuple(bob))
            rows["line_at"].append(tuple(tip))
            rows["line_turn"].append(tuple(reach.to_track_quat("Y", "Z")))
            rows["line_size"].append((1.0, max(reach.length, 0.01), 1.0))
            # The fish: on the hook from the strike, then off to the barrel or back to the pond.
            if c.STRIKE <= u < c.CAUGHT:
                wriggle = 0.5 * math.sin(u * 34.0)
                rows["fish_at"].append(tuple(bob + Vector((0.0, 0.0, -0.36))))
                rows["fish_turn"].append((-math.pi / 2, wriggle, 0.0))
                rows["fish_size"].append(self.size)
            elif c.CAUGHT <= u < c.AWAY:
                v = (u - c.CAUGHT) / (c.AWAY - c.CAUGHT)
                rows["fish_at"].append(tuple(_arc(hang + Vector((0.0, 0.0, -0.36)), self.away, v, 0.9)))
                rows["fish_turn"].append((-math.pi / 2 + 5.0 * v, 0.6 * v, 0.0))
                rows["fish_size"].append(self.size)
            else:
                rows["fish_at"].append(tuple(self.away))
                rows["fish_turn"].append((0.0, 0.0, 0.0))
                rows["fish_size"].append((0.0, 0.0, 0.0))
            ripples = _ripple(self.rings, events, u, self.float_at if u < c.AWAY else self.away, self.level, tau, self.height)
            for ring, (at, size, colour) in ripples.items():
                for row, value in zip(rings[ring], (at, size, colour)):
                    row.append(value)
        anim.write(arm, "rotation_euler", rows["arm"])
        anim.write(body, "rotation_euler", rows["body"])
        anim.write(self.bobber, "location", rows["bob"])
        anim.write(self.line, "location", rows["line_at"])
        anim.write(self.line, "rotation_quaternion", rows["line_turn"])
        anim.write(self.line, "scale", rows["line_size"])
        anim.write(self.fish, "location", rows["fish_at"])
        anim.write(self.fish, "rotation_euler", rows["fish_turn"])
        anim.write(self.fish, "scale", rows["fish_size"])
        for ring, (at, size, colour) in rings.items():
            anim.write(ring, "location", at)
            anim.write(ring, "scale", size)
            anim.write(ring, "color", colour)


class Jumper:
    """A fish that leaps out of the water every few seconds, somewhere new each time."""

    def __init__(self, fish, rings, spots, level, period=5.2, phase=0.0, height=0.045, size=0.6, once=None):
        """`spots` are (where it leaves the water, where it comes back) pairs. With `once`,
        it leaps a single time, at that moment (somebody clicked the pond)."""
        self.fish, self.rings, self.spots, self.level, self.period, self.phase, self.height = fish, rings, spots, level, period, phase, height
        self.size, self.once = (size,) * 3, once

    def bake(self, session):
        leap = (0.5, 1.25)  # when in each period it is in the air
        at, turn, size = [], [], []
        rings = {ring: ([], [], []) for ring in self.rings}
        for f in range(session.frames):
            tau = f / anim.FPS
            clock = tau + self.phase if self.once is None else tau - self.once + leap[0]
            out, back = (Vector((*p, self.level)) for p in self.spots[int(clock // self.period) % len(self.spots) if self.once is None else 0])
            u = clock % self.period if self.once is None else clock
            if leap[0] <= u < leap[1]:
                v = (u - leap[0]) / (leap[1] - leap[0])
                here, soon = _arc(out, back, v, 0.75), _arc(out, back, min(v + 0.02, 1.0), 0.75)
                going = soon - here if v < 0.98 else here - _arc(out, back, v - 0.02, 0.75)
                at.append(tuple(here))
                turn.append((-math.atan2(going.z, math.hypot(going.x, going.y)), 0.0, math.atan2(going.x, -going.y)))
                size.append(self.size)
            else:
                at.append(tuple(out))
                turn.append((0.0, 0.0, 0.0))
                size.append((0.0, 0.0, 0.0))
            for ring, moment, where in ((self.rings[0], leap[0], out), (self.rings[1], leap[1], back)):
                ripple = _ripple([ring], [(moment, 0)], u, where, self.level, tau, self.height)[ring]
                for row, value in zip(rings[ring], ripple):
                    row.append(value)
        anim.write(self.fish, "location", at)
        anim.write(self.fish, "rotation_euler", turn)
        anim.write(self.fish, "scale", size)
        for ring, (where, grow, colour) in rings.items():
            anim.write(ring, "location", where)
            anim.write(ring, "scale", grow)
            anim.write(ring, "color", colour)
