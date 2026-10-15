/** The film's two clocks: what a triggered piece does when the scroll passes it, returns before it, or jumps. */

import { describe, expect, it } from "vitest";
import { POP, UNPOP, cycle, linear } from "../../../lib/landing/anim/easing";
import { Timeline } from "../../../lib/landing/anim/timeline";
import { Track } from "../../../lib/landing/anim/track";
import { Round } from "../../../lib/landing/anim/round";

describe("the timeline", () => {
  it("runs a triggered piece in real time once the scroll has passed its start", () => {
    const tl = new Timeline();
    const pop = tl.trigger(1.0, 0.3);
    tl.update(0.9, 0, 0.1);
    expect(pop.t).toBe(0);
    tl.update(1.0, 0.1, 0.1);
    tl.update(1.0, 0.2, 0.1);
    expect(pop.t).toBeCloseTo(0.2);
    // It keeps running however far the scroll goes on.
    tl.update(4.0, 10.2, 10);
    expect(pop.t).toBeCloseTo(10.2);
  });

  it("takes a triggered piece back when the scroll returns before it, faster than it played", () => {
    const tl = new Timeline();
    const pop = tl.trigger(1.0, 0.3);
    tl.update(1.2, 0, 5); // long past
    tl.update(0.9, 5.05, 0.05);
    // Back from the end of what it plays, not from five seconds in.
    expect(pop.t).toBeGreaterThan(0);
    expect(pop.t).toBeLessThan(0.3);
    tl.update(0.9, 5.4, 0.35);
    expect(pop.t).toBe(0);
    // And it plays again the next time.
    tl.update(1.1, 5.5, 0.1);
    expect(pop.t).toBeCloseTo(0.1);
  });

  it("plays no catch-up after a jump", () => {
    const tl = new Timeline();
    const before = tl.trigger(1.0, 0.3);
    const after = tl.trigger(5.0, 0.3);
    tl.settle(3.0);
    expect(POP(before.t / 0.3)).toBe(1);
    expect(after.t).toBe(0);
    tl.update(3.0, 0, 0.016);
    expect(tl.frame.jumped).toBe(true);
    tl.update(3.0, 0.016, 0.016);
    expect(tl.frame.jumped).toBe(false);
  });

  it("runs a part inside its stretch of scroll, once more as the scroll leaves, and once to begin with", () => {
    const tl = new Timeline();
    const seen: number[] = [];
    tl.during(2, 3, ({ s }) => seen.push(s));
    tl.update(0, 0, 0.016); // its things still need their opening state
    tl.update(1, 0.016, 0.016);
    tl.update(2.5, 0.032, 0.016);
    tl.update(3.5, 0.048, 0.016); // leaving: it puts its things away
    tl.update(4, 0.064, 0.016);
    expect(seen).toEqual([0, 2.5, 3.5]);
  });

  it("runs every part once where the scroll jumps to, so a part left past its end puts its things as that place has them", () => {
    const tl = new Timeline();
    // A sign that is gone at the end of the page, as the pond's is once the camera is home.
    let sign = true;
    tl.during(6.7, 6.9, ({ s }) => void (sign = s < 6.78));
    tl.update(1.3, 0, 0.016);
    for (const s of [6.75, 6.8, 7.5]) tl.update(s, 0.1, 0.016);
    expect(sign).toBe(false);
    // Round to the sky, past the end: the scroll never passed the sign's stretch on the way.
    tl.settle(0);
    tl.update(0, 0.2, 0.016);
    expect(sign).toBe(true);
  });
});

describe("curves", () => {
  it("never overshoots between keys, and rests where two keys agree", () => {
    const track = new Track([[0, 0], [1, 1], [2, 1], [3, 5]]);
    for (let s = 0; s <= 3; s += 0.01) {
      const value = track.value(s);
      expect(value).toBeGreaterThanOrEqual(-1e-9);
      expect(value).toBeLessThanOrEqual(5 + 1e-9);
      if (s > 1 && s < 2) expect(value).toBeCloseTo(1, 9);
    }
    expect(track.value(-1)).toBe(0);
    expect(track.value(9)).toBe(5);
    expect(track.value(track.when(3))).toBeCloseTo(3, 4);
  });

  it("pops in from nothing and out to nothing", () => {
    expect(POP(-1)).toBe(0);
    expect(POP(0)).toBe(0);
    expect(POP(0.55)).toBeGreaterThan(1);
    expect(POP(1)).toBe(1);
    expect(UNPOP(0)).toBe(1);
    expect(UNPOP(1)).toBe(0);
    expect(linear([[0, 2], [1, 4]])(0.25)).toBeCloseTo(2.5);
  });

  it("loops round from its last key to its first", () => {
    const loop = cycle(2, [[0, [0]], [1, [10]]]);
    const out: number[] = [];
    expect(loop(0, out)[0]).toBeCloseTo(0);
    expect(loop(1, out)[0]).toBeCloseTo(10);
    expect(loop(2, out)[0]).toBeCloseTo(0);
    expect(loop(4.5, out)[0]).toBeCloseTo(loop(0.5, out)[0]);
  });
});

describe("taking turns", () => {
  it("gives each its turn while the camera rests there, and none when it does not", () => {
    const round = new Round(3, 2);
    expect(round.step(false, 0)).toBe(false);
    expect(round.showing(0, 0)).toBe(0);
    expect(round.step(true, 1)).toBe(true); // the first has the word at once
    expect(round.which).toBe(0);
    expect(round.showing(0, 1.5)).toBe(1);
    expect(round.showing(1, 1.5)).toBe(0);
    expect(round.step(true, 2.9)).toBe(false);
    expect(round.step(true, 3.0)).toBe(true);
    expect(round.which).toBe(1);
    round.step(true, 5.1);
    round.step(true, 7.2);
    expect(round.which).toBe(0); // and round again
    round.step(false, 8);
    expect(round.showing(0, 8)).toBe(0);
  });

  it("gives the word at once to what a visitor points at, and goes on from there", () => {
    const round = new Round(6, 2.3);
    round.step(true, 0);
    round.take(4, 1);
    expect(round.showing(4, 1.5)).toBe(1);
    expect(round.step(true, 3.2)).toBe(false);
    expect(round.step(true, 3.31)).toBe(true);
    expect(round.which).toBe(5);
    // Held on what is pointed at for as long as it is.
    round.held = true;
    for (let time = 3.4; time < 20; time += 0.1) round.step(true, time);
    expect(round.which).toBe(5);
    expect(round.showing(5, 20)).toBeGreaterThan(0.9);
  });
});
