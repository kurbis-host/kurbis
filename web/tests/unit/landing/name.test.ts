/** The name a visitor picks: what may be typed, what is wrong with it, whether it is free. */

import { describe, expect, it } from "vitest";
import { LONGEST, flawOf, isFree, typed } from "../../../lib/landing/name";

describe("a server's name", () => {
  it("is typed in lowercase letters, digits and dashes only", () => {
    expect(typed("Big Farm!")).toBe("bigfarm");
    expect(typed("läuft_2-gether")).toBe("luft2-gether");
    expect(typed("x".repeat(80))).toHaveLength(LONGEST);
  });

  it("says what is wrong with a name", () => {
    expect(flawOf("")).toBe("empty");
    expect(flawOf("ab")).toBe("short");
    expect(flawOf("-abc")).toBe("dash");
    expect(flawOf("abc-")).toBe("dash");
    expect(flawOf("my-world-2")).toBeNull();
  });

  it("answers whether a name is free, and can be called off", async () => {
    await expect(isFree("pumpkinpatch")).resolves.toBe(true);
    await expect(isFree("steve")).resolves.toBe(false);
    const abort = new AbortController();
    const asked = isFree("pumpkinpatch", abort.signal);
    abort.abort();
    await expect(asked).rejects.toThrow();
  });
});
