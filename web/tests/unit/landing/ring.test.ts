/** The page as a ring: where the scroll is on it for each place on the page, where the page comes round, and the cover over it there. */

import { describe, expect, it } from "vitest";
import { AGAIN, HOLD, LEAD, ROLL } from "../../../lib/landing/film/credits";
import { FILM, Ring } from "../../../lib/landing/film/ring";

const ring = new Ring(FILM);
/** Places on the page, every hundredth of a screen from its top to its foot. */
const places = Array.from({ length: Math.round(ring.length * 100) + 1 }, (_, k) => k / 100);

describe("the page's ring", () => {
  it("is the film, the credits, the tail and the lead-in once round, with a copy of the tail over it and of the lead-in under it", () => {
    expect(ring.rolled).toBeCloseTo(FILM + ROLL, 9);
    expect(ring.last).toBeCloseTo(FILM + ROLL + AGAIN, 9);
    expect(ring.round).toBeCloseTo(FILM + ROLL + AGAIN + LEAD, 9);
    expect(ring.length).toBeCloseTo(AGAIN + ring.round + LEAD, 9);
    // (Once round is longer than the page was by the lead-in alone.)
    expect(ring.round - (FILM + ROLL + AGAIN)).toBeCloseTo(LEAD, 9);
  });

  it("opens at the sky, with the end of the credits at the page's top and the sky again at its foot", () => {
    expect(ring.page(0)).toBeCloseTo(AGAIN + LEAD, 9);
    expect(ring.at(ring.page(0))).toBeCloseTo(0, 9);
    expect(ring.at(0)).toBeCloseTo(ring.rolled, 9);
    expect(ring.at(ring.length)).toBeCloseTo(0, 9);
  });

  it("has a place on the page for every scroll position, and finds it there", () => {
    for (let s = -LEAD; s < ring.last; s += 0.01) expect(ring.at(ring.page(s))).toBeCloseTo(s, 9);
    // The stops exactly, not a hair before them (a toast arrives as the scroll reaches its place).
    for (const s of [0, 0.12, 3.4, FILM, ring.rolled]) expect(ring.at(ring.page(s))).toBeGreaterThanOrEqual(s);
  });

  it("comes round where the tail ends and the lead-in begins, under the cover", () => {
    const seam = ring.page(-LEAD);
    expect(ring.at(seam)).toBeCloseTo(-LEAD, 9);
    expect(ring.at(seam - 1e-6)).toBeCloseTo(ring.last, 5);
    expect(ring.at(seam + ring.round)).toBeCloseTo(-LEAD, 9);
    expect(ring.at(seam + ring.round - 1e-6)).toBeCloseTo(ring.last, 5);
    // Nowhere else does the scroll position leap.
    for (let k = 1; k < places.length; k += 1) {
      const step = Math.abs(ring.at(places[k]) - ring.at(places[k - 1]));
      const across = [seam, seam + ring.round].some((at) => places[k - 1] < at && places[k] >= at - 1e-9);
      if (!across) expect(step).toBeCloseTo(0.01, 6);
    }
  });

  it("moves a place in a copy to the same place in the ring proper, and leaves the rest where it is", () => {
    for (const page of places) {
      const home = ring.home(page);
      expect(home).toBeGreaterThanOrEqual(ring.top - 1e-9);
      expect(home).toBeLessThanOrEqual(ring.top + ring.round + 1e-9);
      // (The seam's two sides are one place.)
      const [a, b] = [ring.at(home), ring.at(page)];
      expect(Math.min(Math.abs(a - b), ring.round - Math.abs(a - b))).toBeCloseTo(0, 9);
      if (page >= ring.top && page <= ring.top + ring.round) expect(home).toBe(page);
    }
    // From the page's top, to the end of the credits in the ring proper; from its foot, to the sky.
    expect(ring.home(0)).toBeCloseTo(ring.page(ring.rolled), 9);
    expect(ring.home(ring.length)).toBeCloseTo(ring.page(0), 9);
  });

  it("covers the frame over the tail and lifts it over the lead-in, all of it either side of the seam", () => {
    expect(ring.cover(ring.rolled)).toBe(0);
    expect(ring.cover(0)).toBe(0);
    for (const s of [0.5, 3.4, FILM, ring.rolled - 0.01]) expect(ring.cover(s)).toBe(0);
    for (const s of [ring.last - HOLD, ring.last - 1e-6, -LEAD, -LEAD + HOLD]) expect(ring.cover(s)).toBe(1);
    // Rising over the tail, and lifting over the lead-in, never back.
    for (let s = ring.rolled; s < ring.last - 0.01; s += 0.01) expect(ring.cover(s + 0.01)).toBeGreaterThanOrEqual(ring.cover(s));
    for (let s = -LEAD; s < -0.01; s += 0.01) expect(ring.cover(s + 0.01)).toBeLessThanOrEqual(ring.cover(s));
    expect(ring.cover(ring.rolled + AGAIN / 2)).toBeGreaterThan(0.25);
    expect(ring.cover(-LEAD / 2)).toBeGreaterThan(0.25);
    // Not all of it for long: a little more than the hold either side of the seam.
    expect(ring.cover(ring.last - HOLD - 0.03)).toBeLessThan(0.85);
    expect(ring.cover(-LEAD + HOLD + 0.03)).toBeLessThan(0.85);
  });
});
