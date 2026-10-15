/**
 * A chicken jockey: a baby zombie riding a chicken, the game's rare spawn, and since the
 * Minecraft movie (2025) one of its best-known lines. In about one visit in four, a
 * visitor who stays at the pond a few seconds sees one run across the meadow behind it:
 * out of the reeds behind Alex, past Steve, and on until the frame has lost it. Steve
 * shouts it, and his eyes go after it. It comes once a visit: a visitor who scrolls
 * straight on never sees it, and one who scrolls away while it runs ends it for good.
 *
 * The art built both as the game models them, the zombie seated as the game seats a rider,
 * and laid out the run in each frame on the ground's own heights (`points.jockey`). Here
 * it runs: the chicken's legs patter, its wings flap now and then (and on every hop up a
 * block), the zombie bounces with each step, its arms out. Clicked, the chicken flaps and
 * hops, and hurries.
 */

import { Quaternion, Vector3 } from "three";
import { clamp01, lerp, smooth } from "../anim/easing";
import type { Ctx } from "./context";

/** The share of visits it comes in; seconds at the pond before it does (somewhere between). */
const CHANCE = 0.25;
const WAIT = [6, 10] as const;
/** Seconds over which it crosses the stretch of its run the lens sees; seconds Steve's shout stays (a shout is short). */
const CROSSING = 5.2;
const SHOUT = 1.2;
/** Steps a second its legs make, and more for each block a second it goes (a hurrying chicken patters faster the faster it goes); degrees they swing. */
const PATTER = [2.4, 0.9] as const;
const SWING = 55;
/** Pixels the rider is jolted down at each footfall (only down: never higher than its seat). */
const BOUNCE = 1.2;
/** Seconds a hop up a block takes, and how far over the arc's line it goes (blocks). */
const HOP = 0.34;
const LIFT = 0.35;
/** A click: how much faster it runs, for how long, and the hop it gives (blocks, seconds). */
const HURRY = 0.6;
const HURRIED = 2.5;
const STARTLE = [0.45, 0.36] as const;
/** Wings: beats a second while flapping, and the bursts a hurrying chicken gives now and then (seconds apart, and long). */
const BEAT = 7;
const BURSTS = [1.35, 0.4] as const;
/** Seconds Steve shouts it to the lens (eyes wide, mouth open), before he turns to watch it go. */
const STARTLED = 0.6;

const PX = 1 / 16;
const DEG = Math.PI / 180;
const X = new Vector3(1, 0, 0);

/**
 * Whether and when the jockey runs: decided once a visit, by the visit's first throw (and
 * how long it waits, by the second). The visitor's stays at the pond are told each frame;
 * after a few seconds of one it runs, once. If the stay ends while it runs, the run ends
 * with it and does not come again.
 */
export class JockeyTimer {
  /** Whether it comes this visit. */
  readonly comes: boolean;
  /** Seconds of a stay before it runs. */
  readonly wait: number;
  private since: number | null = null;
  private started: number | null = null;
  private over = false;

  constructor(random: () => number = Math.random) {
    this.comes = random() < CHANCE;
    this.wait = lerp(WAIT[0], WAIT[1], random());
  }

  /** Whether the visitor is at the pond at `time`. Returns seconds into the run while it is on, else null. */
  at(here: boolean, time: number): number | null {
    if (!this.comes || this.over) return null;
    if (!here) {
      this.since = null;
      if (this.started !== null) this.end();
      return null;
    }
    this.since ??= time;
    if (this.started === null && time - this.since >= this.wait) this.started = time;
    return this.started === null ? null : time - this.started;
  }

  /** It has run its course (or been sent off): it does not come again. */
  end() {
    this.started = null;
    this.over = true;
  }
}

/** What Steve does while it goes by: whether he is shouting it, and what he looks at (the lens as he shouts, then it). */
export interface Watch {
  shouting: boolean;
  at: Vector3;
}

