/** The name a visitor picks: what may be typed, what is wrong with it, whether it is free. */

import { afterEach, describe, expect, it, vi } from "vitest";
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

  it("asks the control plane whether a name is free, and can be called off", async () => {
    const fetch = vi.fn((url: string, init?: RequestInit) =>
      new Promise<Response>((resolve, reject) => {
        init?.signal?.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")));
        const subdomain = url.split("/").pop() ?? "";
        setTimeout(() => resolve(Response.json({ subdomain, address: `${subdomain}.kurbis.host`, available: subdomain !== "steve" })), 5);
      }),
    );
    vi.stubGlobal("fetch", fetch);
    await expect(isFree("pumpkinpatch")).resolves.toBe(true);
    expect(fetch).toHaveBeenLastCalledWith("/api/v1/server-addresses/pumpkinpatch", expect.objectContaining({ headers: { accept: "application/json" } }));
    await expect(isFree("steve")).resolves.toBe(false);
    const abort = new AbortController();
    const asked = isFree("pumpkinpatch", abort.signal);
    abort.abort();
    await expect(asked).rejects.toThrow();
  });

  it("does not make up an answer when the check fails", async () => {
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve(new Response("unavailable", { status: 503 }))));
    await expect(isFree("pumpkinpatch")).rejects.toThrow("503");
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
});
