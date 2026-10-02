import { afterEach, describe, expect, it, vi } from "vitest";
import { browserApi } from "../../lib/api/operations";

afterEach(() => vi.unstubAllGlobals());

describe("generated browser API client", () => {
  it("uses the current same-origin fetch implementation", async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => Response.json([]));
    vi.stubGlobal("fetch", fetchMock);

    expect(await browserApi.getServers()).toEqual([]);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect((fetchMock.mock.calls[0][0] as Request).url).toContain("/api/v1/servers");
  });

  it("uses generated command bodies and rejects server ids without a route template", async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => Response.json({ observed_state: "queued" }, { status: 202 }));
    vi.stubGlobal("fetch", fetchMock);

    await browserApi.command("00000000-0000-4000-8000-000000000001", { command: "reset", mode: "world" });
    const request = fetchMock.mock.calls[0][0] as Request;
    expect(new URL(request.url).pathname).toBe("/api/v1/servers/00000000-0000-4000-8000-000000000001/commands");
    expect(await request.json()).toEqual({ command: "reset", mode: "world" });
  });
});
