/**
 * Life: what moves by itself, in real time, whatever the scroll does. Wind, water and
 * drifting flakes live in the shaders; here are the creatures and the loops.
 */

import { Euler, Quaternion, Vector3, type Object3D } from "three";
import { clamp01, cycle, smooth, wrap } from "../anim/easing";
import { fade } from "../anim/timeline";
import { vec, type Ctx } from "./context";

const RAD = Math.PI / 180;
const DEG = 180 / Math.PI;
const UP = new Vector3(0, 1, 0);

/** The swell of the water at a place, as the water's own shader has it. */
export function swell(x: number, y: number, time: number, height: number): number {
  return height * (0.6 * Math.sin(0.9 * x + 0.6 * y - time * 1.6) + 0.4 * Math.sin(-0.5 * x + 1.1 * y - time * 2.3 + 1.0));
}

function arc(a: Vector3, b: Vector3, v: number, height: number, out: Vector3): Vector3 {
  out.lerpVectors(a, b, v);
  out.z += 4 * height * v * (1 - v);
  return out;
}

/** A thing's turn as the art writes it: degrees about X, Y and Z, applied in that order. */
function turnOf(thing: Object3D): number[] {
  const euler = new Euler().setFromQuaternion(thing.quaternion, "ZYX");
  return [euler.x * DEG, euler.y * DEG, euler.z * DEG];
}

function turn(thing: Object3D, degrees: readonly number[]) {
  thing.rotation.set(degrees[0] * RAD, degrees[1] * RAD, degrees[2] * RAD, "ZYX");
}

/** Rings spread on the water from `at` after each of `events` [moment in the loop, which ring]. */
function ripple(rings: readonly Object3D[], events: readonly (readonly [number, number])[], u: number, at: Vector3, level: number, time: number, height: number) {
  for (const ring of rings) fade(ring, 0);
  for (const [moment, k] of events) {
    const age = u - moment;
    if (age < 0 || age >= 0.9) continue;
    const grow = 0.3 + (1.5 * age) / 0.9;
    const ring = rings[k];
    ring.position.set(at.x, at.y, level + 0.03 + swell(at.x, at.y, time, height));
    ring.scale.set(grow, grow, 1);
    fade(ring, 0.75 * (1 - age / 0.9));
  }
}

/**
 * A friend fishing, for ever: wind up, cast, wait, a bite, the strike, and a fish that
 * goes into the barrel (or back into the pond). Only the arm and the body are looped by
 * hand; the bobber, the line, the fish and the ripples follow from where the rod's tip is.
 */
