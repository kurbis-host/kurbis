import type { QueryClient, QueryKey } from "@tanstack/react-query";
import { isActiveAiJob } from "./ai";
import type { components } from "./api/generated";
import { queryKeys } from "./query";

type AiJobDetail = components["schemas"]["AiJobDetail"];
type ServerEvent = components["schemas"]["ServerEvent"];
type ServerEventKind = components["schemas"]["ServerEventKind"];

// A record over the generated kind keeps this list complete as the contract grows.
const eventKinds: Record<ServerEventKind, true> = {
  server: true,
  operation: true,
  snapshot: true,
  log: true,
  players: true,
  whitelist: true,
  operators: true,
  ai: true,
};
/** Every named event of the per-server stream. */
export const serverEventKinds = Object.keys(eventKinds) as ServerEventKind[];

/** Replace a cached resource with the event's authoritative copy, or refetch it when the event carries none. */
function replaceQueryData(queryClient: QueryClient, queryKey: QueryKey, data: unknown) {
  if (data) {
    // Keep an older in-flight read from overwriting the live copy.
    void queryClient.cancelQueries({ queryKey, exact: true });
    queryClient.setQueryData(queryKey, data);
  } else {
    void queryClient.invalidateQueries({ queryKey });
  }
}

export function applyServerEvent(queryClient: QueryClient, serverId: string, event: ServerEvent) {
  const serverKey = queryKeys.server(serverId);
  const logsKey = queryKeys.logs(serverId);
  // Every durable event also adds a row to the activity feed.
  void queryClient.invalidateQueries({ queryKey: queryKeys.activity(serverId) });

  switch (event.type) {
    case "server":
      replaceQueryData(queryClient, serverKey, event.data);
      break;
    case "operation":
      void queryClient.invalidateQueries({ queryKey: serverKey });
      break;
    case "snapshot":
      void queryClient.invalidateQueries({ queryKey: queryKeys.snapshots(serverId) });
      void queryClient.invalidateQueries({ queryKey: serverKey });
      break;
    case "players":
      replaceQueryData(queryClient, queryKeys.players(serverId), event.data);
      void queryClient.invalidateQueries({ queryKey: serverKey });
      break;
    case "whitelist":
      replaceQueryData(queryClient, queryKeys.whitelist(serverId), event.data);
      break;
    case "operators":
      replaceQueryData(queryClient, queryKeys.operators(serverId), event.data);
      break;
    case "ai":
      replaceQueryData(queryClient, queryKeys.ai(serverId), event.data);
      // A finished request's tool calls never change again; refresh only the
      // details that were read, or are being read, while it was still active.
      void queryClient.invalidateQueries({
        queryKey: queryKeys.aiJobs(serverId),
        predicate: ({ state }) => {
          const detail = state.data as AiJobDetail | undefined;
          return !detail || isActiveAiJob(detail.job.state);
        },
      });
      break;
    case "log":
      if (event.data) {
        const entry = event.data;
        void queryClient.cancelQueries({ queryKey: logsKey, exact: true });
        queryClient.setQueryData(logsKey, (current: { items: typeof entry[]; next_after_id: number | null } | undefined) => {
          if (!current || current.items.some((item) => item.id === entry.id)) return current;
          return { ...current, items: [...current.items, entry].slice(-1000), next_after_id: entry.id };
        });
      } else void queryClient.invalidateQueries({ queryKey: logsKey });
      break;
  }
}

export function parseServerEvent(raw: string) {
  try {
    const value = JSON.parse(raw) as Partial<ServerEvent>;
    if (typeof value.id !== "number" || !Number.isInteger(value.id)
      || !serverEventKinds.includes(value.type as ServerEventKind)) {
      return { success: false as const, error: new Error("Invalid server event") };
    }
    return { success: true as const, data: value as ServerEvent };
  } catch {
    return { success: false as const, error: new Error("Invalid SSE JSON") };
  }
}
