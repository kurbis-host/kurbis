"""Timeline helpers.

The page runs on two clocks. Scroll position, counted in screens, drives the
camera and the story; real time drives life and reactions. Every animated value
is therefore a function of both. Values are collected on a `Timeline` and baked,
for one `Session` (one way of scrolling through the page), to keys on frames.

Three kinds of motion come out of that:
  scrubbed    a function of the scroll position (the camera, the pumpkin's journey)
  triggered   starts when the scroll passes a point, then plays in real time (a pop)
  live        runs in real time whatever the scroll does (breathing, loops, reactions)
"""

import bisect
import math

import bpy
import numpy as np
from mathutils import Matrix, Quaternion, Vector

FPS = 24
EVER = float("-inf")  # a piece that starts here has always been playing


def frame(t):
    return int(round(t * FPS)) + 1


def clamp01(u):
    return min(max(u, 0.0), 1.0)


def smooth(u):
    u = clamp01(u)
    return u * u * (3 - 2 * u)


def ease_in(u, p=2.0):
    return clamp01(u) ** p


def ease_out(u, p=2.0):
    return 1 - (1 - clamp01(u)) ** p


def back_out(u, s=1.7):
    u = clamp01(u) - 1
    return 1 + u * u * ((s + 1) * u + s)


def lerp(a, b, u):
    return a + (b - a) * u


class Track:
    """Monotone cubic curve through keys [(s, value)]. No overshoot, and it comes to
    rest wherever two neighbouring keys agree. Flat before the first and after the
    last key. `ends="free"` keeps moving through the first and last key."""

    def __init__(self, keys, ends="rest"):
        self.ends = ends
        keys = sorted(keys, key=lambda k: k[0])
        self.t = np.array([k[0] for k in keys], float)
        self.v = np.array([np.atleast_1d(np.asarray(k[1], float)) for k in keys])
        n = len(self.t)
        self.m = np.zeros_like(self.v)
        if n < 2:
            return
        h = np.diff(self.t)[:, None]
        d = np.diff(self.v, axis=0) / h
        for i in range(1, n - 1):
            w1, w2 = 2 * h[i, 0] + h[i - 1, 0], h[i, 0] + 2 * h[i - 1, 0]
            ok = d[i - 1] * d[i] > 0
            with np.errstate(divide="ignore", invalid="ignore"):
                self.m[i] = np.where(ok, (w1 + w2) / (w1 / d[i - 1] + w2 / d[i]), 0.0)
        if ends == "free":
            self.m[0], self.m[-1] = d[0], d[-1]

    def __call__(self, t):
        ts, v, m = self.t, self.v, self.m
        if t <= ts[0]:
            out = v[0]
        elif t >= ts[-1]:
            out = v[-1]
        else:
            i = int(np.searchsorted(ts, t, side="right")) - 1
            h = ts[i + 1] - ts[i]
            u = (t - ts[i]) / h
            h00, h10 = 2 * u**3 - 3 * u**2 + 1, u**3 - 2 * u**2 + u
            h01, h11 = -2 * u**3 + 3 * u**2, u**3 - u**2
            out = h00 * v[i] + h10 * h * m[i] + h01 * v[i + 1] + h11 * h * m[i + 1]
        return float(out[0]) if out.shape[0] == 1 else Vector(out)

    def when(self, value):
        """Where a rising one-value track reaches `value` (by bisection)."""
        lo, hi = float(self.t[0]), float(self.t[-1])
        for _ in range(40):
            mid = (lo + hi) / 2
            if self(mid) < value:
                lo = mid
            else:
                hi = mid
        return hi


