"use client";

import { useQuery } from "@tanstack/react-query";
import {
  ArchiveRestore,
  CircleUserRound,
  Gauge,
  ScrollText,
  Settings2,
  UserRoundCheck,
  WandSparkles,
} from "lucide-react";
import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import type { ReactNode } from "react";
import type { components } from "../lib/api/generated";
import { browserApi } from "../lib/api/operations";
import { lifecycle } from "../lib/lifecycle";
import { queryKeys, selectedServerId } from "../lib/query";
import { RevokeServerAccess } from "./revoke-server-access";

type Server = components["schemas"]["Server"];
type ServerScopedSession = components["schemas"]["ServerScopedSession"];
type Session = components["schemas"]["Session"];

const serverNav = [
  { suffix: "", label: "Overview", Icon: Gauge },
  { suffix: "/console", label: "Console", Icon: ScrollText },
  { suffix: "/backups", label: "Backups", Icon: ArchiveRestore },
  { suffix: "/players", label: "Players", Icon: UserRoundCheck },
  { suffix: "/settings", label: "Settings", Icon: Settings2 },
] as const;

function isActive(pathname: string, href: string) {
  return pathname === href || (href === "/app/account" && pathname.startsWith(`${href}/`));
}

function stateTone(state: string | undefined) {
  if (state === "online") return "bg-[#91bd67] shadow-[0_0_0_3px_rgba(145,189,103,0.13)]";
  if (state === "sleeping") return "bg-[#94a4a5]";
  if (state === "failed") return "bg-[#df7468]";
  return "bg-[#df9d58]";
}

/**
 * Shared navigation and live state surface for account and server-only sessions.
 * The account shell selects a server and links into the same /manage routes as
 * scoped server grants, so the feature panels never have two wrappers.
 */
