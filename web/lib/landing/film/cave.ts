/**
 * The cave the cut face opens. It has two states, asleep and awake, and one sequence
 * from each to the other. Two things play those sequences: the pumpkin arriving
 * (scrubbed by the scroll), and a hand on the lever (in real time).
 *
 * Asleep: dark and blue, torches out, nobody there, the world kept as one small
 * glowing block. Awake: torches lit, friends in, the block unfolded into the world.
 */

import { Vector3, type Mesh, type Object3D, type ShaderMaterial } from "three";
import { setPose, type MoveFn } from "../anim/actor";
import { POP, UNPOP, clamp01, cycle, flicker, lerp, linear, smooth } from "../anim/easing";
import { fade, grow } from "../anim/timeline";
import { GREY, YELLOW, type Card } from "../engine/hud";
import { vec, type Ctx } from "./context";

/** One step of a sequence: put a thing where `dt` seconds into the sequence have it. */
type Step = (dt: number, time: number) => void;

interface Pull {
  what: "wake" | "sleep";
  at: number;
}

export interface Cave {
  /** A hand on the lever: the cave goes to sleep, or wakes. False if it was not ready for one. */
  pull(time: number): boolean;
  /** Whether it is awake, or on its way there. */
  awake(): boolean;
  /** What the pumpkin makes of it: whether it sleeps, and how much of its candle burns. */
  readonly sleeper: { asleep: boolean; candle: number };
}

const RAD = Math.PI / 180;
/** The lights of the cave exist only while it can be seen. */
const NEAR: [number, number] = [2.3, 4.2];
/** Seconds a visitor rests at the lever without touching it before the pumpkin shows how, and how long it dozes. */
const PATIENCE = 7;
const DOZE = 3.8;

