/** Things that take turns: whose turn it is, and since when. */

import { clamp01 } from "./easing";

/** Whose turn it is and since when; taking turns of `each` seconds while `on`. */
export class Round {
  which = 0;
  since = 0;
  running = false;
  /** Held on the one the visitor points at. */
  held = false;

  constructor(
    private readonly count: number,
    private readonly each: number,
  ) {}

  /** Returns true on the frame a new turn begins. */
  step(on: boolean, time: number): boolean {
    if (!on) {
      this.running = false;
      return false;
    }
    if (!this.running) {
      this.running = true;
      this.which = 0;
      this.since = time;
      return true;
    }
    if (this.held) this.since = Math.max(this.since, time - this.each + 0.5);
    if (time - this.since < this.each) return false;
    this.which = (this.which + 1) % this.count;
    this.since = time;
    return true;
  }

  /** Give the word to `which` now; the round goes on from there. */
  take(which: number, time: number) {
    this.running = true;
    this.which = which;
    this.since = time;
  }

  /** How much `which` shows: it fades in for its turn and out before the next. */
  showing(which: number, time: number): number {
    if (!this.running || this.which !== which) return 0;
    const age = time - this.since;
    return clamp01(age / 0.15) * (1 - clamp01((age - this.each + 0.25) / 0.2));
  }
}
