/** Chat stacks: each newer line pushes the older ones up, and no two lines share a place. */

import { describe, expect, it } from "vitest";
import { newerThan } from "../../../lib/landing/film/interface";

describe("chat stacking", () => {
  it("counts the showing lines that began later", () => {
    const lines = [
      { begun: 1, shown: 1, here: true },
      { begun: 2, shown: 1, here: true },
      { begun: 3, shown: 0, here: false }, // gone: takes no room
      { begun: null, shown: 0 }, // not yet said
    ];
    expect(newerThan(lines, 0)).toEqual([2]);
    expect(newerThan(lines, 1)).toEqual([]);
    expect(newerThan(lines, 3)).toEqual([]);
  });

  it("tells lines that began at the same moment apart by their order", () => {
    // Two triggers that fire in the same frame give their lines the same moment.
    const lines = [
      { begun: 5, shown: 1, here: true },
      { begun: 5, shown: 1, here: true },
      { begun: 5, shown: 1, here: true },
    ];
    const places = lines.map((_, i) => newerThan(lines, i).length);
    expect(places).toEqual([2, 1, 0]);
    expect(new Set(places).size).toBe(lines.length);
  });
});
