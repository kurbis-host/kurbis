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

describe("AI assistant routes", () => {
  it("reads the overview and a request, saves settings and cancels through the generated server routes", async () => {
    const settings = { enabled: true, model: "claude-haiku-5-5", effort: "low" as const, monthly_budget_micros: 2_500_000 };
    const requests: { method: string; path: string; body: unknown }[] = [];
    const fetchMock = vi.fn(async (input: RequestInfo | URL, _init?: RequestInit) => {
      const request = input as Request;
      const text = await request.text();
      requests.push({ method: request.method, path: new URL(request.url).pathname, body: text ? JSON.parse(text) : undefined });
      return Response.json({});
    });
    vi.stubGlobal("fetch", fetchMock);

    await browserApi.getAiOverview("server-1");
    await browserApi.updateAiSettings("server-1", settings);
    await browserApi.getAiJob("server-1", "job-1");
    await browserApi.cancelAiJob("server-1", "job-1");

    expect(requests).toEqual([
      { method: "GET", path: "/api/v1/servers/server-1/ai", body: undefined },
      { method: "PUT", path: "/api/v1/servers/server-1/ai/settings", body: settings },
      { method: "GET", path: "/api/v1/servers/server-1/ai/jobs/job-1", body: undefined },
      { method: "POST", path: "/api/v1/servers/server-1/ai/jobs/job-1/cancel", body: undefined },
    ]);
  });
});