function fisher(ctx: Ctx, data: Ctx["points"]["fishing"][number]): (time: number) => void {
  const { C } = ctx;
  const PERIOD = 9.6;
  const [WIND, CAST, LANDS, BITE, STRIKE, CAUGHT, AWAY] = [0.45, 0.72, 1.35, 6.0, 6.6, 7.3, 7.95]; // moments in the loop
  const { tag, phase } = data;
  const arm = ctx.node(`${tag}_arm_r`);
  const body = ctx.node(`${tag}_body`);
  const rod = ctx.node(`${tag}_Rod`);
  const bobber = ctx.node(`${tag}_Bobber`);
  const line = ctx.node(`${tag}_Line`);
  const fish = ctx.node(`${tag}_Fish`);
  const rings = [0, 1, 2].map((i) => ctx.node(`${tag}_Ring_${i}`));
  const restArm = turnOf(arm);
  const restBody = turnOf(body);
  const lean = (part: number[], dx: number) => [part[0] + dx, part[1], part[2]];
  const armAt = cycle(PERIOD, [
    [0, restArm], [WIND, lean(restArm, -86)], [CAST, lean(restArm, 20)], [LANDS, restArm],
    [BITE - 0.05, restArm], [BITE + 0.06, lean(restArm, -12)], [BITE + 0.2, lean(restArm, 2)], [BITE + 0.36, lean(restArm, -12)], [BITE + 0.5, restArm],
    [STRIKE + 0.3, lean(restArm, -50)], [AWAY, lean(restArm, -40)], [8.7, restArm],
  ], phase);
  const bodyAt = cycle(PERIOD, [
    [0, restBody], [WIND, lean(restBody, -11)], [CAST, lean(restBody, 7)], [LANDS, restBody],
    [BITE, restBody], [STRIKE + 0.3, lean(restBody, -13)], [AWAY, lean(restBody, -8)], [8.7, restBody],
  ], phase);
  const events: [number, number][] = [[LANDS, 0], [BITE + 0.05, 1], [BITE + 0.35, 0], [STRIKE, 2]];
  if (!data.keeps) events.push([AWAY, 1]);
  const floatAt = vec(data.float);
  const away = vec(data.away);
  const level = C.POND_LEVEL;
  const height = C.SWELL;
  const tip = new Vector3();
  const hang = new Vector3();
  const afloat = new Vector3();
  const bob = new Vector3();
  const reach = new Vector3();
  const hooked = new Vector3();
  const out: number[] = [];
  const size = data.size;

  return (time) => {
    const u = wrap(time + phase, PERIOD);
    turn(arm, armAt(time, out));
    turn(body, bodyAt(time, out));
    rod.updateWorldMatrix(true, false);
    rod.localToWorld(tip.set(data.tip[0], data.tip[1], data.tip[2]));
    hang.copy(tip);
    hang.z -= 0.42;
    afloat.set(floatAt.x, floatAt.y, floatAt.z + level - 0.04 + swell(floatAt.x, floatAt.y, time, height));
    if (u >= BITE && u < STRIKE) afloat.z -= 0.2 * Math.max(Math.sin(((u - BITE) * 2 * Math.PI) / 0.3), 0);
    if (u < CAST || u >= CAUGHT) bob.copy(hang);
    else if (u < LANDS) arc(hang, afloat, smooth((u - CAST) / (LANDS - CAST)), 0.9, bob);
    else if (u < STRIKE) bob.copy(afloat);
    else arc(afloat, hang, smooth((u - STRIKE) / (CAUGHT - STRIKE)), 0.6, bob);
    bobber.visible = true;
    bobber.position.copy(bob);
    reach.subVectors(bob, tip);
    const length = Math.max(reach.length(), 0.01);
    line.visible = true;
    line.position.copy(tip);
    line.quaternion.setFromUnitVectors(UP, reach.divideScalar(length));
    line.scale.set(1, length, 1);
    // The fish: on the hook from the strike, then off to the barrel or back to the pond.
    fish.visible = u >= STRIKE && u < AWAY;
    if (u >= STRIKE && u < CAUGHT) {
      fish.position.set(bob.x, bob.y, bob.z - 0.36);
      fish.rotation.set(-Math.PI / 2, 0.5 * Math.sin(u * 34), 0, "ZYX");
      fish.scale.setScalar(size);
    } else if (fish.visible) {
      const v = (u - CAUGHT) / (AWAY - CAUGHT);
      hooked.copy(hang);
      hooked.z -= 0.36;
      arc(hooked, away, v, 0.9, fish.position);
      fish.rotation.set(-Math.PI / 2 + 5 * v, 0.6 * v, 0, "ZYX");
      fish.scale.setScalar(size);
    }
    ripple(rings, events, u, u < AWAY ? floatAt : away, level, time, height);
  };
}

/** A fish that leaps out of the water every few seconds, somewhere new each time. */
function leaper(ctx: Ctx, name: string, ringNames: readonly string[], spots: readonly (readonly [readonly number[], readonly number[]])[], period: number, phase: number, size = 0.6) {
  const { C } = ctx;
  const fish = ctx.node(name);
  const rings = ringNames.map((ring) => ctx.node(ring));
  const leap: [number, number] = [0.5, 1.25]; // when in each period it is in the air
  const from = new Vector3();
  const back = new Vector3();
  const here = new Vector3();
  const soon = new Vector3();
  /** `once` is the moment somebody clicked the pond: it leaps a single time, then. */
  return (time: number, once: number | null = null) => {
    const clock = once === null ? time + phase : time - once + leap[0];
    const spot = spots[once === null ? Math.floor(clock / period) % spots.length : 0];
    const u = once === null ? wrap(clock, period) : clock;
    from.set(spot[0][0], spot[0][1], C.POND_LEVEL);
    back.set(spot[1][0], spot[1][1], C.POND_LEVEL);
    fish.visible = u >= leap[0] && u < leap[1];
    if (fish.visible) {
      const v = (u - leap[0]) / (leap[1] - leap[0]);
      arc(from, back, v, 0.75, here);
      if (v < 0.98) arc(from, back, Math.min(v + 0.02, 1), 0.75, soon).sub(here);
      else soon.copy(here).sub(arc(from, back, v - 0.02, 0.75, soon));
      fish.position.copy(here);
      fish.rotation.set(-Math.atan2(soon.z, Math.hypot(soon.x, soon.y)), 0, Math.atan2(soon.x, -soon.y), "ZYX");
      fish.scale.setScalar(size);
    }
    ripple([rings[0]], [[leap[0], 0]], u, from, C.POND_LEVEL, time, C.SWELL);
    ripple([rings[1]], [[leap[1], 0]], u, back, C.POND_LEVEL, time, C.SWELL);
  };
}