# -- sessions ------------------------------------------------------------------
class Session:
    """One visit: scroll position as a function of real time. The visitor scrolls at
    a steady `pace` (seconds per screen) and rests at each of `stops`, given as
    (scroll position, seconds) or (scroll position, seconds, {event: seconds into
    the rest}). Events are what the visitor does while resting: a click, a key."""

    def __init__(self, name, end, pace=2.0, stops=(), lead=0.5, tail=1.5):
        self.name, self.end, self.pace = name, float(end), float(pace)
        rests = [(0.0, lead, {})]
        rests += [(stop[0], stop[1], dict(stop[2]) if len(stop) > 2 else {}) for stop in sorted(stops, key=lambda r: r[0])]
        rests += [(self.end, tail, {})]
        self.spans, self.events = [], {}  # spans: (begins, ends, scroll position) of each rest
        tau, at = 0.0, 0.0
        for s, seconds, events in rests:
            tau += (s - at) * self.pace
            self.spans.append((tau, tau + seconds, s))
            for key, dt in events.items():
                self.events[key] = tau + dt
            tau, at = tau + seconds, s
        self.duration = tau

    @property
    def frames(self):
        return int(round(self.duration * FPS)) + 1

    def scroll(self, tau):
        gone, at = 0.0, 0.0
        for t0, t1, s in self.spans:
            if tau < t0:
                return at + (tau - gone) / self.pace
            if tau <= t1:
                return s
            gone, at = t1, s
        return self.end

    def when(self, s):
        """The moment the scroll first gets to s."""
        gone, at = 0.0, 0.0
        for t0, t1, here in self.spans:
            if s <= here:
                return gone + (s - at) * self.pace
            gone, at = t1, here
        return self.duration + 1.0

    def at(self, event):
        """The moment of a named event, or None if this visitor never does it."""
        return self.events.get(event)

    def resting(self, tau):
        return any(t0 < tau < t1 for t0, t1, _ in self.spans[1:-1])


# -- writing curves --------------------------------------------------------------
def _action(block):
    data = block.animation_data or block.animation_data_create()
    if data.action is None:
        data.action = bpy.data.actions.new(f"{block.name}Action")
    return data.action


def write(owner, path, values):
    """Give a property one value per frame (`values[f]` belongs to frame f + 1). Keys
    are kept only where the value changes, with straight lines between them."""
    data = np.asarray(values, np.float64)
    if data.ndim == 1:
        data = data[:, None]
    if np.all(data == data[0]):
        setattr(owner, path, values[0])
        return 0
    block, full = owner.id_data, owner.path_from_id(path)
    action = _action(block)
    frames = np.arange(1, len(data) + 1, dtype=np.float64)
    made = 0
    for index in range(data.shape[1]):
        v = data[:, index]
        keep = np.ones(len(v), bool)
        keep[1:-1] = (v[1:-1] != v[:-2]) | (v[1:-1] != v[2:])
        co = np.stack([frames[keep], v[keep]], axis=1)
        curve = action.fcurve_ensure_for_datablock(block, full, index=index)
        points = curve.keyframe_points
        points.add(len(co))
        points.foreach_set("co", co.ravel())
        points.foreach_set("interpolation", [1] * len(co))  # linear
        curve.update()
        made += len(co)
    return made


def begins(session, clock, start):
    """The moment a piece (or anything else with a clock) begins in this session."""
    return start if clock == "real" else session.when(start)


class Timeline:
    """Everything animated, as pieces per property. A piece begins at a scroll position
    (or at a moment, for clock="real") and from then on gives the value as
    `fn(s, dt)`: s is the scroll position now, dt the real seconds since the piece
    began. The piece that began last wins; before the first one begins, the property
    holds that piece's opening value.

    A third clock, ("scrub", rate), plays a sequence written in seconds by the scroll
    instead: it begins at a scroll position, and dt is the scroll since then times
    `rate` (seconds per screen). So one sequence can be triggered here and scrubbed there."""

    def __init__(self):
        self.channels = {}
        self.now = 0.0  # the real time of the frame being baked

    def add(self, owner, path, start, fn, clock="scroll"):
        key = (owner.as_pointer(), path)
        self.channels.setdefault(key, (owner, path, []))[2].append((clock, start, fn))

    def bake(self, session):
        n = session.frames
        taus = [f / FPS for f in range(n)]
        scrolls = [session.scroll(tau) for tau in taus]
        keys = 0
        for owner, path, pieces in self.channels.values():
            begun = sorted(((begins(session, clock, start), i) for i, (clock, start, _) in enumerate(pieces)))
            first = pieces[begun[0][1]]
            opening = first[2](session.scroll(max(begun[0][0], 0.0)) if first[0] == "real" else first[1], 0.0)
            values, k = [], -1
            for tau, s in zip(taus, scrolls):
                self.now = tau
                while k + 1 < len(begun) and begun[k + 1][0] <= tau + 1e-9:
                    k += 1
                if k < 0:
                    values.append(opening)
                    continue
                clock, start, fn = pieces[begun[k][1]]
                values.append(fn(s, (s - start) * clock[1] if isinstance(clock, tuple) else tau - begun[k][0]))
            keys += write(owner, path, values)
        return keys


