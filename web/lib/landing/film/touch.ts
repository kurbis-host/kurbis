/**
 * The visitor's hand: what the pointer is over, and what that does. Things that can be
 * touched are boxes in the world, each open for a stretch of the scroll. The interface's
 * buttons come first; then the nearest box along the line of sight.
 *
 * A tap is a hover and a click in one, and what it pointed at stays pointed at for a moment.
 */

import { Box3, Ray, Vector2, Vector3, type PerspectiveCamera } from "three";
import type { Frame } from "../anim/timeline";
import type { Card } from "../engine/hud";
import type { Interface } from "./interface";

export interface Target {
  name: string;
  /** The middle of the box, and half its size (it may move: a thing that walks). */
  at: Vector3 | (() => Vector3);
  half: Vector3;
  /** The stretch of scroll over which it can be touched. */
  seen: readonly [number, number];
  /** A tooltip while it is pointed at. */
  tip?: () => Card | null;
  /**
   * How it hangs: beside its thing (the default), centred under it, or at an offset in
   * interface pixels with this point of the card (0..1 across and up) there.
   */
  hang?: "beside" | "under" | { offset: readonly [number, number]; anchor: readonly [number, number] };
  enter?(time: number): void;
  leave?(time: number): void;
  click?(time: number, hit: Vector3): void;
  /** The pointer over it: a pickaxe where there is something to dig up, else a hand. */
  pick?: boolean;
}

/** Seconds a tapped thing stays pointed at. */
const LINGER = 2.6;

export class Touch {
  private readonly targets: Target[] = [];
  private readonly buttons = new Map<string, (time: number) => void>();
  /** Where the pointer is on the canvas, as a share of its width and height from the top left; null when it is away. */
  pointer: Vector2 | null = null;
  private tapped = -Infinity;
  private byTouch = false;
  hovered: Target | null = null;
  /** The interface's button under the pointer, if any. */
  pressing: string | null = null;
  /** What the pointer should look like, for the page to set. */
  cursor: "" | "pointer" | "pick" = "";
  /** How wide the tooltip has faded in. */
  private shown = 0;
  private tipFor: Target | null = null;

  private readonly ray = new Ray();
  private readonly box = new Box3();
  private readonly hit = new Vector3();
  private readonly ndc = new Vector3();

  add(target: Target) {
    this.targets.push(target);
  }

  /** Something the interface's button of this name does. */
  button(name: string, press: (time: number) => void) {
    this.buttons.set(name, press);
  }

  /** The pointer moved to (x, y), shares of the canvas from its top left; or left it. */
  move(x: number | null, y = 0, touch = false) {
    if (x === null) {
      // A finger that lifts has left, but what it tapped stays pointed at for a moment.
      if (!this.byTouch) this.pointer = null;
      return;
    }
    this.pointer = (this.pointer ?? new Vector2()).set(x, y);
    this.byTouch = touch;
  }

  private under(s: number, camera: PerspectiveCamera): Target | null {
    if (!this.pointer) return null;
    this.ray.origin.setFromMatrixPosition(camera.matrixWorld);
    this.ray.direction.set(this.pointer.x * 2 - 1, 1 - this.pointer.y * 2, 0.5).unproject(camera).sub(this.ray.origin).normalize();
    let best: Target | null = null;
    let nearest = Infinity;
    for (const target of this.targets) {
      if (s < target.seen[0] || s >= target.seen[1]) continue;
      const at = typeof target.at === "function" ? target.at() : target.at;
      this.box.min.copy(at).sub(target.half);
      this.box.max.copy(at).add(target.half);
      const point = this.ray.intersectBox(this.box, scratch);
      if (!point) continue;
      const far = point.distanceToSquared(this.ray.origin);
      if (far < nearest) {
        nearest = far;
        best = target;
        this.hit.copy(point);
      }
    }
    return best;
  }

  /** A press at the pointer's place (a click, or a tap, which also points). */
  press(time: number, s: number, camera: PerspectiveCamera, ui: Interface | null, ratio: number, size: readonly [number, number]): boolean {
    if (!this.pointer) return false;
    const button = ui?.hud.under(this.pointer.x * size[0], this.pointer.y * size[1], ratio) ?? null;
    if (button) {
      this.buttons.get(button)?.(time);
      return true;
    }
    const target = this.under(s, camera);
    if (this.byTouch) this.tapped = time;
    if (!target) return false;
    this.point(target, time);
    target.click?.(time, this.hit);
    return true;
  }

  private point(target: Target | null, time: number) {
    if (target === this.hovered) return;
    this.hovered?.leave?.(time);
    this.hovered = target;
    target?.enter?.(time);
  }

  /** Each frame: what is pointed at now (the camera moves under a still pointer), and its tooltip. */
  update({ s, time, dt }: Frame, camera: PerspectiveCamera, ui: Interface | null, ratio: number, size: readonly [number, number]) {
    this.pressing = this.pointer && ui ? ui.hud.under(this.pointer.x * size[0], this.pointer.y * size[1], ratio) : null;
    const lingering = this.byTouch && time - this.tapped < LINGER && this.hovered && s >= this.hovered.seen[0] && s < this.hovered.seen[1];
    if (!lingering) this.point(this.pressing || (this.byTouch && time - this.tapped >= LINGER) ? null : this.under(s, camera), time);
    this.cursor = this.pressing ? "pointer" : this.hovered ? (this.hovered.pick ? "pick" : "pointer") : "";

    // The tooltip of what is pointed at, hung where its thing is on screen.
    if (this.hovered?.tip) this.tipFor = this.hovered;
    const wanted = this.hovered === this.tipFor && this.tipFor !== null;
    this.shown = wanted ? Math.min(this.shown + dt / 0.12, 1) : Math.max(this.shown - dt / 0.15, 0);
    const owner = this.tipFor;
    const card = this.shown > 0 && owner?.tip ? owner.tip() : null;
    if (!ui || !owner || !card) return;
    this.ndc.copy(typeof owner.at === "function" ? owner.at() : owner.at).project(camera);
    const x = (this.ndc.x * 0.5 + 0.5) * ui.hud.width;
    const y = (this.ndc.y * 0.5 + 0.5) * ui.hud.height;
    const right = x > ui.hud.width * 0.62;
    const { hang } = owner;
    if (hang === "under") ui.tips.push({ card, at: [x, y], anchor: [0.5, 1], alpha: this.shown });
    else if (typeof hang === "object") ui.tips.push({ card, at: [x + hang.offset[0], y + hang.offset[1]], anchor: hang.anchor, alpha: this.shown });
    else ui.tips.push({ card, at: [x + (right ? -10 : 12), y + 8], anchor: [right ? 1 : 0, 0], alpha: this.shown });
  }
}

const scratch = new Vector3();
