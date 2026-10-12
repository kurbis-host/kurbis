/**
 * The page runs on two clocks. Scroll position, counted in screens, drives the camera
 * and the story; real time drives life and reactions. Every animated value is a
 * function of both, and comes in three kinds:
 *
 *   scrubbed    a function of the scroll position (the camera, the pumpkin's journey)
 *   triggered   starts when the scroll passes a point, then plays in real time (a pop)
 *   live        runs in real time whatever the scroll does (breathing, loops, reactions)
 *
 * The film is written as parts, each a function that puts its things where one frame
 * has them. A triggered piece keeps its own clock (a Trigger): it runs forward while
 * the scroll is past its start and is taken back when the scroll returns before it, so
 * what popped in pops out again.
 */

import { Vector3, type Object3D } from "three";

export interface Frame {
  /** Scroll position, in screens. */
  s: number;
  /** Seconds of real time the film has run. */
  time: number;
  /** Seconds since the frame before. */
  dt: number;
  /** The scroll did not get here by scrolling: the page opened part way down, or a shortcut was taken. */
  jumped: boolean;
}

/** How much faster than it played a triggered piece is taken back. */
const REWIND = 2.5;
/** A clock that has long run out (the page was opened, or jumped, past its start). */
const LONG = 1e5;

export class Trigger {
  /** Seconds since it began; 0 while it has not. */
  t = 0;

  /** `length` is how long what it plays takes, which is how far it is taken back. */
  constructor(
    readonly start: number,
    readonly length = 1,
  ) {}

  step(s: number, dt: number) {
    if (s >= this.start) this.t += dt;
    else if (this.t > 0) this.t = Math.max(Math.min(this.t, this.length) - dt * REWIND, 0);
  }

  /** As if the scroll had been here for ever. */
  settle(s: number) {
    this.t = s >= this.start ? LONG : 0;
  }
}

interface Part {
  s0: number;
  s1: number;
  fn: (frame: Frame) => void;
  ran: boolean;
}

export class Timeline {
  private readonly parts: Part[] = [];
  private readonly triggers: Trigger[] = [];
  readonly frame: Frame = { s: 0, time: 0, dt: 0, jumped: true };
  private settled = true;

  /**
   * Run `fn` on every frame whose scroll position lies in s0..s1, and once more as the
   * scroll leaves, so it can put its things away. (Give the stretch over which its
   * things can be seen or heard of, generously.)
   */
  during(s0: number, s1: number, fn: (frame: Frame) => void) {
    this.parts.push({ s0, s1, fn, ran: false });
  }

  always(fn: (frame: Frame) => void) {
    this.during(-Infinity, Infinity, fn);
  }

  trigger(start: number, length = 1): Trigger {
    const trigger = new Trigger(start, length);
    this.triggers.push(trigger);
    return trigger;
  }

  /** The scroll jumped (the page opened part way down, a shortcut was taken): nothing plays catch-up. */
  settle(s: number) {
    for (const trigger of this.triggers) trigger.settle(s);
    this.settled = true;
  }

  update(s: number, time: number, dt: number) {
    const { frame } = this;
    frame.s = s;
    frame.time = time;
    frame.dt = dt;
    frame.jumped = this.settled;
    this.settled = false;
    for (const trigger of this.triggers) trigger.step(s, dt);
    for (const part of this.parts) {
      const inside = s >= part.s0 && s <= part.s1;
      if (inside || part.ran) part.fn(frame);
      part.ran = inside;
    }
    // A part that has never run still owes its things their opening state.
    if (!this.opened) {
      this.opened = true;
      for (const part of this.parts) if (!part.ran) part.fn(frame);
    }
  }

  private opened = false;
}

// -- things ------------------------------------------------------------------------
const BASES = new WeakMap<Object3D, Vector3>();

/** A thing's size as the art built it. */
export function baseOf(object: Object3D): Vector3 {
  let base = BASES.get(object);
  if (!base) {
    base = object.scale.clone();
    BASES.set(object, base);
  }
  return base;
}

/** Scale a thing from the size it was built at. What has no size is not drawn at all. */
export function grow(object: Object3D, k: number) {
  const base = baseOf(object);
  object.visible = k > 1e-4;
  if (object.visible) object.scale.set(base.x * k, base.y * k, base.z * k);
}

export function grow3(object: Object3D, x: number, y: number, z: number) {
  const base = baseOf(object);
  object.visible = Math.abs(x * y * z) > 1e-9;
  if (object.visible) object.scale.set(base.x * x, base.y * y, base.z * z);
}

/** How much of a thing that fades is there (its material reads this), 0..1. */
export function fade(object: Object3D, alpha: number) {
  object.userData.tint = alpha;
  object.visible = alpha > 1e-3;
}
