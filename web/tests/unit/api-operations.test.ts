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
    expect(new URL(request.url, "http://localhost").pathname).toBe("/api/v1/servers/00000000-0000-4000-8000-000000000001/commands");
    expect(await request.json()).toEqual({ command: "reset", mode: "world" });
  });
});

describe("name-only player mutations", () => {
  it("sends whitelist additions by name and removals by stable stored ID", async () => {
    const whitelist = { enabled: true, revision: 1, applied_revision: 1, synchronized: true, entries: [] };
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = input instanceof Request
        ? input
        : new Request(new URL(String(input), "http://localhost"), init);
      if (request.method === "POST") {
        expect(await request.clone().json()).toEqual({ name: "OfflineJava" });
        return Response.json(whitelist, { status: 201 });
      }
      expect(new URL(request.url, "http://localhost").searchParams.get("player_id")).toBe("offline-player-id");
      return Response.json(whitelist);
    });
    vi.stubGlobal("fetch", fetchMock);

    await browserApi.addWhitelistEntry("server-1", { name: "OfflineJava" });
    await browserApi.removeWhitelistEntry("server-1", "offline-player-id");
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("uses the server operator roster endpoints with name-only targets", async () => {
    const roster = { revision: 2, applied_revision: 2, synchronized: true, entries: [] };
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = input instanceof Request
        ? input
        : new Request(new URL(String(input), "http://localhost"), init);
      expect(new URL(request.url, "http://localhost").pathname).toBe("/api/v1/servers/server-1/operators");
      if (request.method === "POST") expect(await request.clone().json()).toEqual({ name: "TrustedPlayer" });
      if (request.method === "DELETE") {
        expect(new URL(request.url, "http://localhost").searchParams.get("player_id")).toBe("trusted-player-id");
      }
      return Response.json(roster, { status: request.method === "POST" ? 201 : 200 });
    });
    vi.stubGlobal("fetch", fetchMock);

    expect(await browserApi.getOperators("server-1")).toEqual(roster);
    expect(await browserApi.addOperatorEntry("server-1", { name: "TrustedPlayer" })).toEqual(roster);
    expect(await browserApi.removeOperatorEntry("server-1", "trusted-player-id")).toEqual(roster);
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });
});
