"use client";

import * as Tooltip from "@radix-ui/react-tooltip";
import { QueryClient, QueryClientProvider, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState, type ReactNode } from "react";
import type { Server, Session } from "../lib/contracts";
import { fetchServer } from "../lib/api/queries";
import { queryKeys } from "../lib/query";
import { applyServerEvent, parseServerEvent } from "../lib/sse";

function EventsBridge() {
  const queryClient = useQueryClient();
  const { data: server } = useQuery({ queryKey: queryKeys.server, queryFn: fetchServer });

  useEffect(() => {
    if (!server) return;
    queryClient.setQueryData(queryKeys.eventStatus, "connecting");
    const source = new EventSource("/api/v1/server/events", { withCredentials: true });
    const consume = (event: MessageEvent<string>, type?: string) => {
      try {
        const raw = type
          ? JSON.stringify({ id: Number(event.lastEventId || 0), type, data: JSON.parse(event.data) })
          : event.data;
        const parsed = parseServerEvent(raw);
        if (parsed.success) applyServerEvent(queryClient, parsed.data);
      } catch {
        void queryClient.invalidateQueries({ queryKey: queryKeys.server });
      }
    };
    source.onopen = () => queryClient.setQueryData(queryKeys.eventStatus, "live");
    source.onerror = () => queryClient.setQueryData(queryKeys.eventStatus, "reconnecting");
    source.onmessage = (event) => consume(event);
    for (const name of ["server", "operation", "snapshot", "players", "whitelist", "log"] as const) {
      source.addEventListener(name, (event) => consume(event as MessageEvent<string>, name));
    }
    return () => source.close();
  }, [queryClient, server?.id]);
  return null;
}

export function AppProviders({ session, server, children }: { session: Session; server: Server | null; children: ReactNode }) {
  const [client] = useState(() => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { staleTime: 20_000, retry: 1, refetchOnWindowFocus: false }, mutations: { retry: 0 } } });
    queryClient.setQueryData(queryKeys.session, session);
    queryClient.setQueryData(queryKeys.server, server);
    queryClient.setQueryData(queryKeys.eventStatus, server ? "connecting" : "idle");
    return queryClient;
  });
  return <QueryClientProvider client={client}><Tooltip.Provider delayDuration={300}><EventsBridge />{children}</Tooltip.Provider></QueryClientProvider>;
}
