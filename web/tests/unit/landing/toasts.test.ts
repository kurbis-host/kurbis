/** Toasts take their corner one at a time, whether the scroll sends them or something that happened. */

import { describe, expect, it } from "vitest";
import { TRAVEL, Toasts } from "../../../lib/landing/film/toasts";

/** What shows at a moment, by card. */
const showing = (toasts: Toasts<string>, s: number, time: number) => toasts.update(s, time, false).map(([card]) => card);

describe("toasts", () => {
  const scrolled = () => new Toasts([{ card: "grass", from: 1.0, until: 99 }]);

  it("sends off the scrolled toast showing when an event's comes", () => {
    const toasts = scrolled();
    showing(toasts, 0.9, 0);
    expect(showing(toasts, 1.1, 0.1)).toEqual(["grass"]);
    expect(toasts.send("Getting Wood", () => "wood", 1.0, { from: 1.05, until: 1.46 })).toBe(true);
    expect(showing(toasts, 1.1, 1.0)).toEqual(["grass", "wood"]); // (the one going slides out as the other comes)
    expect(showing(toasts, 1.1, 1.0 + TRAVEL + 0.01)).toEqual(["wood"]);
  });

  it("tells an event once a visit, unless asked again", () => {
    const toasts = scrolled();
    showing(toasts, 1.2, 0);
    expect(toasts.send("Getting Wood", () => "wood", 0, { from: 1.05, until: 1.46 })).toBe(true);
    showing(toasts, 1.2, 3); // its time is up: it slides out
    showing(toasts, 1.2, 3.5);
    expect(toasts.send("Getting Wood", () => "wood", 10, { from: 1.05, until: 1.46 })).toBe(false);
    expect(showing(toasts, 1.2, 10.1)).toEqual([]);
    expect(toasts.send("Getting Wood", () => "wood", 10.1, { from: 1.05, until: 1.46, again: true })).toBe(true);
    expect(showing(toasts, 1.2, 10.2)).toEqual(["wood"]);
  });

  it("goes when its stretch of scroll is left, and does not come outside it", () => {
    const toasts = scrolled();
    showing(toasts, 1.2, 0);
    toasts.send("Getting Wood", () => "wood", 0, { from: 1.05, until: 1.46 });
    showing(toasts, 1.5, 0.5); // left: it is sent off
    expect(showing(toasts, 1.5, 0.5 + TRAVEL + 0.01)).toEqual([]);
    expect(toasts.list).toHaveLength(1); // (done with: only the scrolled one is left)
    expect(toasts.send("Sweet Dreams", () => "bed", 1, { from: 3.0, until: 3.6 })).toBe(false);
  });

  it("is gone at once, not slid out, when the scroll is taken out of its stretch at a jump", () => {
    const toasts = scrolled();
    showing(toasts, 1.2, 0);
    toasts.send("The End", () => "end", 0.5, { from: 1.05, until: Infinity });
    expect(toasts.update(0, 1.0, true)).toEqual([]); // (the page began again, at the top)
    expect(showing(toasts, 0, 1.1)).toEqual([]);
  });

  it("is sent off by the scrolled toast that comes after it", () => {
    const toasts = new Toasts([{ card: "dirt", from: 2.3, until: 99 }]);
    showing(toasts, 2.0, 0);
    toasts.send("On A Rail", () => "rail", 0, { from: 1.9, until: 2.5 });
    expect(showing(toasts, 2.4, 1.0)).toEqual(["dirt", "rail"]);
    expect(showing(toasts, 2.4, 1.0 + TRAVEL + 0.01)).toEqual(["dirt"]);
  });
});