/**
 * A small thing on two wings (a butterfly, a bat). The wings beat for ever; the whole
 * wanders round its home, never further than `reach`, on a path that does not quite
 * repeat, and faces the way it flies.
 */
function flier(ctx: Ctx, name: string, reach: readonly number[], speed: number, beat: number, seed: number): (time: number) => void {
  const root = ctx.node(name);
  const wings: [Object3D, number][] = [[ctx.node(`${name}_l`), -1], [ctx.node(`${name}_r`), 1]];
  const home = root.position.clone();
  const turns = [[0.71, 1.93], [0.53, 1.61], [0.97, 2.39]]; // two unrelated turns on each axis
  const at = (time: number, out: Vector3) => {
    const here = turns.map(([a, b], k) => reach[k] * (0.62 * Math.sin(a * speed * time + seed * 1.7 + 2.1 * k) + 0.38 * Math.sin(b * speed * time + seed * 0.9 + 1.3 * k)));
    return out.set(home.x + here[0], home.y + here[1], home.z + here[2] + 0.05 * Math.sin(time * 9 + seed));
  };
  const ahead = new Vector3();
  const behind = new Vector3();
  return (time) => {
    at(time, root.position);
    at(time + 0.04, ahead).sub(at(time - 0.04, behind));
    root.rotation.set(0, 0, Math.atan2(ahead.y, ahead.x) - Math.PI / 2);
    for (const [wing, side] of wings) wing.rotation.set(0, -side * (48 + 38 * Math.sin(time * 2 * Math.PI * beat + seed)) * RAD, 0); // mostly held up: seen from the side they still show
  };
}

/**
 * The pond and the meadow: two friends fishing, a fish that leaps, butterflies. And what
 * answers a click: the pond (a fish leaps where it was touched), a friend (a wave), the
 * tree (petals), the pumpkin (a jump).
 */
