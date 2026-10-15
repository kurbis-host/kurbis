/** Names that do in the name fields what they do in the game (lib/landing/film/eggs.ts). */

import { describe, expect, it } from "vitest";
import { eggOf } from "../../../lib/landing/film/eggs";
import { typed } from "../../../lib/landing/name";

describe("a name that is an egg", () => {
  it("is known as it is typed, in either case", () => {
    expect(eggOf("dinnerbone")).toBe("upsideDown");
    expect(eggOf("GRUMM")).toBe("upsideDown");
    expect(eggOf("Herobrine")).toBe("removed");
    expect(eggOf("creeper")).toBe("creeper");
  });

  it("takes jeb_ as the field can hold it", () => {
    expect(eggOf(typed("jeb_"))).toBe("rainbow");
  });

  it("is no egg once the name is changed", () => {
    for (const name of ["", "dinnerbon", "dinnerbones", "jebb", "creepers", "herobrine2", "steve"]) expect(eggOf(name)).toBeNull();
  });
});
