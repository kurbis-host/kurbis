/**
 * Life: what moves by itself, in real time, whatever the scroll does. Wind, water and
 * drifting flakes live in the shaders; here are the creatures and the loops.
 */

import { BoxGeometry, Euler, Mesh, Quaternion, Vector3, type Material, type Object3D } from "three";
import { clamp01, cycle, smooth, wrap } from "../anim/easing";
import { fade } from "../anim/timeline";
import { Player, addPose, keyed, type Delta } from "../anim/player";
import { COPY } from "../copy";
import { loadPicture } from "../engine/bundle";
import { makeItem } from "../engine/item";
import { modelMaterial } from "../engine/models";
import { vec, type Ctx } from "./context";
import { setJockey, type Watch } from "./jockey";
import type { Speaker } from "./speech";

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
 * The leather boot a friend now and then fishes up: the pack's pictures under these keys
 * of the art's HUD (scripts/export_web.py), the boot in the game's default leather
 * colour, and over it, as it is, what the dye leaves alone.
 */
const BOOT = ["boot", "boot_overlay"] as const;
const LEATHER = [0xa0, 0x65, 0x40];
/** An item in the world is half a block across, as the game draws one (dropped, or flying to whoever fished it up). */
const ITEM = 0.5;
/** Seconds a boot lies on the bank before it is gone. */
const JUNK_LIES = 4.5;
/** Which catch of a stay is the first boot (counting from 0: the third), and how many catches on the next one is (either). */
const FIRST_JUNK = 2;
const JUNK_EVERY = [4, 5] as const;

/** Where a boot comes down by each friend: on the bank in front of them, a little to one side, clear of them and of the reeds, where either frame sees it (the ground there is as high as where they stand). */
const BANKS: Record<string, readonly number[]> = { Steve: [-4.7, 2.7, 1], Alex: [-8.75, 5.3, 1] };

/**
 * Which catches are a boot, as fishing in the game now and then brings up junk: about one
 * in four or five. The two friends' catches are counted together (they strike in turn,
 * five seconds apart), and afresh each time the visitor comes to the pond: never the
 * first they see, which is a fish, and the third, so that a boot comes up within about
 * fifteen seconds of their stopping there.
 */
export class Junk {
  /** The visitor's stay at the pond, counted from 1; 0 while they are elsewhere. */
  stay = 0;
  private stays = 0;
  private since = 0;
  private catches = 0;
  private next: number = FIRST_JUNK;

  constructor(private readonly random: () => number = Math.random) {}

  /** Whether the visitor is at the pond at `time` (told every frame, before any catch). */
  at(here: boolean, time: number) {
    if (!here) this.stay = 0;
    else if (!this.stay) {
      this.stays += 1;
      this.stay = this.stays;
      this.since = time;
      this.catches = 0;
      this.next = FIRST_JUNK;
    }
  }

  /** Whether the catch struck at `strike` is a boot (asked once a catch, as it is struck). */
  boot(strike: number): boolean {
    if (!this.stay || strike < this.since) return false; // (struck before they came: they did not see it)
    const which = this.catches;
    this.catches += 1;
    if (which !== this.next) return false;
    this.next += JUNK_EVERY[this.random() < 0.5 ? 0 : 1];
    return true;
  }
}

/**
 * A friend fishing, for ever: wind up, cast, wait, a bite, the strike, and a fish that
 * goes into the barrel (or back into the pond). The body acts it out with its whole
 * weight (lib/landing/anim/player.ts); the bobber, the line, the fish and the ripples
 * follow from where the rod's tip is. Now and then (`junk`) it is a boot instead, which
 * comes down on the bank by them (`boot`, at `bank`); they say so (`say`). Something
 * going by (`watch`: a chicken jockey) it watches instead, and shouts.
 */
