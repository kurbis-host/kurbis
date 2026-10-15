/** Now and then the friends at the pond fish up a boot: never the first catch a visitor sees, soon after, and about one in four or five. */

import { describe, expect, it } from "vitest";
import { seeded } from "../../../lib/landing/anim/easing";
import { Junk } from "../../../lib/landing/film/life";

/** When the two friends strike, as the film has them: each every 9.6 s, half a loop apart. */
const strikes = (from: number, until: number) => {
  const all: number[] = [];
  for (const first of [6.6, 1.8]) for (let at = first; at < until; at += 9.6) if (at >= from) all.push(at);
  return all.sort((a, b) => a - b);
};

/** A stay at the pond from `from` to `until`: which catches were boots, and when. */
const stay = (junk: Junk, from: number, until: number) => {
  junk.at(true, from);
  return strikes(from, until).map((at) => ({ at, boot: junk.boot(at) }));
};

describe("a boot on the line", () => {
  it("is never the first catch, and the third, so it comes up within fifteen seconds of stopping", () => {
    for (let from = 0; from < 9.6; from += 0.4) {
      const catches = stay(new Junk(seeded(Math.round(from * 10))), from, from + 20);
      expect(catches.map(({ boot }) => boot).slice(0, 3)).toEqual([false, false, true]);
      expect(catches[2].at - from).toBeLessThan(15);
    }
  });

  it("comes up about one catch in four or five after that", () => {
    const catches = stay(new Junk(seeded(7)), 0, 600);
    const boots = catches.flatMap(({ boot }, i) => (boot ? [i] : []));
    const gaps = boots.slice(1).map((at, i) => at - boots[i]);
    expect(new Set(gaps)).toEqual(new Set([4, 5]));
  });

  it("does not count a catch struck before the visitor came", () => {
    const junk = new Junk(seeded(1));
    junk.at(true, 10);
    expect(junk.boot(6.6)).toBe(false);
    expect([11.4, 16.2, 21.0].map((at) => junk.boot(at))).toEqual([false, false, true]);
  });

  it("is counted afresh each time the visitor comes back, and never comes up while they are away", () => {
    const junk = new Junk(seeded(2));
    stay(junk, 0, 12); // (two catches, then they go on)
    junk.at(false, 12);
    expect(junk.boot(16.2)).toBe(false);
    expect(stay(junk, 30, 50).map(({ boot }) => boot).slice(0, 3)).toEqual([false, false, true]);
  });
});