TL = Timeline()


def reset():
    global TL
    TL = Timeline()
    return TL


def now():
    """The real time of the frame being baked, for pieces that mix a loop into their value."""
    return TL.now


def _value(v):
    return float(v[0]) if len(v) == 1 else tuple(float(c) for c in v)


def cycle(period, keys, phase=0.0):
    """A value that loops for ever: smooth steps through [(seconds into the loop, value)],
    and from the last key round to the first. Returns fn(seconds)."""
    keys = sorted(keys, key=lambda k: k[0])
    times = [keys[-1][0] - period] + [k[0] for k in keys] + [keys[0][0] + period]
    values = [np.atleast_1d(np.asarray(k[1], float)) for k in (keys[-1], *keys, keys[0])]

    def fn(t):
        u = (t + phase) % period
        i = min(max(bisect.bisect_right(times, u) - 1, 0), len(times) - 2)
        return _value(values[i] + (values[i + 1] - values[i]) * smooth((u - times[i]) / (times[i + 1] - times[i])))

    return fn


def scrub(owner, path, start, fn):
    """Scrubbed: from scroll position `start` on, the property is fn(scroll position)."""
    TL.add(owner, path, start, lambda s, dt: fn(s))


def after(owner, path, start, fn, clock="scroll"):
    """Triggered: once the scroll gets to `start` (or at that moment, for clock="real"),
    the property is fn(seconds since)."""
    TL.add(owner, path, start, lambda s, dt: fn(dt), clock)


def live(owner, path, fn, start=0.0):
    """Live: from the moment `start` on, the property is fn(seconds since)."""
    TL.add(owner, path, start, lambda s, dt: fn(dt), "real")


def linear(points):
    """fn(x): straight ramps between [(x, value)], flat outside them."""
    xs = [p[0] for p in points]
    vs = [np.atleast_1d(np.asarray(p[1], float)) for p in points]

    def fn(x):
        if x <= xs[0]:
            return _value(vs[0])
        if x >= xs[-1]:
            return _value(vs[-1])
        i = bisect.bisect_right(xs, x) - 1
        span = xs[i + 1] - xs[i]
        return _value(vs[i] + (vs[i + 1] - vs[i]) * ((x - xs[i]) / span if span > 0 else 1.0))

    return fn


def ramp(owner, path, points):
    """Scrubbed: straight ramps between [(scroll position, value)], flat outside."""
    fn = linear(points)
    TL.add(owner, path, EVER, lambda s, dt: fn(s), "real")


def switch(owner, path, points):
    """Scrubbed: hard steps. Each value of [(scroll position, value)] holds until the next."""
    xs = [p[0] for p in points]
    vs = [p[1] for p in points]
    TL.add(owner, path, EVER, lambda s, dt: vs[max(bisect.bisect_right(xs, s + 1e-9) - 1, 0)], "real")


def _shape(keys):
    xs, ys = [k[0] for k in keys], [k[1] for k in keys]

    def fn(u):
        if u <= xs[0]:
            return ys[0]
        if u >= xs[-1]:
            return ys[-1]
        i = bisect.bisect_right(xs, u) - 1
        return lerp(ys[i], ys[i + 1], (u - xs[i]) / (xs[i + 1] - xs[i]))

    return fn


_POP = _shape(((0.0, 0.0), (0.55, 1.14), (0.8, 0.97), (1.0, 1.0)))
_UNPOP = _shape(((0.0, 1.0), (0.3, 1.1), (1.0, 0.0)))


def pop(obj, start, dur=0.28, base=None, clock="scroll"):
    """Triggered: scale in from nothing with a little overshoot. Hidden until then."""
    base = Vector(base if base is not None else obj.scale)
    TL.add(obj, "scale", start, lambda s, dt: tuple(base * _POP(dt / dur)), clock)
    return base


def unpop(obj, start, dur=0.2, base=None, clock="scroll"):
    """Triggered: a small swell, then gone."""
    base = Vector(base if base is not None else obj.scale)
    TL.add(obj, "scale", start, lambda s, dt: tuple(base * _UNPOP(dt / dur)), clock)
    return base