export function setCave(ctx: Ctx): Cave {
  const { tl, C, stage, points } = ctx;
  const { lamps } = stage;
  const folded = points.cave_folded; // how long into going to sleep the block is back
  const wakes: Step[] = [];
  const sleeps: Step[] = [];
  const rewakes: Step[] = []; // what waking does only after a sleep: putting away what sleep brought
  const hidden: (() => void)[] = []; // nothing to see until a sequence shows it
  const before: ((s: number) => void)[] = []; // what the scroll does before the arrival begins
  const lights: string[] = [];

  /** A light that burns while the cave is awake. */
  const lit = (name: string, onAt: number, offAt: number, rise = 0.1, fall = 0.15, flame: number | null = null) => {
    const energy = lamps.base(name);
    lights.push(name);
    const waver = (time: number) => (flame === null ? 1 : flicker(time, flame));
    wakes.push((dt, time) => lamps.set(name, energy * clamp01((dt - onAt) / rise) * waver(time)));
    sleeps.push((dt, time) => lamps.set(name, energy * (1 - clamp01((dt - offAt) / fall)) * waver(time)));
  };
  /** A thing that is there while the cave is awake. */
  const switched = (thing: Object3D, onAt: number, offAt: number) => {
    wakes.push((dt) => void (thing.visible = dt >= onAt));
    sleeps.push((dt) => void (thing.visible = dt < offAt));
  };

  // Torches and lanterns: they fire outward from the block, and die from the far end back.
  const flames = [...C.TORCHES.map((at) => ({ kind: "Torch", at })), ...C.LANTERNS.map((at) => ({ kind: "Lantern", at }))];
  const far = (i: number) => Math.hypot(flames[i].at[0] - C.SNAPSHOT[0], flames[i].at[1] - C.SNAPSHOT[1]);
  const order = flames.map((_, i) => i).sort((a, b) => far(a) - far(b));
  flames.forEach(({ kind }, i) => {
    const rank = order.indexOf(i);
    const onAt = 0.1 + 0.07 * rank;
    const offAt = 0.45 + 0.09 * (flames.length - 1 - rank);
    const dead = ctx.node(`${kind}_${i}_out`);
    switched(ctx.node(`${kind}_${i}_lit`), onAt, offAt);
    wakes.push((dt) => void (dead.visible = dt < onAt));
    sleeps.push((dt) => void (dead.visible = dt >= offAt));
    lit(`${kind}Light_${i}`, onAt, offAt, 0.1, 0.15, 1.7 * i);
  });
  for (const name of ["Furnace_a", "Furnace_b", "Berries_a", "Berries_b", "Berries_c", "Diamonds"]) lit(`Cave_${name}`, 0.3, 0.3, 0.5, 0.8);
  lit("Cave_CastKey", 0.3, 0.2, 0.6, 0.8);
  lit("Cave_Front", 0.1, 0.3, 0.6, 1.0);
  // Moonlight through the open front while it sleeps, so the sleeper and the block can be seen.
  const arriving = linear([[2.45, 0], [2.7, 150]]);
  lights.push("Cave_Moon");
  before.push((s) => lamps.set("Cave_Moon", arriving(s)));
  wakes.push((dt) => lamps.set("Cave_Moon", 150 * (1 - clamp01(dt / 0.5))));
  sleeps.push((dt) => lamps.set("Cave_Moon", 150 * clamp01((dt - 0.6) / 1.0)));

  // The two friends: they join once the lights are on, and leave before they go out.
  const steve = ctx.node("Cave_Steve");
  const alex = ctx.node("Cave_Alex");
  const friends: [Object3D, Object3D, number, number][] = [[steve, ctx.node("Cave_Tag_JAVA"), 1.0, 0.25], [alex, ctx.node("Cave_Tag_BEDROCK"), 1.35, 0.55]];
  for (const [rig, tag, joins, leaves] of friends) {
    for (const [thing, late] of [[rig, 0], [tag, 0.12]] as const) {
      wakes.push((dt) => grow(thing, POP((dt - joins - late) / 0.32)));
      sleeps.push((dt) => grow(thing, UNPOP((dt - leaves) / 0.22)));
    }
  }
  // He mines the diamonds, for ever; she cannot keep still.
  const swing = cycle(1.5, [[0, [-150, 0, 0]], [0.28, [-58, 0, 0]], [0.5, [-70, 0, 0]], [1.2, [-150, 0, 0]]]);
  const bow = cycle(1.5, [[0, [12, 0, 0]], [0.28, [22, 0, 0]], [1.2, [12, 0, 0]]]);
  const arm = ctx.node("Cave_Steve_arm_r");
  const body = ctx.node("Cave_Steve_body");
  const alexHome = alex.position.clone();
  // Now and then a diamond flies from his pick into the chest behind him.
  const gemAt = ctx.node("Cave_Diamond_At");
  const gem = ctx.node("Cave_Diamond");
  switched(gemAt, 1.4, 0.25);
  const struck = steve.position.clone().add(new Vector3(-1.2, -0.25, 1.05)); // from the rock in front of him, over his head
  const kept = ctx.node("Cave_Chest_base").position.clone().add(new Vector3(0, 0, 0.7));

  // One headline for each state, in the same place.
  const awakeLine = ctx.node("Cave_Awake");
  const asleepLine = ctx.node("Cave_Asleep");
  hidden.push(() => grow(awakeLine, 0), () => grow(asleepLine, 0));
  wakes.push((dt) => grow(awakeLine, POP((dt - 1.6) / 0.28)));
  sleeps.push((dt) => grow(awakeLine, UNPOP((dt - 0.5) / 0.2)));
  sleeps.push((dt) => grow(asleepLine, POP((dt - 1.7) / 0.28)));
  rewakes.push((dt) => grow(asleepLine, UNPOP(dt / 0.2)));

  // The snapshot: the world as one small block that keeps its own light. Waking, it
  // swells and a fine frame races outward from it: the world unfolding. Going to
  // sleep, the world folds back into it with a flash.
  const snap = ctx.node("Snapshot");
  const wave = ctx.node("Snapshot_Wave");
  const waveLook = ((wave as Mesh).material as ShaderMaterial | undefined)?.uniforms?.uColor?.value as number[] | undefined;
  const burst = (peak: number, at: number, dt: number) => peak * Math.max(1 - Math.abs(dt - at - 0.12) / (dt - at < 0.12 ? 0.12 : 0.36), 0);
  before.push(() => grow(wave, 0));
  wakes.push((dt) => grow(snap, dt < 0.16 ? lerp(1, 1.9, smooth(dt / 0.16)) : 0));
  wakes.push((dt) => {
    grow(wave, dt < 0.5 ? 1 + 13 * (dt / 0.5) ** 1.5 : 0);
    if (waveLook) waveLook[3] = Math.max(1 - dt / 0.5, 0) ** 1.5;
  });
  sleeps.push((dt) => grow(snap, POP((dt - folded) / 0.3)));
  lights.push("Cave_Unfold");
  wakes.push((dt) => lamps.set("Cave_Unfold", burst(1000, 0, dt)));
  sleeps.push((dt) => lamps.set("Cave_Unfold", burst(420, folded, dt)));
  for (const name of ["Cave_SnapLight", "Cave_SnapFloor"]) {
    const energy = lamps.base(name);
    const coming = linear([[2.3, 0], [2.45, energy]]);
    lights.push(name);
    before.push((s) => lamps.set(name, coming(s)));
    wakes.push((dt) => lamps.set(name, energy * (1 - clamp01(dt / 0.1))));
    sleeps.push((dt) => lamps.set(name, energy * clamp01((dt - folded) / 0.3)));
  }

  // The sleeper's Zs, which rise beside the pumpkin while the cave is asleep. (Where it
  // works the lever itself, it dozes off right beside it.)
  const home = vec(points.cast.home);
  const side = vec(points.lever_side); // in front of the lever, clear of the block
  const bed = home.clone();
  [0.024, 0.032, 0.042].forEach((cell, i) => {
    const z = ctx.node(`Cave_Zzz_${i}`);
    const rise = (time: number, share: number) => {
      const u = (time / 2.7 + i / 3) % 1;
      const size = (lerp(0.022, 0.046, u) / cell) * smooth(u / 0.12) * smooth((1 - u) / 0.18) * share;
      z.visible = size > 1e-4;
      if (!z.visible) return;
      z.scale.setScalar(size);
      z.position.set(bed.x - 0.5 - u, bed.y - 0.2, bed.z + 1.1 + u + 0.07 * Math.sin(u * 9));
    };
    hidden.push(() => void (z.visible = false));
    sleeps.push((dt, time) => rise(time, clamp01((dt - 1.5) / 0.3)));
    rewakes.push((dt, time) => rise(time, 1 - clamp01(dt / 0.15)));
  });

  // The lever: on a lamp in the right foreground, far bigger than life.
  const lampOn = ctx.node("Lever_Lamp_on");
  const lampOff = ctx.node("Lever_Lamp_off");
  const handle = ctx.node("Lever_handle");
  wakes.push((dt) => {
    lampOn.visible = dt > 0;
    lampOff.visible = !lampOn.visible;
    handle.rotation.set(0, lerp(-40, 40, smooth(dt / 0.12)) * RAD, 0);
  });
  sleeps.push((dt) => {
    lampOff.visible = dt > 0;
    lampOn.visible = !lampOff.visible;
    handle.rotation.set(0, lerp(40, -40, smooth(dt / 0.12)) * RAD, 0);
  });
  lit("Lever_Glow", 0, 0, 0.3, 0.2);
  // A frame that breathes round the lever: this is the thing to touch here.
  const hint = ctx.node("Lever_Hint");

  // Who plays the sequences. Arriving wakes it, by the scroll; after that, whoever pulls
  // the lever. Only the last two matter: the one before gives what the last leaves alone.
  let pulls: Pull[] = [];
  const sleeper = { asleep: false, candle: 1 };
  const flip = (time: number) => {
    const last = pulls[pulls.length - 1];
    const what = !last || last.what === "wake" ? "sleep" : "wake";
    pulls.push({ what, at: time });
    if (pulls.length > 2) pulls.shift();
    // Its comings and goings, in chat.
    const said: [number, string, typeof YELLOW][] = what === "wake" ? [[1.0, "Steve joined the game", YELLOW], [1.35, "Alex joined the game", YELLOW]] : [[0.25, "Steve left the game", YELLOW], [0.55, "Alex left the game", YELLOW], [folded + 0.15, "World saved.", GREY]];
    for (const [delay, text, color] of said) ctx.ui?.say(text, color, time + delay, { stay: 6, from: NEAR[0], until: 3.62 });
  };

  // Where nobody touches the lever, the pumpkin goes over and works it itself: a leap to
  // knock it off, a doze, a start, a leap to knock it back on, and home.
  const { pumpkin } = ctx.cast;
  let idle = 0;
  let shown = false;
  let demo: { off: number; on: number; done: number } | null = null;
  /** Straight up beside the lever, turning over once, to hit the handle at the top. */
  const knock: MoveFn = (u, out) => {
    if (u < 0.14) {
      setPose(out, side, 10, 1 - 0.3 * Math.sin((Math.PI * 0.5 * u) / 0.14));
    } else if (u > 0.88) {
      const v = (u - 0.88) / 0.12;
      setPose(out, side, 10, 1 - 0.36 * (1 - v) * Math.cos(Math.PI * 0.5 * v));
    } else {
      const v = (u - 0.14) / 0.74;
      setPose(out, side, 10, 1 + 0.16 * Math.abs(Math.cos(Math.PI * v)));
      out.pos.x += 0.3 * Math.sin(Math.PI * v);
      out.pos.z += 4 * 1.5 * v * (1 - v);
      out.tilt[1] = 360 * smooth(v);
    }
  };
  const between = (from: Vector3, to: Vector3): MoveFn => (u, out) => {
    setPose(out, from, 10, 1 + 0.12 * Math.abs(Math.cos(Math.PI * u)));
    out.pos.lerp(to, smooth(u));
    out.pos.z += 4 * 1.1 * u * (1 - u);
  };
  const show = (time: number) => {
    const off = time + 1.35;
    const on = off + DOZE;
    demo = { off, on, done: 0 };
    shown = true;
    pumpkin.act(off - 1.35, off - 0.5, between(home, side));
    pumpkin.act(off - 0.5, off + 0.5, knock); // at the top of the leap, half way, the lever goes over
    pumpkin.act(off + 0.5, on - 0.5, (_u, out) => setPose(out, side, 10, 1, true));
    pumpkin.act(on - 0.5, on + 0.5, knock);
    pumpkin.act(on + 0.5, on + 1.3, between(side, home));
  };
  const run = (steps: readonly Step[], dt: number, time: number) => {
    for (const step of steps) step(dt, time);
  };
  const play = (pull: Pull, time: number) => {
    run(pull.what === "wake" ? wakes : sleeps, time - pull.at, time);
    if (pull.what === "wake") run(rewakes, time - pull.at, time);
  };
  const out: number[] = [];

  tl.during(NEAR[0] - 0.1, NEAR[1] + 0.1, ({ s, time, dt }) => {
    const near = s >= NEAR[0] && s < NEAR[1];
    if (!near && (pulls.length || shown)) {
      // Out of sight it is as the scroll has it again.
      pulls = [];
      demo = null;
      shown = false;
      pumpkin.settle();
    }
    // The pumpkin's own turn at the lever, if the visitor has rested here and left it alone.
    idle = s > 3.3 && s < 3.52 && !pulls.length && !shown && ctx.touch.hovered?.name !== "lever" ? idle + dt : 0;
    if (idle > PATIENCE) show(time);
    if (demo) {
      if (demo.done === 0 && time >= demo.off) flip(time);
      if (demo.done === 1 && time >= demo.on) flip(time);
      demo.done = time >= demo.on ? 2 : time >= demo.off ? 1 : 0;
      if (time > demo.on + 1.4) demo = null;
    }
    bed.copy(demo ? side : home);
    for (const hide of hidden) hide();
    const arrived = Math.max((s - C.WAKE) * C.ARRIVAL_RATE, 0); // arriving, the scroll plays it: done before the stop
    const last = pulls[pulls.length - 1];
    if (!last) {
      run(wakes, arrived, time);
      if (s < C.WAKE) for (const set of before) set(s);
    } else {
      if (pulls.length > 1) play(pulls[0], time);
      else run(wakes, arrived, time);
      play(last, time);
    }
    if (!near) for (const name of lights) lamps.set(name, 0);

    // While the cave sleeps, so does the pumpkin, and its candle goes out.
    const since = last ? time - last.at : 0;
    sleeper.asleep = last ? (last.what === "sleep" ? since >= 1.2 : since < 0.15) : false;
    if (demo) sleeper.asleep = demo.done === 1 && since >= 1.2 && time < demo.on - 0.6; // it starts up before it knocks the lever back
    ctx.cast.pumpkin.mask = sleeper.asleep ? "asleep" : null;
    sleeper.candle = last ? (last.what === "sleep" ? 1 - clamp01((since - 1) / 0.4) : clamp01(since / 0.3)) : 1;

    fade(hint, s >= 3.12 && s < 3.6 ? 0.18 + 0.4 * (0.5 + 0.5 * Math.sin((time * 2 * Math.PI) / 1.5)) : 0);
    if (!near) return;
    swing(time, out);
    arm.rotation.set(out[0] * RAD, out[1] * RAD, out[2] * RAD, "ZYX");
    bow(time, out);
    body.rotation.set(out[0] * RAD, out[1] * RAD, out[2] * RAD, "ZYX");
    alex.position.z = alexHome.z + 0.42 * Math.abs(Math.sin((time * Math.PI) / 0.62));
    const flight = clamp01(((time % 4.5) - 0.3) / 0.75); // one in three swings, from the moment the pick lands
    const size = 0.36 * smooth(flight / 0.08) * (1 - smooth((flight - 0.82) / 0.18));
    gem.visible = size > 1e-3;
    if (gem.visible) {
      gem.scale.setScalar(size);
      gem.position.lerpVectors(struck, kept, flight);
      gem.position.z += 4 * 1.5 * flight * (1 - flight);
      gem.rotation.set(0, 0, time * 7);
    }
  });

  // Small things to find while it is awake: a torch that can be snuffed (it relights), the
  // chest (its lid lifts), and the diamonds Steve is after.
  const snuffed = flames.map(() => -Infinity);
  flames.forEach(({ kind }, i) => {
    ctx.touch.add({
      name: `${kind}_${i}`,
      at: ctx.node(`${kind}_${i}_lit`).position.clone().add(new Vector3(0, 0, kind === "Torch" ? 0.45 : 0.3)),
      half: new Vector3(0.3, 0.3, 0.5),
      seen: [3.12, 3.6],
      click: (time) => void (snuffed[i] = time),
    });
  });
  const lid = ctx.node("Cave_Chest_lid");
  const lidRest = lid.rotation.x;
  let lifted = 0;
  let peeked = false;
  const finds: Record<string, Card | null> = { chest: null, diamonds: null };
  ctx.touch.add({
    name: "cave_chest",
    at: ctx.node("Cave_Chest_base").position.clone().add(new Vector3(0, 0, 0.5)),
    half: new Vector3(0.6, 0.6, 0.6),
    seen: [3.12, 3.6],
    pick: true,
    enter: () => void (peeked = true),
    leave: () => void (peeked = false),
    tip: () => (finds.chest ??= ctx.ui?.draw.tooltip("Chest", ["Steve's diamonds. So far: three."]) ?? null),
  });
  ctx.touch.add({
    name: "cave_diamonds",
    at: new Vector3(-7.0, 3.0, -23.4),
    half: new Vector3(1.4, 1.2, 1.6),
    seen: [3.12, 3.6],
    pick: true,
    tip: () => (finds.diamonds ??= ctx.ui?.draw.tooltip("Diamonds", ["Day one. He will not stop talking about it."]) ?? null),
  });
  tl.during(NEAR[0], NEAR[1], ({ time, dt }) => {
    // (After the sequences have had their say: a snuffed flame stays out for a moment.)
    flames.forEach(({ kind }, i) => {
      const out = time - snuffed[i];
      if (out < 0 || out > 2.6) return;
      const burning = ctx.node(`${kind}_${i}_lit`);
      if (!burning.visible && out > 0.05) return; // the cave is dark anyway
      const back = clamp01((out - 2.2) / 0.4);
      burning.visible = back > 0;
      ctx.node(`${kind}_${i}_out`).visible = back <= 0;
      const lamp = lamps.byName.get(`${kind}Light_${i}`);
      if (lamp) lamp.energy *= back;
    });
    lifted = peeked ? Math.min(lifted + dt / 0.18, 1) : Math.max(lifted - dt / 0.3, 0);
    lid.rotation.x = lidRest - 0.6 * smooth(lifted);
  });

  // A hand on the lever. Its tooltip says what pulling it does now.
  const cave: Cave = {
    pull(time) {
      const last = pulls[pulls.length - 1];
      if (tl.frame.s < C.WAKE + 0.2 || tl.frame.s >= NEAR[1] || (last && time - last.at < 2.2)) return false;
      if (demo) {
        // The visitor takes over: the pumpkin leaves the lever to them.
        demo = null;
        pumpkin.settle();
      }
      shown = true;
      flip(time);
      return true;
    },
    awake: () => (pulls.length ? pulls[pulls.length - 1].what === "wake" : true),
    sleeper,
  };
  const says: Record<string, Card | null> = { awake: null, asleep: null };
  ctx.touch.add({
    name: "lever",
    at: hint.position.clone(),
    half: new Vector3(0.6, 0.6, 0.85),
    seen: [3.12, 3.6],
    hang: { offset: ctx.pick([-16, 26], [24, 34]), anchor: [1, 0] }, // between the headline and the block
    tip: () => {
      if (!ctx.ui) return null;
      says.awake ??= ctx.ui.draw.tooltip("Lever", ["Pull it: everyone leaves,", "and the server sleeps."]);
      says.asleep ??= ctx.ui.draw.tooltip("Lever", ["Pull it: someone joins,", "and the server wakes."]);
      return cave.awake() ? says.awake : says.asleep;
    },
    click: (time) => void cave.pull(time),
  });
  return cave;
}
