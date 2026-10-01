import { QueryClient } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";
import type { ServerEvent } from "../../lib/contracts";
import { applyServerEvent } from "../../lib/sse";
import { queryKeys } from "../../lib/query";
import { server } from "./fixtures";

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

  it("refreshes activity for every durable event type", () => {
    const client = new QueryClient();
    const invalidate = vi.spyOn(client, "invalidateQueries");
    const events: ServerEvent[] = [
      { id: 1, type: "server" },
      { id: 2, type: "operation" },
      { id: 3, type: "snapshot" },
      { id: 4, type: "players" },
      { id: 5, type: "whitelist" },
      { id: 6, type: "log" },
    ];

    for (const event of events) {
      invalidate.mockClear();
      applyServerEvent(client, server.id, event);
      expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.activity(server.id) });
    }
  });
});
