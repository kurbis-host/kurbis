import type { QueryClient } from "@tanstack/react-query";
import type { components } from "./api/generated";
import { queryKeys } from "./query";

type ServerEvent = components["schemas"]["ServerEvent"];

export function applyServerEvent(queryClient: QueryClient, serverId: string, event: ServerEvent) {
  const serverKey = queryKeys.server(serverId);
  const activityKey = queryKeys.activity(serverId);
  const snapshotsKey = queryKeys.snapshots(serverId);
  const playersKey = queryKeys.players(serverId);
  const whitelistKey = queryKeys.whitelist(serverId);
  const logsKey = queryKeys.logs(serverId);
  // Every durable event also adds a row to the activity feed.
  void queryClient.invalidateQueries({ queryKey: activityKey });

  switch (event.type) {
    case "server":
      if (event.data) {
        void queryClient.cancelQueries({ queryKey: serverKey, exact: true });
        queryClient.setQueryData(serverKey, event.data);
      } else {
        void queryClient.invalidateQueries({ queryKey: serverKey });
      }
      break;
    case "operation":
      void queryClient.invalidateQueries({ queryKey: serverKey });
      break;
    case "snapshot":
      void queryClient.invalidateQueries({ queryKey: snapshotsKey });
      void queryClient.invalidateQueries({ queryKey: serverKey });
      break;
    case "players":
      if (event.data) {
        void queryClient.cancelQueries({ queryKey: playersKey, exact: true });
        queryClient.setQueryData(playersKey, event.data);
      } else {
        void queryClient.invalidateQueries({ queryKey: playersKey });
      }
      void queryClient.invalidateQueries({ queryKey: serverKey });
      break;
    case "whitelist":
      if (event.data) {
        void queryClient.cancelQueries({ queryKey: whitelistKey, exact: true });
        queryClient.setQueryData(whitelistKey, event.data);
      } else {
        void queryClient.invalidateQueries({ queryKey: whitelistKey });
      }
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
      || !["server", "operation", "snapshot", "players", "whitelist", "log"].includes(value.type ?? "")) {
      return { success: false as const, error: new Error("Invalid server event") };
    }
    return { success: true as const, data: value as ServerEvent };
  } catch {
    return { success: false as const, error: new Error("Invalid SSE JSON") };
  }
}