function fisher(ctx: Ctx, data: Ctx["points"]["fishing"][number], player: Player, junk: Junk, boot: Mesh, bank: Vector3, say: (time: number) => void): (time: number, dt: number, extra: Delta | null, watch?: Watch | null) => void {
  const { C } = ctx;
  const PERIOD = 9.6;
  const [WIND, CAST, LANDS, BITE, STRIKE, CAUGHT, AWAY] = [0.45, 0.72, 1.35, 6.0, 6.6, 7.3, 7.95]; // moments in the loop
  const { tag, phase } = data;
  const rod = ctx.node(`${tag}_Rod`);
  const bobber = ctx.node(`${tag}_Bobber`);
  const line = ctx.node(`${tag}_Line`);
  const fish = ctx.node(`${tag}_Fish`);
  const rings = [0, 1, 2].map((i) => ctx.node(`${tag}_Ring_${i}`));
  // The cast and the catch, as changes from how the art stood them.
  const acts: [number, Delta][] = [
    [0, {}],
    [WIND - 0.28, { body: [-3, 0, 0], armR: [-18, 0, 0], elbowR: 18 }], // gathering itself
    [WIND, { body: [-11, 0, -7], armR: [-92, 0, -8], elbowR: 72, head: [-9, 0, 0], legR: [-6, 0, 0], kneeL: 12 }], // wound up: the rod back over the shoulder
    [CAST - 0.05, { body: [11, 0, 5], armR: [32, 0, 4], elbowR: 4, head: [5, 0, 0], kneeR: 10, legL: [-5, 0, 0] }], // the whip
    [CAST + 0.14, { body: [8, 0, 3], armR: [26, 0, 2], elbowR: 9 }], // follow-through
    [LANDS + 0.2, { body: [2, 0, 0], armR: [3, 0, 0], elbowR: 2 }],
    [BITE - 0.04, {}],
    [BITE + 0.07, { armR: [-11, 0, 0], body: [3, 0, 0], elbowR: 8 }],
    [BITE + 0.2, { armR: [4, 0, 0] }],
    [BITE + 0.36, { armR: [-13, 0, 0], elbowR: 10 }],
    [STRIKE - 0.13, { body: [7, 0, 0], armR: [7, 0, 0], kneeR: 22, kneeL: 22, legR: [-12, 0, 0], legL: [-10, 0, 0], lift: -0.06 }], // down, before the heave
    [STRIKE + 0.24, { body: [-15, 0, -4], armR: [-64, 0, -4], elbowR: 58, head: [-12, 0, 0], kneeR: 4, kneeL: 6 }], // the heave
    [CAUGHT - 0.05, { body: [-8, 0, 0], armR: [-50, 0, 0], elbowR: 42, kneeR: 18, kneeL: 18, lift: -0.03 }],
    [CAUGHT + 0.12, { body: [-5, 0, 0], armR: [-48, 0, 0], elbowR: 40, lift: 0.16, legR: [8, 0, 0], kneeR: 30 }], // a hop of joy
    [CAUGHT + 0.36, { body: [-4, 0, 0], armR: [-44, 0, 0], elbowR: 38 }],
    [AWAY, { armR: [-28, 0, 0], elbowR: 24 }],
    [8.8, {}],
    [PERIOD, {}],
  ];
  // A boot instead: it comes off the hook as it is heaved out and lands by their feet
  // (seconds from the strike), and there is no hop of joy. They watch it come down, and
  // sag, the head on one side, and slump, looking at it.
  const [OFF, DOWN] = [0.3, 0.9];
  const down = STRIKE + DOWN;
  const glum: [number, Delta][] = [
    ...acts.filter(([at]) => at < CAUGHT),
    [down + 0.12, { body: [2, 0, 0], armR: [-22, 0, 0], elbowR: 20, kneeR: 10, kneeL: 10, lift: -0.03 }], // it is down
    [down + 0.5, { body: [6, 0, 0], armR: [-18, 8, 0], elbowR: 18, armL: [6, -16, 0], elbowL: 14, head: [0, 12, 0], kneeR: 6, kneeL: 6 }], // the arms go loose
    [down + 1.1, { body: [11, 0, 0], armR: [-4, 0, 0], elbowR: 6, armL: [5, 0, 0], head: [0, 6, 0], kneeR: 6, kneeL: 6, lift: -0.02 }], // and a slump
    [9.25, { body: [5, 0, 0] }],
    [PERIOD, {}],
  ];
  const events: [number, number][] = [[LANDS, 0], [BITE + 0.05, 1], [BITE + 0.35, 0], [STRIKE, 2]];
  const quiet = [...events]; // (with a boot, nothing goes back into the pond)
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
  const size = data.size;
  /** The loop whose catch was last decided on; the boot that is out, if one is: the loop it came up in, when it was struck, the stay it belongs to, which way it faces, whether it has been remarked on, and where it came off the hook. */
  let asked = -Infinity;
  let junked: { loop: number; at: number; stay: number; facing: number; said: boolean; off: Vector3 } | null = null;
  const HANG = 0.22; // from the bobber's underside to the boot's middle
  const lying = new Vector3(bank.x, bank.y, bank.z + ITEM / 2 + 0.1);
  const camera = ctx.stage.camera;

  return (time, dt, extra, watch = null) => {
    const u = wrap(time + phase, PERIOD);
    const loop = Math.floor((time + phase) / PERIOD);
    if (u >= STRIKE && loop !== asked) {
      asked = loop;
      const at = time - (u - STRIKE);
      // (Its front toward the camera a little after it is down, as it turns.)
      if (boot.parent && junk.boot(at)) junked = { loop, at, stay: junk.stay, facing: Math.atan2(camera.position.x - bank.x, -(camera.position.y - bank.y)) - 0.6, said: false, off: floatAt.clone() };
    }
    // Gone with the visitor, or in its time.
    if (junked && (junked.stay !== junk.stay || time - junked.at > DOWN + JUNK_LIES + 0.25)) junked = null;
    const junky = junked?.loop === loop; // (this catch is the boot)
    addPose(player.rest(), keyed(junky ? glum : acts, u));
    if (extra) addPose(player.target, extra);
    // What it watches: the bobber while it waits, the fish once it is caught (or the boot, a while longer).
    player.look = u >= LANDS - 0.3 && u < STRIKE ? bob : junky && u >= STRIKE && u < down + 1.7 ? boot.position : u >= STRIKE && u < AWAY + 0.2 ? fish.position : null;
    player.face.wide = u >= BITE && u < STRIKE + 0.35;
    player.face.mouth = u >= BITE && u < STRIKE + 0.35 ? "open" : u >= CAUGHT && u < AWAY + 0.9 && !junky ? "smile" : "closed";
    if (extra) player.face.mouth = "smile";
    if (watch) player.look = watch.at;
    if (watch?.shouting) [player.face.wide, player.face.mouth] = [true, "open"];
    player.update(time, dt);
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
    fish.visible = !junky && u >= STRIKE && u < AWAY;
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
    // Or the boot: on the hook in its place, swinging as it comes up out of the water; off
    // it and in an arc to the bank, where it lies as the game's dropped items do (its
    // underside a tenth of a block up, bobbing a tenth either way, turning a radian a
    // second) until it is gone.
    boot.visible = junked !== null;
    if (junked) {
      const age = time - junked.at;
      if (age < OFF) {
        const swing = 0.35 * Math.sin(age * 6) * (1 - age / OFF);
        const [under, across] = [Math.cos(swing) * HANG, Math.sin(swing) * HANG];
        boot.position.set(bob.x - across * Math.cos(junked.facing), bob.y - across * Math.sin(junked.facing), bob.z - 0.02 - under); // (hung from under the bobber)
        boot.rotation.set(0, swing, junked.facing, "ZYX");
        junked.off.copy(boot.position);
      } else {
        if (age < DOWN) arc(junked.off, lying, (age - OFF) / (DOWN - OFF), 0.5, boot.position);
        else boot.position.set(lying.x, lying.y, lying.z + 0.1 * Math.sin(2 * (age - DOWN)));
        boot.rotation.set(0, 0, junked.facing + Math.max(age - DOWN, 0), "ZYX");
      }
      boot.scale.setScalar(1 - smooth((age - DOWN - JUNK_LIES) / 0.25));
      if (!junked.said && age >= DOWN + 0.35) {
        junked.said = true;
        say(time);
      }
    }
    ripple(rings, junky ? quiet : events, u, u < AWAY ? floatAt : away, level, time, height);
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

/** The pack's picture of the oak log the meadow's trees are made of: its key in the art's HUD (scripts/export_web.py). */
const WOOD = "wood";
/** A block's dropped item: a quarter of a block across. Seconds it hops for, and lies for before it is gone. */
const DROP = 0.25;
const HOP = 0.7;
const LIES = 5;

/**
 * Blocks that drop, as the game's dropped items do: each hops out of `from` (inside the
 * trunk it came from, so it comes out of the bark), lands on the grass at a spot of its
 * own, and bobs and turns there for a few seconds before it is gone. As many may be out
 * at once as there are spots; another drop takes the oldest.
 */
function dropper(ctx: Ctx, from: readonly number[], spots: readonly (readonly number[])[], look: Promise<Material | null>) {
  // (Its sides' pictures upright, Z being up; v counted from the top, as the models' shader takes it.)
  const geometry = new BoxGeometry(DROP, DROP, DROP).rotateX(Math.PI / 2);
  const uv = geometry.getAttribute("uv");
  for (let i = 0; i < uv.count; i += 1) uv.setY(i, 1 - uv.getY(i));
  const blocks = spots.map((spot) => ({ mesh: new Mesh(geometry), spot: vec(spot), at: -Infinity }));
  let ready = false;
  void look.then((material) => {
    if (!material) return;
    for (const { mesh } of blocks) {
      mesh.material = material;
      mesh.visible = false;
      ctx.stage.scene.add(mesh);
    }
    ready = true;
  });
  const start = vec(from);
  const land = new Vector3();
  let next = 0;
  return {
    drop(time: number) {
      blocks[next].at = time;
      next = (next + 1) % blocks.length;
    },
    /** Where each is at `time`, at `size` (the meadow takes them with it when it goes). */
    update(time: number, size: number) {
      if (!ready) return;
      blocks.forEach(({ mesh, spot, at }, k) => {
        const age = time - at;
        const left = 1 - smooth((age - HOP - LIES) / 0.25);
        mesh.visible = age >= 0 && left > 0 && size > 0;
        if (!mesh.visible) return;
        // As the game's: its underside a tenth of a block over the ground, bobbing a tenth either way, turning a radian a second.
        land.set(spot.x, spot.y, spot.z + DROP / 2 + 0.1);
        if (age < HOP) arc(start, land, age / HOP, 1.0, mesh.position);
        else mesh.position.set(land.x, land.y, land.z + 0.1 * Math.sin(2 * (age - HOP)));
        mesh.rotation.set(0, 0, 0.9 * k + age);
        mesh.scale.setScalar(left * size);
      });
    },
  };
}

/**
 * The pond and the meadow: two friends fishing, a fish that leaps, butterflies. And what
 * answers a click: the pond (a fish leaps where it was touched), a friend (a wave), the
 * tree (petals, and a block of its wood), the pumpkin (a jump).
 */
export function setMeadowLife(ctx: Ctx) {
  const { tl, C, points, stage } = ctx;
  // The two friends, alive (lib/landing/anim/player.ts).
  const friends = new Map(points.fishing.map((data) => [data.tag, new Player(stage, data.tag, data.tag === "Alex", data.tag === "Alex" ? 1 : 0)]));
  // Now and then one fishes up a boot, drawn as the game draws an item in the world, and
  // says so (in a bubble, as when they are clicked: `talks`, below).
  const junk = new Junk();
  const boots = new Map(points.fishing.map((data) => [data.tag, new Mesh()]));
  const pictures = BOOT.map((key) => stage.data.hud?.[key]);
  if (pictures.every((name) => name && stage.pack.has(name))) {
    const layers = pictures.map((name, k) => ({ picture: stage.pack.picture(name!), tint: k === 0 ? LEATHER : undefined }));
    void makeItem(layers, ITEM).then(({ geometry, texture }) => {
      const material = modelMaterial(stage.globals, { kind: "tex", image: BOOT[0] }, { ...stage.pictures, named: new Map([[BOOT[0], texture]]) });
      for (const [tag, mesh] of boots) {
        mesh.geometry = geometry;
        mesh.material = material;
        mesh.name = `${tag}_Boot`;
        mesh.visible = false;
        stage.scene.add(mesh);
      }
    }, () => {});
  }
  const talks = new Map<string, (time: number, line: string, stay?: number) => void>();
  /** The last boot to come up, and in which of the visitor's stays at the pond. */
  let booted: { at: number; stay: number } | null = null;
  const fishers = points.fishing.map((data) => ({
    tag: data.tag,
    play: fisher(ctx, data, friends.get(data.tag)!, junk, boots.get(data.tag)!, vec(BANKS[data.tag] ?? data.away), (time) => {
      booted = { at: time, stay: junk.stay };
      talks.get(data.tag)?.(time, COPY.talk.fishing);
    }),
  }));
  const jumps = leaper(ctx, "Leaper", ["Leaper_Ring_0", "Leaper_Ring_1"], [[[-7.9, 2.0], [-7.3, 2.8]], [[-6.0, 1.3], [-6.8, 1.6]]], 5.2, 2.0);
  const butterflies = [0, 1, 2, 3].map((i) => flier(ctx, `Butterfly_${i}`, [1.3, 1.1, 0.45], 0.9 + 0.17 * i, 4.6 + 0.7 * i, 2.3 * i + 0.4));
  const seen: [number, number] = [1.05, C.STOMP - 0.06];
  // Now and then a chicken jockey runs by behind the pond, and Steve shouts it (./jockey.ts).
  const jockey = setJockey(ctx, seen, (time, stay) => talks.get("Steve")?.(time, COPY.talk.jockey, stay));

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

  // Click a friend and they wave back: the free arm up, the hand going, a smile. And they
  // say something (./speech.ts); whoever is fishing may have caught a boot. (When one
  // really comes up they say so too, in place of whatever they were saying: `talks`; and at
  // the next click after, either of them has more to say of it, while the visitor stays.)
  const waved = new Map<string, number>();
  const said = COPY.talk.pond;
  const caught = () => (booted && booted.stay === junk.stay ? booted.at : null);
  for (const tag of ["Steve", "Alex"] as const) {
    const root = ctx.node(tag);
    const fishing = points.fishing.some((data) => data.tag === tag);
    const lines = fishing ? [...said[tag], COPY.talk.fishing] : said[tag];
    const speaker: Speaker = { rig: tag, seen, headline: [ctx.find("Landing_Type")], lines, after: [{ since: caught, lines: said.boot[tag] }] };
    const talk = ctx.speech.speaker(speaker);
    talks.set(tag, (time, line, stay) => ctx.speech.tell(speaker, time, line, stay));
    ctx.touch.add({
      name: `friend_${tag}`,
      at: root.position.clone().add(new Vector3(0, 0, 1)),
      half: new Vector3(0.45, 0.45, 1.05),
      seen,
      click: (time) => {
        waved.set(tag, time + 0.05);
        talk(time);
      },
    });
  }
  const wave = (tag: string, time: number): Delta | null => {
    const since = time - (waved.get(tag) ?? -Infinity);
    if (since < 0 || since > 2.2) return null;
    const up = smooth(since / 0.3) * (1 - smooth((since - 1.7) / 0.4));
    const hand = Math.sin(since * 13);
    // As the trailer waves: the arm up and out to the side, nearly straight, swinging from the shoulder.
    // (Straight up from where the art held it: its forward tilt is taken out, or the arm points at the camera.)
    const base = friends.get(tag)!.base.turn.armL;
    return { armL: [-base[0] * up, (-150 + 20 * hand) * up, -base[2] * up], elbowL: 10 * up, head: [-5 * up, -3 * up * hand, 0], body: [0, 5 * up, 0] };
  };
  let last = 0;

  const petals = ctx.node("Petal_Burst");
  let shaken = -Infinity;
  // A tree punched gives wood, as the game's oldest achievement has it: the toast once a
  // visit, and a block of the trunk's oak each time, out onto the grass beside it. Its
  // picture is the pack's (a key of the art's HUD): the toast's icon, and the block's sides.
  const wood = stage.data.hud?.[WOOD];
  const look = wood && stage.pack.has(wood) ? loadPicture(stage.pack.picture(wood)).then((picture) => modelMaterial(stage.globals, { kind: "tex", image: WOOD }, { ...stage.pictures, named: new Map([[WOOD, picture]]) }), () => null) : Promise.resolve(null);
  const drops = dropper(ctx, [6.15, 7.35, 2.4], [[4.8, 7.3, 1], [4.15, 7.6, 1], [3.45, 7.35, 1]], look);
  const punch = (time: number) => ctx.ui?.toast(COPY.toasts.events.wood, WOOD, time, { from: seen[0], until: seen[1] });
  const shake = (time: number) => {
    shaken = time + 0.1;
    drops.drop(time + 0.1);
    punch(time);
  };
  // The crown, and the trunk under it.
  ctx.touch.add({ name: "tree", at: new Vector3(6.0, 7.0, 7.2), half: new Vector3(3.2, 3.2, 2.6), seen, click: shake });
  ctx.touch.add({ name: "trunk", at: new Vector3(6.5, 7.5, 3.5), half: new Vector3(0.6, 0.6, 2.5), seen, click: shake });
  // A tall frame does not reach that tree: there the giant behind the pond is the one to punch (too far off for its block to be seen).
  if (ctx.tall) ctx.touch.add({ name: "giant", at: new Vector3(-22, 16, 12), half: new Vector3(1, 1, 6), seen, click: punch });

  const { pumpkin } = ctx.cast;
  ctx.touch.add({ name: "pumpkin", at: vec(C.LANDING_SPOT).add(new Vector3(0, 0, 0.5)), half: new Vector3(0.62, 0.62, 0.62), seen: [0.95, 1.44], click: (time) => pumpkin.jump(time + 0.05) });

  // The meadow lives while it is in sight: at the landing, and home again at the end.
  const live = ({ s, time }: { s: number; time: number }) => {
    const dt = Math.min(Math.max(time - last, 0), 0.1);
    last = time;
    junk.at(s >= seen[0] && s < seen[1], time); // (boots come up only while the visitor is at the pond)
    jockey.update(s >= seen[0] && s < seen[1], time, dt); // (and so does a jockey)
    for (const { tag, play } of fishers) play(time, dt, wave(tag, time), tag === "Steve" ? jockey.watch : null);
    jumps(time);
    for (const fly of butterflies) fly(time);
    if (splashed !== null && time - splashed < 3) splash(time, splashed);
    else splash(time, time + 9); // (long before its leap: nothing shows)
    const since = time - shaken;
    petals.visible = since >= 0 && since < 3.2;
    petals.userData.grow = Math.min(since / 0.15, 1) * (1 - smooth((since - 2.6) / 0.6));
    drops.update(time, 1 - smooth((s - C.STOMP) / 0.07)); // (gone as the ground breaks, as the type is)
  };
  tl.during(0.6, 2.0, live);
  tl.during(C.HOME - 0.05, C.END + 0.1, live);
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
