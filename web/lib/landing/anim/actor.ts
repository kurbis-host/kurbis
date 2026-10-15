/**
 * The pumpkin. Its journey is a list of scrubbed moves, each a function of its own
 * progress u in 0..1; the move that started last is the one playing. While a move
 * leaves it calm, real time adds life on top: breathing, blinks, a jump when clicked.
 */

import { Vector3, type Object3D } from "three";
import { clamp01, lerp, smooth } from "./easing";

export interface Pose {
  pos: Vector3;
  /** Degrees about the vertical. */
  yaw: number;
  /** Degrees of tumble about its own X, Y and Z. */
  tilt: [number, number, number];
  /** Below 1 it flattens (and widens to keep its volume), above 1 it stretches. */
  squash: number;
  /** A pose it rests in: it breathes, blinks and can be made to jump. */
  calm: boolean;
  /** Its size all round; 0 is gone. */
  grow: number;
}

export type MoveFn = (u: number, out: Pose) => void;

interface Move {
  s0: number;
  s1: number;
  fn: MoveFn;
}

const RAD = Math.PI / 180;
/** How much it swells with a breath, and how long a breath takes. */
const BREATH = [0.03, 2.6] as const;
/** Seconds a click-jump lasts. */
const JUMP = 0.55;

export function setPose(out: Pose, pos: Vector3, yaw = 0, squash = 1, calm = false): Pose {
  out.pos.copy(pos);
  out.yaw = yaw;
  out.tilt[0] = 0;
  out.tilt[1] = 0;
  out.tilt[2] = 0;
  out.squash = squash;
  out.calm = calm;
  out.grow = 1;
  return out;
}

export function newPose(): Pose {
  return { pos: new Vector3(), yaw: 0, tilt: [0, 0, 0], squash: 1, calm: false, grow: 1 };
}

export class Actor {
  private readonly moves: Move[] = [];
  private readonly faces: [number, string][] = [];
  private visible: [number, number | null] = [-Infinity, null];
  private readonly pose = newPose();
  private readonly blinks: number[] = [1.7];
  private jumpAt = -Infinity;
  private jumpHeight = 0.75;
  private jumpAnywhere = false;
  private acts: { t0: number; t1: number; fn: MoveFn }[] = [];
  /** A face worn over whatever the scroll says (asleep while the cave sleeps), or null. */
  mask: string | null = null;
  /** Degrees it turns on top of where it faces (toward the pointer, say). */
  glance = 0;

  constructor(
    readonly root: Object3D,
    private readonly spin: Object3D,
    private readonly bodies: ReadonlyMap<string, Object3D>,
    private readonly extras: readonly Object3D[] = [],
    private readonly size = 1,
  ) {
    spin.rotation.order = "ZYX"; // the art's XYZ
  }

  // scrubbed --------------------------------------------------------------------------
  move(s0: number, s1: number, fn: MoveFn): this {
    this.moves.push({ s0, s1, fn });
    this.moves.sort((a, b) => a.s0 - b.s0);
    return this;
  }

  /** Calm from s0 until the next move starts. */
  rest(s0: number, pos: Vector3, yaw = 0): this {
    const at = pos.clone();
    return this.move(s0, s0 + 1, (_u, out) => setPose(out, at, yaw, 1, true));
  }

  /**
   * Crouch, jump along a parabola from p0 to p1, land with a squash. `antic` and `land`
   * are the shares of the move spent crouching and landing.
   */
  hop(s0: number, s1: number, from: Vector3, to: Vector3, height: number, options: { yaw?: [number, number]; flips?: number; axis?: 0 | 1 | 2; antic?: number; land?: number } = {}): this {
    const p0 = from.clone();
    const p1 = to.clone();
    const [yaw0, yaw1] = options.yaw ?? [0, 0];
    const flips = options.flips ?? 0;
    const axis = options.axis ?? 0;
    const a = options.antic ?? 0.14;
    const b = options.land ?? 0.2;
    return this.move(s0, s1, (u, out) => {
      const yaw = lerp(yaw0, yaw1, smooth(u));
      if (a > 0 && u < a) {
        setPose(out, p0, yaw, 1 - 0.3 * Math.sin((Math.PI * 0.5 * u) / a));
        return;
      }
      if (b > 0 && u > 1 - b) {
        const v = (u - (1 - b)) / b;
        setPose(out, p1, yaw, 1 - 0.42 * Math.cos(Math.PI * 0.5 * v) ** 2 * (1 - v) + 0.08 * Math.sin(Math.PI * v));
        return;
      }
      const v = (u - a) / (1 - a - b);
      setPose(out, p0, yaw, 1 + 0.2 * Math.abs(Math.cos(Math.PI * v)));
      out.pos.lerp(p1, v);
      out.pos.z += 4 * height * v * (1 - v);
      out.tilt[axis] = 360 * flips * smooth(v);
    });
  }