def show(obj, s_in, s_out=None, base=None):
    """Scrubbed: there between two scroll positions, gone outside them."""
    base, zero = tuple(base if base is not None else obj.scale), (0.0, 0.0, 0.0)
    points = [(s_in - 1.0, zero), (s_in, base)] + ([(s_out, zero)] if s_out is not None else [])
    switch(obj, "scale", points)


def light_ramp(obj, points):
    """Scrubbed light: energy ramps between [(scroll position, energy)], and the lamp is
    hidden from the render wherever it is dark at both ends of a stretch."""
    ramp(obj.data, "energy", points)
    lit = [s for s, energy in points if energy > 0]
    first = max((s for s, _ in points if s < lit[0]), default=lit[0] - 1.0)
    last = min((s for s, _ in points if s > lit[-1]), default=None)
    hidden = [(first - 1.0, True), (first, False)] + ([(last, True)] if last is not None else [])
    switch(obj, "hide_render", hidden)
    obj.hide_viewport = False


def light_window(obj, s_in, s_out=None, energy=None, fade_in=0.12, fade_out=0.2):
    """Triggered light: it comes on when the scroll gets to s_in and goes off at s_out,
    each over a moment of real time. Hidden from the render while off."""
    data = obj.data
    energy = data.energy if energy is None else energy
    TL.add(data, "energy", s_in, lambda s, dt: energy * (clamp01(dt / fade_in) if fade_in > 0 else 1.0))
    TL.add(obj, "hide_render", EVER, lambda s, dt: True, "real")
    TL.add(obj, "hide_render", s_in, lambda s, dt: False)
    if s_out is not None:
        TL.add(data, "energy", s_out, lambda s, dt: energy * (1.0 - clamp01(dt / fade_out) if fade_out > 0 else 0.0))
        TL.add(obj, "hide_render", s_out, lambda s, dt: dt > fade_out)
    obj.hide_viewport = False


# -- camera ------------------------------------------------------------------
def look_quat(eye, target, up=(0.0, 0.0, 1.0), roll=0.0):
    """Orientation looking from `eye` to `target`. `up` is a hint, so it may be any
    direction not parallel to the view; `roll` (degrees) turns about the view axis."""
    forward = (Vector(target) - Vector(eye)).normalized()
    right = forward.cross(Vector(up))
    if right.length < 1e-6:
        right = forward.cross(Vector((0.0, 1.0, 0.0)))
    right.normalize()
    true_up = right.cross(forward)
    quat = Matrix((right, true_up, -forward)).transposed().to_quaternion()
    if roll:
        quat = quat @ Quaternion((0.0, 0.0, 1.0), math.radians(roll))
    return quat


class Shot:
    """One continuous stretch of camera, scrubbed. Each property has its own keys
    [(scroll position, value)]: where the camera is, what it looks at, an up hint,
    a roll in degrees, the lens."""

    def __init__(self, eye, target, up=None, roll=None, lens=None, ends="rest"):
        self.s0, self.s1 = min(k[0] for k in eye), max(k[0] for k in eye)
        self.tracks = {
            "eye": Track(eye, ends),
            "target": Track(target, ends),
            "up": Track(up or [(self.s0, (0.0, 0.0, 1.0))]),
            "roll": Track(roll or [(self.s0, 0.0)]),
            "lens": Track(lens or [(self.s0, 24.0)]),
        }

    def __call__(self, s):
        return {name: track(s) for name, track in self.tracks.items()}


def _shot_at(shots, s):
    return [shot for shot in shots if shot.s0 <= s + 1e-9][-1]


def bake_camera(cam, shots, session):
    """Shots are played back to back; the jump between two shots must be hidden
    behind a full-frame cover."""
    cam.rotation_mode = "QUATERNION"
    shots = sorted(shots, key=lambda shot: shot.s0)
    where, turn, lens = [], [], []
    prev = None
    for f in range(session.frames):
        state = _shot_at(shots, session.scroll(f / FPS))(session.scroll(f / FPS))
        quat = look_quat(state["eye"], state["target"], state["up"], state["roll"])
        if prev is not None and prev.dot(quat) < 0:
            quat.negate()
        prev = quat.copy()
        where.append(tuple(state["eye"]))
        turn.append(tuple(quat))
        lens.append(state["lens"])
    write(cam, "location", where)
    write(cam, "rotation_quaternion", turn)
    write(cam.data, "lens", lens)
    return lens


