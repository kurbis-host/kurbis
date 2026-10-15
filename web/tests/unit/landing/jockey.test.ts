/** Now and then a chicken jockey runs by the pond: in about one visit in four, once, after a few seconds' stay there; never for a visitor who scrolls straight on. */

import { describe, expect, it } from "vitest";
import { seeded } from "../../../lib/landing/anim/easing";
import { JockeyTimer } from "../../../lib/landing/film/jockey";

/** A visit whose throws come out as given: the first says whether it comes, the second how long it waits. */
const throws = (...values: number[]) => () => values.shift() ?? 0.5;

/** The visitor's stays as [from, until] seconds; the moments (60 a second) at which it is running, and how far into its run. */
const visit = (timer: JockeyTimer, stays: [number, number][], until: number) => {
  const running: [number, number][] = [];
  for (let frame = 0; frame <= until * 60; frame += 1) {
    const time = frame / 60;
    const t = timer.at(stays.some(([a, b]) => time >= a && time < b), time);
    if (t !== null) running.push([time, t]);
  }
  return running;
};

describe("a chicken jockey", () => {
  it("comes in about one visit in four", () => {
    const visits = Array.from({ length: 4000 }, (_, k) => new JockeyTimer(seeded(k + 1)));
    const share = visits.filter((timer) => timer.comes).length / visits.length;
    expect(share).toBeGreaterThan(0.22);
    expect(share).toBeLessThan(0.28);
    for (const timer of visits) expect(timer.wait >= 6 && timer.wait <= 10).toBe(true);
  });

  it("runs once a visit, after six to ten seconds at the pond", () => {
    for (const second of [0, 0.5, 0.999]) {
      const timer = new JockeyTimer(throws(0.1, second));
      const running = visit(timer, [[2, 40]], 40);
      expect(running[0][0] - 2).toBeCloseTo(6 + 4 * second, 1);
      expect(running[0][1]).toBe(0);
      // It runs on until its run is over, and then it does not come again.
      const ends = running.findIndex(([time], i) => i > 0 && time - running[i - 1][0] > 0.02);
      expect(ends).toBe(-1);
      timer.end();
      expect(visit(timer, [[0, 60]], 60)).toEqual([]);
    }
  });

  it("does not come in the other visits", () => {
    expect(visit(new JockeyTimer(throws(0.3, 0)), [[0, 60]], 60)).toEqual([]);
  });

  it("is never seen by a visitor who scrolls straight past, nor by one who stops a while and goes on", () => {
    const timer = new JockeyTimer(throws(0.1, 0));
    expect(visit(timer, [[1, 2.5], [10, 15.9]], 30)).toEqual([]);
  });

  it("counts each stay afresh: one that ends before it comes does not use it up", () => {
    const timer = new JockeyTimer(throws(0.1, 0.5)); // (it waits eight seconds)
    const running = visit(timer, [[0, 5], [20, 40]], 40);
    expect(running[0][0]).toBeCloseTo(28, 1);
  });

  it("ends when the visitor scrolls away while it runs, and does not come again", () => {
    const timer = new JockeyTimer(throws(0.1, 0));
    const running = visit(timer, [[0, 8], [9, 60]], 60);
    expect(running.length).toBeGreaterThan(0);
    expect(Math.max(...running.map(([time]) => time))).toBeLessThan(8);
  });
});
