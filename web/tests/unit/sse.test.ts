import { QueryClient } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";
import type { components } from "../../lib/api/generated";
import { applyServerEvent, parseServerEvent } from "../../lib/sse";
import { queryKeys } from "../../lib/query";
import { activeAiJob, aiOverview, finishedAiJob, server } from "./fixtures";

type AiJobDetail = components["schemas"]["AiJobDetail"];
type ServerEvent = components["schemas"]["ServerEvent"];

describe("server event integration", () => {
  it("patches authoritative server state into the shared cache", () => {
    const client = new QueryClient();
    client.setQueryData(queryKeys.server(server.id), server);
    applyServerEvent(client, server.id, { id: 10, type: "server", data: { ...server, observed_state: "online", desired_state: "running", player_count: 2 } });
    expect(client.getQueryData<typeof server>(queryKeys.server(server.id))).toMatchObject({ observed_state: "online", player_count: 2 });
  });

  it("prevents an older in-flight server read from overwriting a live event", async () => {
    const client = new QueryClient();
    client.setQueryData(queryKeys.server(server.id), server);
    let finishStaleRead!: (value: typeof server) => void;
    const staleRead = client.fetchQuery({
      queryKey: queryKeys.server(server.id),
      queryFn: () => new Promise<typeof server>((resolve) => { finishStaleRead = resolve; }),
      staleTime: 0,
    });
    await Promise.resolve();

    const liveServer = { ...server, observed_state: "online" as const, player_count: 3 };
    applyServerEvent(client, server.id, { id: 11, type: "server", data: liveServer });
    finishStaleRead(server);
    await staleRead.catch(() => undefined);

    expect(client.getQueryData<typeof server>(queryKeys.server(server.id))).toMatchObject({
      observed_state: "online",
      player_count: 3,
    });
  });

  it("appends live log entries without duplicating event ids", () => {
    const client = new QueryClient();
    client.setQueryData(queryKeys.logs(server.id), { items: [], next_after_id: null });
    const event = { id: 12, type: "log" as const, data: { id: 12, created_at: "2026-09-30T18:20:00Z", line: "Server ready" } };
    applyServerEvent(client, server.id, event);
    applyServerEvent(client, server.id, event);
    const page = client.getQueryData<{ items: { id: number }[] }>(queryKeys.logs(server.id));
    expect(page?.items).toHaveLength(1);
  });

  it("replaces player and whitelist caches from typed live events", () => {
    const client = new QueryClient();
    const invalidate = vi.spyOn(client, "invalidateQueries");
    const players = [{
      player_id: "00000000-0000-0000-0000-000000000001",
      name: "Alex",
      edition: "java" as const,
      connected_at: "2026-09-30T18:20:00Z",
      last_seen_at: "2026-09-30T18:20:01Z",
    }];
    const whitelist = {
      enabled: true,
      revision: 3,
      applied_revision: 3,
      synchronized: true,
      entries: [{
        player_id: players[0].player_id,
        name: players[0].name,
        created_at: "2026-09-30T18:19:00Z",
        updated_at: "2026-09-30T18:19:00Z",
      }],
    };
    applyServerEvent(client, server.id, { id: 13, type: "players", data: players });
    applyServerEvent(client, server.id, { id: 14, type: "whitelist", data: whitelist });
    expect(client.getQueryData(queryKeys.players(server.id))).toEqual(players);
    expect(client.getQueryData(queryKeys.whitelist(server.id))).toEqual(whitelist);
    expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.server(server.id) });
  });

  it("patches the operator roster from live operator events and parses the new event kind", () => {
    const client = new QueryClient();
    const roster = {
      revision: 2,
      applied_revision: 2,
      synchronized: true,
      entries: [{
        player_id: "00000000-0000-0000-0000-000000000001",
        name: "Alex",
        created_at: "2026-09-30T18:20:00Z",
        updated_at: "2026-09-30T18:20:00Z",
        is_owner: true,
      }],
    };
    const rawEvent = JSON.stringify({ id: 21, type: "operators", data: roster });
    const parsed = parseServerEvent(rawEvent);
    expect(parsed.success).toBe(true);
    if (!parsed.success) return;
    applyServerEvent(client, server.id, parsed.data);
    expect(client.getQueryData(queryKeys.operators(server.id))).toEqual(roster);
  });

  it("refreshes activity for every durable event type", () => {
    const client = new QueryClient();
    const invalidate = vi.spyOn(client, "invalidateQueries");
    const events: ServerEvent[] = [
      { id: 1, type: "server", data: server },
      { id: 2, type: "operation" },
      { id: 3, type: "snapshot" },
      { id: 4, type: "players", data: [] },
      { id: 5, type: "whitelist", data: { enabled: false, revision: 0, applied_revision: 0, synchronized: true, entries: [] } },
      { id: 6, type: "operators", data: { revision: 0, applied_revision: 0, synchronized: true, entries: [] } },
      { id: 7, type: "log" },
      { id: 8, type: "ai", data: aiOverview },
    ];

    for (const event of events) {
      invalidate.mockClear();
      applyServerEvent(client, server.id, event);
      expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.activity(server.id) });
    }
  });

  it("replaces the AI overview and refreshes only the request details still in progress", () => {
    const client = new QueryClient();
    const detail = (job: typeof activeAiJob): AiJobDetail => ({ ...job, tool_calls: [] });
    client.setQueryData(queryKeys.aiJob(server.id, activeAiJob.id), detail(activeAiJob));
    client.setQueryData(queryKeys.aiJob(server.id, finishedAiJob.id), detail(finishedAiJob));

    const parsed = parseServerEvent(JSON.stringify({ id: 30, type: "ai", data: aiOverview }));
    expect(parsed.success).toBe(true);
    if (!parsed.success) return;
    applyServerEvent(client, server.id, parsed.data);

    expect(client.getQueryData(queryKeys.ai(server.id))).toEqual(aiOverview);
    expect(client.getQueryState(queryKeys.aiJob(server.id, activeAiJob.id))?.isInvalidated).toBe(true);
    expect(client.getQueryState(queryKeys.aiJob(server.id, finishedAiJob.id))?.isInvalidated).toBe(false);
  });

  it("rejects events of unknown kinds", () => {
    expect(parseServerEvent(JSON.stringify({ id: 31, type: "weather", data: {} })).success).toBe(false);
  });
});
