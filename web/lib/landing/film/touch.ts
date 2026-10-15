/**
 * The visitor's hand: what the pointer is over, and what that does. Things that can be
 * touched are boxes in the world, each open for a stretch of the scroll. The interface's
 * buttons come first; then the nearest box along the line of sight.
 *
 * A tap is a hover and a click in one, and what it pointed at stays pointed at for a moment.
 * A tooltip with a link on it can be reached: it stays while the pointer goes onto it, and a
 * tap keeps it until the next tap somewhere else.
 */

import { Box3, Ray, Vector2, Vector3, type PerspectiveCamera } from "three";
import type { Frame } from "../anim/timeline";
import type { Card } from "../engine/hud";
import type { Interface, Tip } from "./interface";

export interface Target {
  name: string;
  /** The middle of the box, and half its size (it may move: a thing that walks). */
  at: Vector3 | (() => Vector3);
  half: Vector3;
  /** The stretch of scroll over which it can be touched. */
  seen: readonly [number, number];
  /** Whether it is there at all (a thing that comes and goes): while it is not, the pointer passes through. */
  here?: () => boolean;
  /**
   * Whether a point of the frame (shares of it from the top left) is on it as it is drawn:
   * for a thing that lies across the frame at a slant, which its box would take in with
   * everything round it. The box still says how near it is.
   */
  over?: (x: number, y: number) => boolean;
  /** A tooltip while it is pointed at, and a link on it. */
  tip?: () => Card | null;
  link?: () => Tip["link"];
  /**
   * How it hangs: beside its thing (the default), centred under it, at an offset in
   * interface pixels with this point of the card (0..1 across and up) there, or where the
   * thing says for that card (counted as an offset is, from the frame's bottom left).
   */
  hang?: "beside" | "under" | { offset: readonly [number, number]; anchor: readonly [number, number] } | ((card: Card) => { at: [number, number]; anchor: readonly [number, number] });
  enter?(time: number): void;
  leave?(time: number): void;
  click?(time: number, hit: Vector3): void;
  /** The pointer over it: a pickaxe where there is something to dig up, a hand where a click does something. */
  pick?: boolean;
}

/** Seconds a tapped thing stays pointed at. */
const LINGER = 2.6;
/** Seconds a tooltip with a link stays after the pointer has left it and its thing: time to cross from the one to the other. */
const CROSS = 0.4;

export class Touch {
  /** Everything that can be touched, as it was added (the debug keys outline what is open: ./debug.ts). */
  readonly targets: Target[] = [];
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
  /** A screen of the game's is over the world (the death screen: ./chat.ts): only the interface's buttons can be pressed. */
  screen = false;
  /** How wide the tooltip has faded in. */
  private shown = 0;
  private tipFor: Target | null = null;
  /** The tooltip as it was last handed to the interface (which says where it drew it), and when the pointer left it and its thing. */
  private tip: Tip | null = null;
  private away: number | null = null;

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
    if (!this.pointer || this.screen) return null;
    this.ray.origin.setFromMatrixPosition(camera.matrixWorld);
    this.ray.direction.set(this.pointer.x * 2 - 1, 1 - this.pointer.y * 2, 0.5).unproject(camera).sub(this.ray.origin).normalize();
    let best: Target | null = null;
    let nearest = Infinity;
    for (const target of this.targets) {
      if (!open(target, s)) continue;
      const at = typeof target.at === "function" ? target.at() : target.at;
      this.box.min.copy(at).sub(target.half);
      this.box.max.copy(at).add(target.half);
      const point = this.ray.intersectBox(this.box, scratch);
      if (!point || (target.over && !target.over(this.pointer.x, this.pointer.y))) continue;
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
    // (A tooltip that stays lies over the world: a press on it goes no further.)
    if (this.reached(ui)) return true;
    const target = this.under(s, camera);
    if (this.byTouch) this.tapped = time;
    if (!target) {
      // A tap anywhere else puts away a tooltip that stays.
      if (this.byTouch && this.hovered?.link) this.point(null, time);
      return false;
    }
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
    // (A tooltip with a link, tapped, stays until the next tap.)
    const held = this.hovered?.link && open(this.hovered, s) ? this.hovered : null;
    const lingering = this.byTouch && this.hovered && open(this.hovered, s) && (held || time - this.tapped < LINGER);
    if (!lingering) {
      let next = this.pressing || (this.byTouch && time - this.tapped >= LINGER) ? null : this.under(s, camera);
      // A tooltip with a link stays while the pointer is on it, and for a moment after it has left both it and its thing.
      if (held && next !== held) {
        if (this.reached(ui)) this.away = null;
        else this.away ??= time;
        if (this.away === null || time - this.away < CROSS) next = held;
      } else this.away = null;
      this.point(next, time);
    }
    if (this.hovered && !open(this.hovered, s)) this.point(null, time); // (it went the moment it was pointed at)
    this.cursor = this.pressing ? "pointer" : this.hovered ? (this.hovered.pick ? "pick" : this.hovered.click ? "pointer" : "") : "";

    // The tooltip of what is pointed at, hung where its thing is on screen.
    if (this.hovered?.tip) this.tipFor = this.hovered;
    const wanted = this.hovered === this.tipFor && this.tipFor !== null;
    this.shown = wanted ? Math.min(this.shown + dt / 0.12, 1) : Math.max(this.shown - dt / 0.15, 0);
    const owner = this.tipFor;
    const card = this.shown > 0 && owner?.tip ? owner.tip() : null;
    this.tip = null;
    if (!ui || !owner || !card) return;
    this.ndc.copy(typeof owner.at === "function" ? owner.at() : owner.at).project(camera);
    const x = (this.ndc.x * 0.5 + 0.5) * ui.hud.width;
    const y = (this.ndc.y * 0.5 + 0.5) * ui.hud.height;
    const right = x > ui.hud.width * 0.62;
    const { hang } = owner;
    const link = owner.link?.();
    if (hang === "under") this.tip = { card, at: [x, y], anchor: [0.5, 1], alpha: this.shown, link };
    else if (typeof hang === "function") this.tip = { card, ...hang(card), alpha: this.shown, link };
    else if (typeof hang === "object") this.tip = { card, at: [x + hang.offset[0], y + hang.offset[1]], anchor: hang.anchor, alpha: this.shown, link };
    else this.tip = { card, at: [x + (right ? -10 : 12), y + 8], anchor: [right ? 1 : 0, 0], alpha: this.shown, link };
    ui.tips.push(this.tip);
  }

  /** Whether the pointer is on the tooltip of what it points at, where that was drawn last (a tooltip with a link: the rest are out of the way). */
  private reached(ui: Interface | null): boolean {
    const drawn = this.tip?.link && this.tip.drawn;
    if (!ui || !this.pointer || !drawn || this.tipFor !== this.hovered) return false;
    const x = this.pointer.x * ui.hud.width;
    const y = (1 - this.pointer.y) * ui.hud.height;
    return x >= drawn[0] && x < drawn[2] && y >= drawn[1] && y < drawn[3];
  }
}

const scratch = new Vector3();

/** Whether a thing can be touched now: in its stretch of scroll, and there. */
export function open(target: Target, s: number): boolean {
  return s >= target.seen[0] && s < target.seen[1] && (target.here?.() ?? true);
}
