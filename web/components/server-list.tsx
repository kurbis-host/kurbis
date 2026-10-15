"use client";

import Link from "next/link";
import { useQuery } from "@tanstack/react-query";
import type { components } from "../lib/api/generated";
import { browserApi } from "../lib/api/operations";
import { lifecycle } from "../lib/lifecycle";
import { formatBytes } from "../lib/format";
import { queryKeys } from "../lib/query";
import { PageHeader, StatusLabel, Surface } from "./ui";
import { RevokeServerAccess } from "./revoke-server-access";

type Server = components["schemas"]["Server"];
type ServerScopedSession = components["schemas"]["ServerScopedSession"];

export function ServerList({
  servers,
  sessions,
  canClaimServer,
}: {
  servers: Server[];
  sessions: ServerScopedSession[];
  canClaimServer: boolean;
}) {
  const { data: currentServers = servers } = useQuery({
    queryKey: queryKeys.servers,
    queryFn: browserApi.getServers,
    initialData: servers,
    staleTime: 20_000,
  });
  const { data: currentSessions = sessions } = useQuery({
    queryKey: queryKeys.serverSessions,
    queryFn: browserApi.getServerSessions,
    initialData: sessions,
    staleTime: 20_000,
  });
  return (
    <>
      <PageHeader
        title="Servers"
        description="Choose a world you own or have been granted access to."
        actions={canClaimServer ? <Link className="inline-flex min-h-11 items-center rounded-sm bg-pumpkin px-4 font-[760] text-[#25170f] no-underline" href="/app/setup">Claim a server</Link> : null}
      />
      {currentServers.length ? (
        <ul aria-label="Accessible servers" className="m-0 grid list-none gap-3 p-0">
          {currentServers.map((server) => {
            const state = lifecycle[server.observed_state];
            const grant = currentSessions.find((item) => item.server_id === server.id);
            return (
              <li key={server.id}>
                <Surface className="flex flex-wrap items-center justify-between gap-4 p-5 transition-colors hover:bg-[#fffdf7]">
                <Link href={`/manage/${server.id}`} className="min-w-0 flex-1 no-underline">
                  <div className="flex flex-wrap items-center gap-2.5">
                    <h2 className="m-0 break-all font-display text-xl font-[820]">{server.subdomain}</h2>
                    {server.is_default ? <span className="rounded-full bg-[#dfe7d2] px-2.5 py-1 text-[11px] font-[760] text-[#3f713e]">Default</span> : null}
                    {grant ? <span className="rounded-full bg-[#e4e7ec] px-2.5 py-1 text-[11px] font-[760] text-[#4c5965]">Shared access</span> : null}
                  </div>
                  <p className="mt-1.5 mb-0 text-[13px] text-ink-soft">{server.java_address}</p>
                  <div className="mt-2 flex flex-wrap items-center gap-4 text-[13px] text-ink-soft">
                    <StatusLabel tone={state.tone}>{state.label}</StatusLabel>
                    <span>{formatBytes(server.storage_bytes)} stored</span>
                  </div>
                </Link>
                {grant ? <RevokeServerAccess sessionId={grant.id} /> : null}
                </Surface>
              </li>
            );
          })}
        </ul>
      ) : (
        <Surface className="p-6">
          <p className="m-0 text-ink-soft">No hosted worlds or shared server access yet.</p>
          {canClaimServer ? <Link className="mt-4 inline-flex min-h-11 items-center rounded-sm bg-pumpkin px-4 font-[760] text-[#25170f] no-underline" href="/app/setup">Claim a server</Link> : null}
        </Surface>
      )}
    </>
  );
}