export interface Jockey {
  /** Each frame, before the friends: whether the visitor is at the pond. */
  update(here: boolean, time: number, dt: number): void;
  /** What Steve watches, while it goes by. */
  watch: Watch | null;
}

/**
 * `seen` is the stretch of scroll the pond is in sight over, and `shout` has the friend who
 * sees it shout it, for `stay` seconds.
 */
export function setJockey(ctx: Ctx, seen: readonly [number, number], shout: (time: number, stay: number) => void, timer = new JockeyTimer()): Jockey {
  const run = ctx.points.jockey;
  const mount = ctx.node("Jockey");
  const part = (name: string) => ctx.node(`Jockey_${name}`);
  const [head, legR, legL, wingR, wingL] = ["head", "leg_r", "leg_l", "wing_r", "wing_l"].map(part);
  const rider = ctx.node("Jockey_Rider");
  const [riderHead, armR, armL] = ["head", "arm_r", "arm_l"].map((name) => part(`Rider_${name}`));
  mount.visible = false;
  // As the art stood them: the seat, and the arms out in front (the page adds their bob).
  const seat = rider.position.clone();
  const arms = [armR, armL].map((arm) => arm.quaternion.clone());
  const turn = new Quaternion();

  const length = (run.path.length - 1) * run.step;
  const speed = (run.seen[1] - run.seen[0]) / CROSSING;
  const here = new Vector3();
  const ahead = new Vector3();
  const behind = new Vector3();
  const at = (far: number, out: Vector3) => {
    const u = Math.min(Math.max(far / run.step, 0), run.path.length - 1);
    const i = Math.min(Math.floor(u), run.path.length - 2);
    const [x0, y0] = run.path[i];
    const [x1, y1] = run.path[i + 1];
    return out.set(lerp(x0, x1, u - i), lerp(y0, y1, u - i), 0);
  };
  // The ground under it, hopped: up a block in an arc that leaves before the edge (so its
  // front never goes into the step) and lands a little after; down one, off the edge in a
  // short drop. Steps close together make one bounding climb.
  const span = speed * HOP;
  const ground = (far: number) => {
    let z = run.ground[0][1];
    let up = 0;
    for (let k = 1; k < run.ground.length; k += 1) {
      const [edge, to] = run.ground[k];
      const rise = to - run.ground[k - 1][1];
      const u = clamp01(rise > 0 ? (far - edge + 0.65 * span) / span : (far - edge + 0.1 * span) / (0.8 * span));
      z += rise > 0 ? rise * u + LIFT * 4 * u * (1 - u) : rise * u * u + 0.12 * 4 * u * (1 - u);
      if (rise > 0 && u > 0 && u < 1) up = Math.max(up, 1 - Math.abs(2 * u - 1));
    }
    return { z, up };
  };

  let running = false;
  let far = 0;
  let last = -Infinity;
  let steps = 0;
  let shouted = false;
  let poked = -Infinity;
  let watching = 0;
  const middle = new Vector3();
  const watch: Watch = { shouting: false, at: ctx.stage.camera.position };
  const eyes = new Vector3();

  ctx.touch.add({
    name: "jockey",
    at: () => middle,
    half: new Vector3(0.45, 0.45, 0.75),
    seen,
    here: () => running,
    click: (time) => {
      poked = time;
    },
  });

  const jockey: Jockey = {
    watch: null,
    update(atPond, time, dt) {
      // (Not asked for a while: the visitor was elsewhere, taken there at once by the bar or a
      // link, or scrolled past where the meadow lives. They left, as surely as one who scrolled.)
      if (time - last > 0.3) timer.at(false, time);
      last = time;
      const t = timer.at(atPond, time);
      if (t === null) {
        if (running) {
          running = false;
          mount.visible = false;
          jockey.watch = null;
        }
        return;
      }
      if (!running) {
        running = true;
        far = 0;
        steps = 0;
        shouted = false;
        poked = -Infinity;
        watching = 0;
      }
      // Clicked: a start, a hop, and a hurry that wears off.
      const since = time - poked;
      const hurry = HURRY * smooth(since / 0.25) * (1 - smooth((since - HURRIED) / 1.0));
      far += speed * (1 + hurry) * dt;
      if (far >= length) {
        timer.end();
        running = false;
        mount.visible = false;
        jockey.watch = null;
        return;
      }
      mount.visible = true;

      // Along its line, facing the way it goes; on the ground, hopping its steps.
      at(far, here);
      at(far + 0.3, ahead).sub(at(far - 0.3, behind));
      const { z, up } = ground(far);
      const startle = since >= 0 && since < STARTLE[1] ? STARTLE[0] * 4 * (since / STARTLE[1]) * (1 - since / STARTLE[1]) : 0;
      mount.position.set(here.x, here.y, z + startle);
      mount.rotation.set(0, 0, Math.atan2(ahead.x, -ahead.y));
      middle.set(here.x, here.y, z + startle + 0.7);

      // The legs patter, faster as it goes faster: a step is half a stride.
      steps += dt * (PATTER[0] + PATTER[1] * speed * (1 + hurry));
      const stride = steps * Math.PI;
      legR.rotation.set(SWING * DEG * Math.cos(stride), 0, 0);
      legL.rotation.set(-SWING * DEG * Math.cos(stride), 0, 0);
      // Its head bobs with its steps, as a hurrying chicken's does.
      head.rotation.set(6 * DEG * Math.sin(2 * stride), 0, 0);

      // The wings: a few quick beats now and then, every hop up a block, and when clicked (as the game flaps them: out from the body, and back).
      const k = Math.floor(t / BURSTS[0]);
      const burst = clamp01((t - k * BURSTS[0] - 0.5 - 0.2 * Math.sin(k * 2.1)) / BURSTS[1]);
      const flapping = Math.max(0.75 * Math.sin(Math.PI * burst), up, since >= 0 && since < 0.7 ? 1 - smooth((since - 0.45) / 0.25) : 0);
      const wing = (Math.sin(t * BEAT * 2 * Math.PI) + 1) * flapping;
      wingR.rotation.set(0, wing, 0);
      wingL.rotation.set(0, -wing, 0);

      // The rider, jolted down into its seat at each footfall, leaning into the run; its
      // arms out in front, bobbing as a zombie's do, and with each jolt (thrown up when clicked).
      const jolt = Math.abs(Math.sin(stride));
      rider.position.set(seat.x, seat.y, seat.z - BOUNCE * PX * jolt);
      rider.rotation.set((5 + 2 * jolt) * DEG, 0, 0);
      riderHead.rotation.set((4 - 6 * jolt) * DEG, 0, 0);
      const flail = since >= 0 && since < 0.8 ? -25 * Math.sin((Math.PI * since) / 0.8) : 0;
      [armR, armL].forEach((arm, i) => {
        const bob = 3 * Math.sin(time * 1.34 + i) - 4 * jolt + flail;
        arm.quaternion.copy(arms[i]).multiply(turn.setFromAxisAngle(X, bob * DEG));
      });

      // Steve sees it come out of the reeds and shouts it, to the lens; then his eyes and
      // his head go after it, while the lens sees it. (It goes by behind him: he turns from
      // the lens to watch it go, and lets it go once it is round at his back.)
      if (!shouted && far >= run.seen[0]) {
        shouted = true;
        watching = time;
        shout(time, SHOUT);
      }
      if (shouted && far <= run.seen[1] + 0.5) {
        riderHead.updateWorldMatrix(true, false);
        watch.shouting = time - watching < SHOUT;
        watch.at = time - watching < STARTLED ? ctx.stage.camera.position : eyes.setFromMatrixPosition(riderHead.matrixWorld);
        jockey.watch = watch;
      } else jockey.watch = null;
    },
  };
  return jockey;
}
