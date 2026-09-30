import type { QueryClient } from "@tanstack/react-query";
import { logEntrySchema, serverEventSchema, serverSchema, type ServerEvent } from "./contracts";
import { queryKeys } from "./query";

export function applyServerEvent(queryClient: QueryClient, event: ServerEvent) {
  switch (event.type) {
    case "server":
      if (event.data) queryClient.setQueryData(queryKeys.server, serverSchema.parse(event.data));
      else void queryClient.invalidateQueries({ queryKey: queryKeys.server });
      void queryClient.invalidateQueries({ queryKey: queryKeys.activity });
      break;
    case "operation":
      void queryClient.invalidateQueries({ queryKey: queryKeys.server });
      void queryClient.invalidateQueries({ queryKey: queryKeys.activity });
      break;
    case "snapshot":
      void queryClient.invalidateQueries({ queryKey: queryKeys.snapshots });
      void queryClient.invalidateQueries({ queryKey: queryKeys.server });
      break;
    case "players":
      if (event.data) queryClient.setQueryData(queryKeys.players, event.data);
      else void queryClient.invalidateQueries({ queryKey: queryKeys.players });
      void queryClient.invalidateQueries({ queryKey: queryKeys.server });
      break;
    case "whitelist":
      if (event.data) queryClient.setQueryData(queryKeys.whitelist, event.data);
      else void queryClient.invalidateQueries({ queryKey: queryKeys.whitelist });
      break;
    case "log":
      if (event.data) {
        const entry = logEntrySchema.parse(event.data);
        queryClient.setQueryData(queryKeys.logs, (current: { items: typeof entry[]; next_after_id: number | null } | undefined) => {
          if (!current || current.items.some((item) => item.id === entry.id)) return current;
          return { ...current, items: [...current.items, entry].slice(-1000), next_after_id: entry.id };
        });
      } else void queryClient.invalidateQueries({ queryKey: queryKeys.logs });
      break;
  }
}

export function parseServerEvent(raw: string) {
  try { return serverEventSchema.safeParse(JSON.parse(raw)); }
  catch { return { success: false as const, error: new Error("Invalid SSE JSON") }; }
}
