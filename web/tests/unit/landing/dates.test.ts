/** The days the game marks, which the film keeps too (lib/landing/film/dates.ts). */

import { describe, expect, it } from "vitest";
import { dayOf } from "../../../lib/landing/film/dates";

const on = (month: number, day: number, hour = 12) => dayOf(new Date(2026, month - 1, day, hour));

describe("a day the game marks", () => {
  it("is Halloween on October 31 only, all day", () => {
    expect(on(10, 31, 0)).toBe("halloween");
    expect(on(10, 31, 23)).toBe("halloween");
    expect(on(10, 30, 23)).toBeNull();
    expect(on(11, 1, 0)).toBeNull();
  });

  it("is Christmas from December 24 to 26", () => {
    for (const day of [24, 25, 26]) expect(on(12, day)).toBe("christmas");
    expect(on(12, 23, 23)).toBeNull();
    expect(on(12, 27, 0)).toBeNull();
  });

  it("is not one on an ordinary day", () => {
    for (const [month, day] of [[1, 1], [10, 15], [12, 31], [6, 24]]) expect(on(month, day)).toBeNull();
  });
});