def survey(shots, end, step=0.01):
    """How much the camera moves per screen of scroll, sampled along the journey:
    [(scroll position, blocks travelled per screen, degrees turned per screen)]."""
    shots = sorted(shots, key=lambda shot: shot.s0)
    out, s = [], 0.0
    while s + step <= end + 1e-9:
        a, b = _shot_at(shots, s), _shot_at(shots, s + step)
        if a is b:
            p, q = a(s), a(s + step)
            turned = look_quat(p["eye"], p["target"], p["up"], p["roll"]).rotation_difference(look_quat(q["eye"], q["target"], q["up"], q["roll"])).angle
            out.append((round(s, 3), (Vector(q["eye"]) - Vector(p["eye"])).length / step, math.degrees(turned) / step))
        s += step
    return out


# -- the pumpkin ---------------------------------------------------------------
def pose(pos, yaw=0.0, tilt=(0.0, 0.0, 0.0), squash=1.0, calm=False, grow=1.0):
    """squash < 1 flattens (and widens to keep the volume), > 1 stretches; grow is its
    size all round (0 is gone). A calm pose is one the pumpkin rests in: it breathes,
    blinks and can be made to jump."""
    return {"pos": Vector(pos), "yaw": yaw, "tilt": tilt, "squash": squash, "calm": calm, "grow": grow}