export function setMeadowLife(ctx: Ctx) {
  const { tl, C, points, stage } = ctx;
  const fishers = points.fishing.map((data) => fisher(ctx, data));
  const jumps = leaper(ctx, "Leaper", ["Leaper_Ring_0", "Leaper_Ring_1"], [[[-7.9, 2.0], [-7.3, 2.8]], [[-6.0, 1.3], [-6.8, 1.6]]], 5.2, 2.0);
  const butterflies = [0, 1, 2, 3].map((i) => flier(ctx, `Butterfly_${i}`, [1.3, 1.1, 0.45], 0.9 + 0.17 * i, 4.6 + 0.7 * i, 2.3 * i + 0.4));
  const seen: [number, number] = [1.05, C.STOMP - 0.06];

  const { pond } = stage.data.spots;
  const splashSpot: [number[], number[]][] = [[[0, 0], [0, 0]]];
  const splash = leaper(ctx, "Clicked_Fish", ["Clicked_Ring_0", "Clicked_Ring_1"], splashSpot, 5.2, 0);
  let splashed: number | null = null;
  ctx.touch.add({
    name: "pond",
    at: new Vector3(pond.center[0], pond.center[1], C.POND_LEVEL),
    half: new Vector3(pond.radius[0], pond.radius[1], 0.12),
    seen,
    click: (time, hit) => {
      const dx = (hit.x - pond.center[0]) / pond.radius[0];
      const dy = (hit.y - pond.center[1]) / pond.radius[1];
      const out = Math.hypot(dx, dy);
      if (out > 0.92) return; // the bank, not the water
      // It leaps from where the water was touched, toward the middle of the pond.
      const reach = 0.9 / Math.max(Math.hypot(hit.x - pond.center[0], hit.y - pond.center[1]), 0.9);
      splashSpot[0] = [[hit.x, hit.y], out < 0.25 ? [hit.x + 0.7, hit.y + 0.5] : [hit.x + (pond.center[0] - hit.x) * reach, hit.y + (pond.center[1] - hit.y) * reach]];
      splashed = time + 0.12;
    },
  });

  const wavers = ["Steve", "Alex"].map((tag) => {
    const root = ctx.node(tag);
    const arm = ctx.node(`${tag}_arm_l`);
    const rest = turnOf(arm);
    const state = { waved: -Infinity };
    ctx.touch.add({ name: `friend_${tag}`, at: root.position.clone().add(new Vector3(0, 0, 1)), half: new Vector3(0.45, 0.45, 1.05), seen, click: (time) => void (state.waved = time + 0.1) });
    return (time: number) => {
      const dt = time - state.waved;
      const up = dt >= 0 ? smooth(dt / 0.25) * (1 - smooth((dt - 1.5) / 0.3)) : 0;
      turn(arm, [rest[0] * (1 - up), (-150 + 22 * Math.sin(dt * 14)) * up + rest[1] * (1 - up), rest[2]]);
    };
  });

  const petals = ctx.node("Petal_Burst");
  let shaken = -Infinity;
  ctx.touch.add({ name: "tree", at: new Vector3(6.0, 7.0, 7.2), half: new Vector3(3.2, 3.2, 2.6), seen, click: (time) => void (shaken = time + 0.1) });

  const { pumpkin } = ctx.cast;
  ctx.touch.add({ name: "pumpkin", at: vec(C.LANDING_SPOT).add(new Vector3(0, 0, 0.5)), half: new Vector3(0.62, 0.62, 0.62), seen: [0.95, 1.44], click: (time) => pumpkin.jump(time + 0.05) });

  tl.during(0.6, 2.0, ({ time }) => {
    for (const fish of fishers) fish(time);
    jumps(time);
    for (const fly of butterflies) fly(time);
    for (const wave of wavers) wave(time);
    if (splashed !== null && time - splashed < 3) splash(time, splashed);
    else splash(time, time + 9); // (long before its leap: nothing shows)
    const since = time - shaken;
    petals.visible = since >= 0 && since < 3.2;
    petals.userData.grow = Math.min(since / 0.15, 1) * (1 - smooth((since - 2.6) / 0.6));
  });
}

/** Under the cave's roof: bats, and drops that fall and leave a ring. */
export function setCaveLife(ctx: Ctx) {
  const { tl, points } = ctx;
  const reaches = ctx.pick([[1.3, 1.6, 0.4], [1.0, 1.4, 0.4]], [[3.2, 2.0, 0.3], [2.8, 1.8, 0.25]]);
  const bats = [0, 1].map((i) => flier(ctx, `Cave_Bat_${i}`, reaches[i], 2.3 - 0.4 * i, 7.5, 5.1 * i + 1.0));
  const gravity = 14;
  const drips = points.drips.map(([x, y, top, floor, period, phase], i) => {
    const drop = ctx.node(`Cave_Drop_${i}`);
    const ring = ctx.node(`Cave_Drip_Ring_${i}`);
    const fall = Math.sqrt((2 * (top - floor)) / gravity);
    ring.position.set(x, y, floor + 0.02);
    return (time: number) => {
      const u = wrap(time + phase, period);
      drop.visible = u < fall;
      drop.position.set(x, y, top - 0.5 * gravity * u * u);
      const spread = (u - fall) / 0.7;
      const wide = 0.25 + 0.7 * clamp01(spread);
      ring.scale.set(wide, wide, 1);
      fade(ring, spread >= 0 && spread <= 1 ? 0.7 * (1 - spread) : 0);
    };
  });
  tl.during(2.3, 4.2, ({ time }) => {
    for (const bat of bats) bat(time);
    for (const drip of drips) drip(time);
  });
}
