"use client";

import * as Tooltip from "@radix-ui/react-tooltip";
import { QueryClient, QueryClientProvider, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState, type ReactNode } from "react";
import { usePathname } from "next/navigation";
import type { Server, Session } from "../lib/contracts";
import { fetchServer, fetchServers } from "../lib/api/queries";
import { queryKeys, selectedServerId } from "../lib/query";
import { applyServerEvent, parseServerEvent } from "../lib/sse";

function EventsBridge({ scopedServerId }: { scopedServerId?: string }) {
  const queryClient = useQueryClient();
  const pathname = usePathname();
  const { data: servers = [] } = useQuery({
    queryKey: queryKeys.servers,
    queryFn: fetchServers,
    enabled: !scopedServerId,
  });
  const serverId = scopedServerId
    ?? selectedServerId(pathname)
    ?? servers.find((server) => server.is_default)?.id
    ?? servers[0]?.id
    ?? null;
  const { data: server } = useQuery({
    queryKey: serverId ? queryKeys.server(serverId) : ["servers", "none"],
    queryFn: () => fetchServer(serverId!),
    enabled: Boolean(serverId),
  });

  useEffect(() => {
    if (!server) return;
    const eventStatusKey = queryKeys.eventStatus(server.id);
    queryClient.setQueryData(eventStatusKey, "connecting");
    const source = new EventSource(`/api/events?server_id=${encodeURIComponent(server.id)}`, { withCredentials: true });
    const consume = (event: MessageEvent<string>) => {
      const parsed = parseServerEvent(event.data);
      if (parsed.success) {
        applyServerEvent(queryClient, server.id, parsed.data);
      } else {
        // Recover all visible server data if an event cannot be decoded.
        void queryClient.invalidateQueries({ queryKey: queryKeys.server(server.id) });
      }
    };
    source.onopen = () => queryClient.setQueryData(eventStatusKey, "live");
    source.onerror = () => queryClient.setQueryData(eventStatusKey, "reconnecting");
    for (const name of ["server", "operation", "snapshot", "players", "whitelist", "log"] as const) {
      source.addEventListener(name, (event) => consume(event as MessageEvent<string>));
    }
    return () => source.close();
  }, [queryClient, server?.id]);
  return null;
}

export function AppProviders({ session, servers, children }: { session: Session; servers: Server[]; children: ReactNode }) {
  const [client] = useState(() => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { staleTime: 20_000, retry: 1, refetchOnWindowFocus: false }, mutations: { retry: 0 } } });
    queryClient.setQueryData(queryKeys.session, session);
    for (const server of servers) queryClient.setQueryData(queryKeys.server(server.id), server);
    queryClient.setQueryData(queryKeys.servers, servers);
    return queryClient;
  });
  return <QueryClientProvider client={client}><Tooltip.Provider delayDuration={300}><EventsBridge />{children}</Tooltip.Provider></QueryClientProvider>;
}

export function ServerProviders({ server, children }: { server: Server; children: ReactNode }) {
  const [client] = useState(() => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { staleTime: 20_000, retry: 1, refetchOnWindowFocus: false }, mutations: { retry: 0 } } });
    queryClient.setQueryData(queryKeys.server(server.id), server);
    return queryClient;
  });
  return <QueryClientProvider client={client}><Tooltip.Provider delayDuration={300}><EventsBridge scopedServerId={server.id} />{children}</Tooltip.Provider></QueryClientProvider>;
}