export function ServerPanelShell({
  servers: initialServers,
  serverSessions: initialServerSessions,
  server: initialServer,
  session: initialSession,
  scoped = false,
  children,
}: {
  servers?: Server[];
  serverSessions?: ServerScopedSession[];
  server?: Server;
  session?: Session;
  scoped?: boolean;
  children: ReactNode;
}) {
  const pathname = usePathname();
  const router = useRouter();
  const routeServerId = selectedServerId(pathname);
  const { data: servers = initialServers ?? [] } = useQuery({
    queryKey: queryKeys.servers,
    queryFn: browserApi.getServers,
    enabled: !scoped || initialServers !== undefined,
    initialData: initialServers,
  });
  const defaultServer = initialServer ?? servers.find((item) => item.is_default) ?? servers[0];
  const activeServerId = initialServer?.id ?? routeServerId ?? defaultServer?.id;
  const { data: server = initialServer } = useQuery({
    queryKey: activeServerId ? queryKeys.server(activeServerId) : ["servers", "none"],
    queryFn: () => browserApi.getServer(activeServerId!),
    enabled: Boolean(activeServerId),
    initialData: activeServerId ? servers.find((item) => item.id === activeServerId) ?? initialServer : undefined,
  });
  const { data: session = initialSession } = useQuery({
    queryKey: queryKeys.session,
    queryFn: browserApi.getSession,
    enabled: !scoped,
    initialData: initialSession,
  });
  const { data: serverSessions = initialServerSessions ?? [] } = useQuery({
    queryKey: queryKeys.serverSessions,
    queryFn: browserApi.getServerSessions,
    initialData: initialServerSessions ?? [],
    staleTime: 20_000,
  });
  const { data: eventStatus = "idle" } = useQuery<string>({
    queryKey: server ? queryKeys.eventStatus(server.id) : ["servers", "none", "event-status"],
    queryFn: async () => "idle",
    enabled: false,
  });
  const state = server ? lifecycle[server.observed_state] : null;
  const scopedGrant = serverSessions.find((grant) => grant.server_id === server?.id);
  const root = server ? `/manage/${server.id}` : "/app/servers";
  const nav = [
    ...(server
      ? serverNav.map(({ suffix, label, Icon }) => ({ href: `${root}${suffix}`, label, Icon }))
      : [{ href: "/app/setup", label: "Setup", Icon: WandSparkles }]),
    ...(!scoped ? [{ href: "/app/account", label: "Account", Icon: CircleUserRound }] : []),
  ];

  function selectServer(serverId: string) {
    const suffix = routeServerId && pathname.startsWith(`/manage/${routeServerId}`)
      ? pathname.slice(`/manage/${routeServerId}`.length)
      : "";
    router.push(`/manage/${encodeURIComponent(serverId)}${suffix}`);
  }

  function afterRevoke(serverId: string) {
    if (session) {
      const accountDefault = servers.find((item) => item.is_default);
      if (accountDefault && accountDefault.id !== serverId) {
        router.replace(`/manage/${accountDefault.id}`);
      } else if (!servers.find((item) => item.id === serverId)?.is_default) {
        router.replace("/app/servers");
      }
      return;
    }
    const nextGrant = serverSessions.find((grant) => grant.server_id !== serverId);
    router.replace(nextGrant ? `/manage/${nextGrant.server_id}` : "/manage/access-unavailable");
  }

  return (
    <div className="grid min-h-screen grid-cols-[236px_minmax(0,1fr)] max-[860px]:block">
      <aside className="sticky top-0 flex h-screen flex-col border-r border-white/8 bg-[#302b25] px-4 pt-[22px] pb-[18px] text-[#f8f1df] max-[860px]:static max-[860px]:h-auto max-[860px]:border-r-0 max-[860px]:px-3.5 max-[860px]:pt-3 max-[860px]:pb-0">
        <Link className="flex items-center gap-[11px] px-2 pt-[5px] pb-[18px] font-display text-[21px] font-[820] tracking-[-0.02em] no-underline" href={scoped ? root : "/app/servers"}>
          <span aria-hidden="true" className="relative grid size-[30px] place-items-center rounded-[7px] bg-pumpkin shadow-[inset_-5px_-5px_0_rgba(90,35,12,0.24)] before:absolute before:top-[-4px] before:left-1/2 before:size-[7px] before:-translate-x-1/2 before:rounded-t-[2px] before:bg-grass" />
          <span>kurbis</span>
        </Link>

        {server && state ? (
          <div className={scoped
            ? "mx-2 mb-[22px] rounded-xl bg-white/7 p-[13px] max-[860px]:mx-0 max-[860px]:mb-2 max-[860px]:grid max-[860px]:grid-cols-[auto_minmax(0,1fr)] max-[860px]:items-center max-[860px]:gap-x-3 max-[860px]:gap-y-0.5 max-[860px]:rounded-sm max-[860px]:p-2.5"
            : "mx-2 mb-[22px] rounded-xl bg-white/7 p-[13px] max-[860px]:hidden"}>
            <div className="flex items-center gap-2 text-[13px] font-[760]"><i className={`size-2 rounded-[3px] ${stateTone(server.observed_state)}`} /><span>{state.label}</span></div>
            <span className={scoped ? "mt-[7px] block overflow-hidden text-ellipsis whitespace-nowrap text-xs text-[#c9c1b1] max-[860px]:mt-0" : "mt-[7px] block overflow-hidden text-ellipsis whitespace-nowrap text-xs text-[#c9c1b1]"}>{server.java_address}</span>
            {scoped ? <span className="mt-2 block text-[11px] text-[#bcb4a5] max-[860px]:mt-0 max-[860px]:text-[10px] max-[860px]:leading-tight">Server only • no Kurbis account</span> : null}
          </div>
        ) : null}

        {servers.length > 1 ? (
          <label className="mx-2 mb-3 grid gap-1.5 text-[11px] font-[760] text-[#c9c1b1]">
            <span>SERVER</span>
            <select aria-label="Select server" className="min-h-10 min-w-0 rounded-sm border border-white/15 bg-[#403a32] px-2.5 text-[13px] text-[#f8f1df]" value={server?.id ?? ""} onChange={(event) => selectServer(event.target.value)}>
              {servers.map((item) => <option key={item.id} value={item.id}>{item.subdomain}{item.is_default ? " · default" : ""}{serverSessions.some((grant) => grant.server_id === item.id) ? " · shared" : ""}</option>)}
            </select>
          </label>
        ) : null}

        <nav className="grid gap-[3px] max-[860px]:flex max-[860px]:gap-1 max-[860px]:overflow-x-auto max-[860px]:pb-2.5 max-[860px]:[scrollbar-width:none] max-[860px]:[&::-webkit-scrollbar]:hidden max-[640px]:grid max-[640px]:grid-cols-3 max-[640px]:overflow-visible max-[640px]:pb-3" aria-label="Server panel">
          {nav.map(({ href, label, Icon }) => {
            const active = isActive(pathname, href);
            return <Link key={href} href={href} aria-current={active ? "page" : undefined} className={`flex min-h-11 items-center gap-[11px] rounded-sm px-[11px] text-sm font-[660] no-underline max-[860px]:min-h-10 max-[860px]:min-w-max max-[860px]:px-2.5 max-[640px]:min-h-11 max-[640px]:min-w-0 max-[640px]:justify-center max-[640px]:gap-1.5 max-[640px]:px-1.5 max-[640px]:text-[13px] ${active ? "bg-canvas text-[#302b25] hover:bg-[#fff7e8]" : "text-[#d8d0c0] hover:bg-white/7 hover:text-white"}`}><Icon size={18} strokeWidth={2.1} /><span>{label}</span></Link>;
          })}
        </nav>

        <div className={`mt-auto border-t border-white/8 px-2 pt-4 ${scoped
          ? "max-[860px]:mx-0 max-[860px]:mt-0 max-[860px]:border-0 max-[860px]:px-0 max-[860px]:pt-0 max-[860px]:pb-3"
          : "max-[860px]:hidden"}`}>
          {!scoped && server ? <div className="flex items-center gap-2 text-xs text-[#bcb4a5]"><i className={`size-[7px] rounded-full ${eventStatus === "live" ? "bg-[#8eb866]" : eventStatus === "reconnecting" ? "animate-[pulse-status_1.2s_infinite] bg-[#d8964e]" : "bg-[#8d8a82]"}`} /><span>{eventStatus === "live" ? "Live updates connected" : eventStatus === "reconnecting" ? "Reconnecting live updates" : "Connecting live updates"}</span></div> : null}
          {scoped ? <p className="m-0 text-xs leading-relaxed text-[#bcb4a5]">Server-only access</p> : null}
          {scopedGrant ? (
            <div className="mt-3"><RevokeServerAccess
              sessionId={scopedGrant.id}
              onRevoked={() => server && afterRevoke(server.id)}
            /></div>
          ) : null}
          {session ? <div className="mt-3 flex min-w-0 items-center gap-[9px]"><span className="grid size-8 shrink-0 place-items-center rounded-sm bg-[#4d453b] font-[800] text-[#f4c08e]">{session.user.display_name.slice(0, 1).toUpperCase()}</span><span className="grid min-w-0 gap-0.5"><strong className="overflow-hidden text-ellipsis whitespace-nowrap text-xs">{session.user.display_name}</strong><span className="overflow-hidden text-[11px] text-ellipsis whitespace-nowrap text-[#aaa294]">{session.user.email ?? session.user.account_status}</span></span></div> : null}
        </div>
      </aside>
      <main className="min-w-0 px-[clamp(20px,4vw,58px)] pt-[42px] pb-[72px] max-[860px]:px-4 max-[860px]:pt-7 max-[860px]:pb-[54px]"><div className="mx-auto w-[min(1180px,100%)]">{children}</div></main>
    </div>
  );
}