  face(s: number, name: string): this {
    this.faces.push([s, name]);
    this.faces.sort((a, b) => a[0] - b[0]);
    return this;
  }

  appear(sIn: number, sOut: number | null = null): this {
    this.visible = [sIn, sOut];
    return this;
  }

  // live --------------------------------------------------------------------------------
  /**
   * It jumps on the spot (somebody clicked it), if it is at rest; or, `anywhere`, on top of
   * whatever the scroll has it doing (a hop where it sits in the cart).
   */
  jump(now: number, height = 0.75, anywhere = false) {
    if (now - this.jumpAt < JUMP) return;
    this.jumpAt = now;
    this.jumpHeight = height;
    this.jumpAnywhere = anywhere;
  }

  /**
   * Do something of its own for a while of real time, wherever the scroll has it
   * resting: `fn(u, pose)` gives the pose for progress u in 0..1.
   */
  act(t0: number, t1: number, fn: MoveFn): { t0: number; t1: number } {
    const act = { t0, t1, fn };
    this.acts.push(act);
    return act;
  }

  /** Forget whatever it was doing of its own (the scroll took it elsewhere). */
  settle() {
    this.acts.length = 0;
  }

  /** Whether it is busy with something of its own at this moment. */
  busy(now: number): boolean {
    return this.acts.some((act) => now >= act.t0 && now < act.t1);
  }

  // reading -----------------------------------------------------------------------------
  /** Where the scroll alone has it. */
  state(s: number, out: Pose = this.pose): Pose {
    let current = this.moves[0];
    for (const move of this.moves) if (s >= move.s0 - 1e-9) current = move;
    current.fn(clamp01((s - current.s0) / (current.s1 - current.s0)), out);
    return out;
  }

  position(s: number, out = new Vector3()): Vector3 {
    return out.copy(this.state(s, scratch).pos);
  }

  private blinking(now: number): boolean {
    const { blinks } = this;
    // Every few seconds, never quite regular.
    while (blinks[blinks.length - 1] < now) {
      const i = blinks.length;
      blinks.push(blinks[i - 1] + 2.3 + 2.4 * ((i * 0.618034) % 1));
    }
    for (let i = blinks.length - 1; i >= 0 && blinks[i] > now - 1; i -= 1) {
      if (now - blinks[i] >= 0 && now - blinks[i] < 0.13) return true;
    }
    return false;
  }

  /** Stand where scroll position s and the moment `now` (seconds) have it. */
  update(s: number, now: number) {
    const p = this.state(s);
    for (const act of this.acts) {
      if (p.calm && now >= act.t0 && now < act.t1) act.fn(Number.isFinite(act.t1) ? (now - act.t0) / (act.t1 - act.t0) : 0, p);
    }
    let { squash } = p;
    let lift = 0;
    let face = this.faces[0]?.[1] ?? "awake";
    for (const [at, name] of this.faces) if (at <= s + 1e-9) face = name;
    if (this.mask) face = this.mask;
    if (p.calm) {
      squash *= 1 + BREATH[0] * Math.sin((2 * Math.PI * now) / BREATH[1]);
      if (face === "awake" && this.bodies.has("blink") && this.blinking(now)) face = "blink";
    }
    const u = (now - this.jumpAt) / JUMP;
    if ((p.calm || this.jumpAnywhere) && u >= 0 && u < 1) {
      if (u < 0.16) {
        squash *= 1 - 0.26 * Math.sin((Math.PI * 0.5 * u) / 0.16);
      } else if (u < 0.8) {
        const v = (u - 0.16) / 0.64;
        lift = 4 * this.jumpHeight * v * (1 - v);
        squash *= 1 + 0.16 * Math.abs(Math.cos(Math.PI * v));
        if (this.bodies.has("whee")) face = "whee";
      } else {
        const v = (u - 0.8) / 0.2;
        squash *= 1 - 0.3 * (1 - v) * Math.cos(Math.PI * 0.5 * v);
      }
    }
    const wide = 1 / Math.sqrt(Math.max(squash, 1e-3));
    const here = s >= this.visible[0] - 1e-9 && (this.visible[1] === null || s < this.visible[1]);
    this.root.position.set(p.pos.x, p.pos.y, p.pos.z + lift);
    this.root.rotation.set(0, 0, (p.yaw + this.glance) * RAD);
    this.root.scale.set(this.size * wide * p.grow, this.size * wide * p.grow, this.size * squash * p.grow);
    this.spin.rotation.set(p.tilt[0] * RAD, p.tilt[1] * RAD, p.tilt[2] * RAD);
    for (const [name, body] of this.bodies) body.visible = here && name === face;
    for (const extra of this.extras) extra.visible = here;
  }
}

const scratch = newPose();
