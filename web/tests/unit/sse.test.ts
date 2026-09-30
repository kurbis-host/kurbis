import { QueryClient } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";
import { applyServerEvent } from "../../lib/sse";
import { queryKeys } from "../../lib/query";
import { server } from "./fixtures";

describe("server event integration", () => {
  it("patches authoritative server state into the shared cache", () => {
    const client = new QueryClient();
    client.setQueryData(queryKeys.server, server);
    applyServerEvent(client, { id: 10, type: "server", data: { ...server, observed_state: "online", desired_state: "running", player_count: 2 } });
    expect(client.getQueryData<typeof server>(queryKeys.server)).toMatchObject({ observed_state: "online", player_count: 2 });
  });

  it("appends live log entries without duplicating event ids", () => {
    const client = new QueryClient();
    client.setQueryData(queryKeys.logs, { items: [], next_after_id: null });
    const event = { id: 12, type: "log" as const, data: { id: 12, created_at: "2026-09-30T18:20:00Z", line: "Server ready" } };
    applyServerEvent(client, event);
    applyServerEvent(client, event);
    const page = client.getQueryData<{ items: { id: number }[] }>(queryKeys.logs);
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
    applyServerEvent(client, { id: 13, type: "players", data: players });
    applyServerEvent(client, { id: 14, type: "whitelist", data: whitelist });
    expect(client.getQueryData(queryKeys.players)).toEqual(players);
    expect(client.getQueryData(queryKeys.whitelist)).toEqual(whitelist);
    expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.server });
  });
});
