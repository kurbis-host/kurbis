"use client";

import * as Tooltip from "@radix-ui/react-tooltip";
import { QueryClient, QueryClientProvider, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState, type ReactNode } from "react";
import { usePathname } from "next/navigation";
import type { components } from "../lib/api/generated";
import { browserApi } from "../lib/api/operations";
import { serverEventStreamPath } from "../lib/api/paths";
import { queryKeys, selectedServerId } from "../lib/query";
import { applyServerEvent, parseServerEvent, serverEventKinds } from "../lib/sse";
import { ReadOnlyProvider } from "./ui";

type Server = components["schemas"]["Server"];
type ServerScopedSession = components["schemas"]["ServerScopedSession"];
type Session = components["schemas"]["Session"];

function EventsBridge({ scopedServerId }: { scopedServerId?: string }) {
  const queryClient = useQueryClient();
  const pathname = usePathname();
  const { data: servers = [] } = useQuery({
    queryKey: queryKeys.servers,
    queryFn: browserApi.getServers,
    enabled: !scopedServerId,
  });
  const serverId = scopedServerId
    ?? selectedServerId(pathname)
    ?? servers.find((server) => server.is_default)?.id
    ?? servers[0]?.id
    ?? null;
  const { data: server } = useQuery({
    queryKey: serverId ? queryKeys.server(serverId) : ["servers", "none"],
    queryFn: () => browserApi.getServer(serverId!),
    enabled: Boolean(serverId),
  });

  useEffect(() => {
    if (!server) return;
    const eventStatusKey = queryKeys.eventStatus(server.id);
    queryClient.setQueryData(eventStatusKey, "connecting");
    const source = new EventSource(serverEventStreamPath(server.id), { withCredentials: true });
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
    for (const name of serverEventKinds) {
      source.addEventListener(name, (event) => consume(event as MessageEvent<string>));
    }
    return () => source.close();
  }, [queryClient, server?.id]);
  return null;
}

function createPanelClient({ session, servers, serverSessions, server }: { session?: Session; servers?: Server[]; serverSessions?: ServerScopedSession[]; server?: Server }) {
  const initialServers = servers ?? [];
  const queryClient = new QueryClient({ defaultOptions: { queries: { staleTime: 20_000, retry: 1, refetchOnWindowFocus: false }, mutations: { retry: 0 } } });
  if (session) queryClient.setQueryData(queryKeys.session, session);
  for (const item of initialServers) queryClient.setQueryData(queryKeys.server(item.id), item);
  if (server) queryClient.setQueryData(queryKeys.server(server.id), server);
  if (servers !== undefined) queryClient.setQueryData(queryKeys.servers, initialServers);
  if (serverSessions !== undefined) queryClient.setQueryData(queryKeys.serverSessions, serverSessions);
  return queryClient;
}

export function AppProviders({ session, servers, serverSessions, children }: { session: Session; servers: Server[]; serverSessions?: ServerScopedSession[]; children: ReactNode }) {
  const [client] = useState(() => {
    return createPanelClient({ session, servers, serverSessions });
  });
  return <QueryClientProvider client={client}><Tooltip.Provider delayDuration={300}><EventsBridge />{children}</Tooltip.Provider></QueryClientProvider>;
}

export function ServerProviders({ server, session, servers, serverSessions, readOnly = false, children }: { server: Server; session?: Session; servers?: Server[]; serverSessions?: ServerScopedSession[]; readOnly?: boolean; children: ReactNode }) {
  const [client] = useState(() => {
    return createPanelClient({ server, session, servers, serverSessions });
  });
  return <QueryClientProvider client={client}><Tooltip.Provider delayDuration={300}><ReadOnlyProvider value={readOnly}><EventsBridge scopedServerId={session && !readOnly ? undefined : server.id} />{children}</ReadOnlyProvider></Tooltip.Provider></QueryClientProvider>;
}

/** The admin dashboard: its own query cache, no server event stream. */
export function AdminProviders({ children }: { children: ReactNode }) {
  const [client] = useState(() => new QueryClient({ defaultOptions: { queries: { staleTime: 10_000, retry: 1, refetchOnWindowFocus: true } } }));
  return <QueryClientProvider client={client}><Tooltip.Provider delayDuration={200}>{children}</Tooltip.Provider></QueryClientProvider>;
}