class Actor:
    """A character whose journey is a list of scrubbed moves, each a function of its own
    progress u in 0..1; the move that started last is the one playing. While a move
    leaves it calm, real time adds life on top: breathing, blinks, a jump when clicked."""

    BREATH = (0.03, 2.6)  # how much it swells, and how long a breath takes
    JUMP = 0.55  # seconds a click-jump lasts

    def __init__(self, root, spin, bodies, size=1.0, extras=()):
        """`bodies` maps a face name to its object (one shows at a time); `extras` are
        parts that show whenever the actor does."""
        self.root, self.spin, self.bodies, self.size, self.extras = root, spin, bodies, size, tuple(extras)
        self.moves, self.faces, self.visible, self.jumps, self.masks, self.glances, self.acts = [], [], [], [], [], [], []

    # scrubbed --------------------------------------------------------------
    def move(self, s0, s1, fn):
        self.moves.append((s0, s1, fn))
        return self

    def rest(self, s0, pos, yaw=0.0):
        """Calm from s0 until the next move starts."""
        self.moves.append((s0, s0 + 1.0, lambda u: pose(pos, yaw, calm=True)))
        return self

    def hop(self, s0, s1, p0, p1, height, yaw=(0.0, 0.0), flips=0.0, axis=0, antic=0.14, land=0.2):
        """Crouch, jump along a parabola from p0 to p1, land with a squash. `antic` and
        `land` are the shares of the move spent crouching and landing."""
        p0, p1 = Vector(p0), Vector(p1)
        a, b = antic, land

        def fn(u):
            y = lerp(yaw[0], yaw[1], smooth(u))
            if a > 0 and u < a:
                return pose(p0, y, squash=1.0 - 0.3 * math.sin(math.pi * 0.5 * u / a))
            if b > 0 and u > 1 - b:
                v = (u - (1 - b)) / b
                return pose(p1, y, squash=1.0 - 0.42 * math.cos(math.pi * 0.5 * v) ** 2 * (1 - v) + 0.08 * math.sin(math.pi * v))
            v = (u - a) / (1 - a - b)
            pos = p0.lerp(p1, v) + Vector((0, 0, 4 * height * v * (1 - v)))
            tilt = [0.0, 0.0, 0.0]
            tilt[axis] = 360.0 * flips * smooth(v)
            return pose(pos, y, tilt=tuple(tilt), squash=1.0 + 0.2 * abs(math.cos(math.pi * v)))

        self.moves.append((s0, s1, fn))
        return self

    def face(self, s, name):
        self.faces.append((s, name))
        return self

    def appear(self, s_in, s_out=None):
        self.visible.append((s_in, s_out))
        return self

    # live ------------------------------------------------------------------
    def jump(self, tau, height=0.75):
        """It jumps on the spot at this moment (somebody clicked it)."""
        if tau is not None:
            self.jumps.append((tau, height))
        return self

    def act(self, tau0, tau1, fn):
        """Do something of its own for a while of real time, wherever the scroll has it
        resting: `fn(u)` gives the pose for progress u in 0..1."""
        if tau0 is not None:
            self.acts.append((tau0, tau1, fn))
        return self

    def glance(self, tau0, tau1, fn):
        """Turn toward something for a while of real time: `fn(seconds)` gives the turn in
        degrees on top of where it faces."""
        if tau0 is not None:
            self.glances.append((tau0, tau1, fn))
        return self

    def mask(self, tau0, tau1, name):
        """Wear another face for a while of real time (asleep, say), whatever the scroll says."""
        if tau0 is not None:
            self.masks.append((tau0, tau1, name))
        return self

    # reading ---------------------------------------------------------------
    def state(self, s):
        current = self.moves[0]  # before the first move starts, wait at its beginning
        for move in self.moves:
            if s >= move[0] - 1e-9:
                current = move
        s0, s1, fn = current
        return fn(clamp01((s - s0) / (s1 - s0)))

    def position(self, s):
        return self.state(s)["pos"]

    def _blinks(self, duration):
        """Moments it blinks: every few seconds, never quite regular."""
        out, tau, i = [], 1.7, 0
        while tau < duration:
            out.append(tau)
            i += 1
            tau += 2.3 + 2.4 * ((i * 0.618034) % 1.0)
        return out

    def bake(self, session):
        self.moves.sort(key=lambda m: m[0])
        faces = sorted(self.faces) or [(0.0, next(iter(self.bodies)))]
        s_in, s_out = self.visible[0] if self.visible else (EVER, None)
        blinks = self._blinks(session.duration) if "blink" in self.bodies else []
        where, turn, size, tumble = [], [], [], []
        shown = {name: [] for name in self.bodies}
        there = []
        for f in range(session.frames):
            tau = f / FPS
            s = session.scroll(tau)
            p = self.state(s)
            for tau0, tau1, fn in self.acts:
                if p["calm"] and tau0 <= tau < tau1:
                    p = fn((tau - tau0) / (tau1 - tau0))
            pos, squash = Vector(p["pos"]), p["squash"]
            face = ([name for at, name in faces if at <= s + 1e-9] or [faces[0][1]])[-1]
            for tau0, tau1, name in self.masks:
                if tau0 <= tau and (tau1 is None or tau < tau1):
                    face = name
            if p["calm"]:
                squash *= 1.0 + self.BREATH[0] * math.sin(2 * math.pi * tau / self.BREATH[1])
                if face == "awake" and any(0.0 <= tau - at < 0.13 for at in blinks):
                    face = "blink"
                for at, height in self.jumps:
                    u = (tau - at) / self.JUMP
                    if 0.0 <= u < 1.0:
                        if u < 0.16:
                            squash *= 1.0 - 0.26 * math.sin(math.pi * 0.5 * u / 0.16)
                        elif u < 0.8:
                            v = (u - 0.16) / 0.64
                            pos.z += 4 * height * v * (1 - v)
                            squash *= 1.0 + 0.16 * abs(math.cos(math.pi * v))
                            face = "whee" if "whee" in self.bodies else face
                        else:
                            v = (u - 0.8) / 0.2
                            squash *= 1.0 - 0.3 * (1 - v) * math.cos(math.pi * 0.5 * v)
            turned = p["yaw"]
            for tau0, tau1, fn in self.glances:
                if tau0 <= tau < tau1 + 0.3:
                    turned += fn(tau) * smooth((tau - tau0) / 0.25) * (1.0 - smooth((tau - tau1) / 0.3))
            wide = 1.0 / math.sqrt(max(squash, 1e-3))
            here = s >= s_in - 1e-9 and (s_out is None or s < s_out)
            where.append(tuple(pos))
            turn.append((0.0, 0.0, math.radians(turned)))
            size.append((self.size * wide * p["grow"], self.size * wide * p["grow"], self.size * squash * p["grow"]))
            tumble.append(tuple(math.radians(a) for a in p["tilt"]))
            for name in self.bodies:
                shown[name].append((1.0, 1.0, 1.0) if here and name == face else (0.0, 0.0, 0.0))
            there.append((1.0, 1.0, 1.0) if here else (0.0, 0.0, 0.0))
        write(self.root, "location", where)
        write(self.root, "rotation_euler", turn)
        write(self.root, "scale", size)
        write(self.spin, "rotation_euler", tumble)
        for name, body in self.bodies.items():
            write(body, "scale", shown[name])
        for extra in self.extras:
            write(extra, "scale", there)
