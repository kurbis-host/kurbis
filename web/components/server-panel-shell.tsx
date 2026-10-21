"use client";

import { useQuery } from "@tanstack/react-query";
import {
  Activity,
  ArchiveRestore,
  CircleUserRound,
  Gauge,
  Puzzle,
  ScrollText,
  Settings2,
  ShieldCheck,
  Sparkles,
  UserRoundCheck,
  WandSparkles,
} from "lucide-react";
import { usePathname, useRouter } from "next/navigation";
import type { ReactNode } from "react";
import type { components } from "../lib/api/generated";
import { browserApi } from "../lib/api/operations";
import { lifecycle } from "../lib/lifecycle";
import { queryKeys, selectedServerId } from "../lib/query";
import { PanelFrame, SidebarAccount, type PanelNavItem } from "./panel-frame";
import { RevokeServerAccess } from "./revoke-server-access";

type Server = components["schemas"]["Server"];
type ServerScopedSession = components["schemas"]["ServerScopedSession"];
type Session = components["schemas"]["Session"];

const serverNav = [
  { suffix: "", label: "Overview", Icon: Gauge },
  { suffix: "/console", label: "Console", Icon: ScrollText },
  { suffix: "/backups", label: "Backups", Icon: ArchiveRestore },
  { suffix: "/players", label: "Players", Icon: UserRoundCheck },
  { suffix: "/plugins", label: "Plugins", Icon: Puzzle },
  { suffix: "/ai", label: "AI", Icon: Sparkles },
  { suffix: "/settings", label: "Settings", Icon: Settings2 },
] as const;

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
  adminView = false,
  children,
}: {
  servers?: Server[];
  serverSessions?: ServerScopedSession[];
  server?: Server;
  session?: Session;
  scoped?: boolean;
  /** An admin reading a server that is not theirs: no server switcher, a read-only note. */
  adminView?: boolean;
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
  const isAdmin = session?.is_admin === true;
  const nav: PanelNavItem[] = [
    ...(server
      ? [
        ...serverNav.map(({ suffix, label, Icon }) => ({ href: `${root}${suffix}`, label, Icon })),
        ...(isAdmin ? [{ href: `${root}/lifecycle`, label: "Lifecycle", Icon: Activity }] : []),
      ]
      : [{ href: "/app/setup", label: "Claim server", Icon: WandSparkles }]),
    ...(!scoped ? [{ href: "/app/account", label: "Account", Icon: CircleUserRound, prefix: true }] : []),
    ...(isAdmin ? [{ href: "/admin", label: "Admin", Icon: ShieldCheck }] : []),
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

  const header = <>
    {server && state ? (
      <div className={scoped
        ? "mx-2 mb-[22px] rounded-xl bg-white/7 p-[13px] max-[860px]:mx-0 max-[860px]:mb-2 max-[860px]:grid max-[860px]:grid-cols-[auto_minmax(0,1fr)] max-[860px]:items-center max-[860px]:gap-x-3 max-[860px]:gap-y-0.5 max-[860px]:rounded-sm max-[860px]:p-2.5"
        : "mx-2 mb-[22px] rounded-xl bg-white/7 p-[13px] max-[860px]:hidden"}>
        <div className="flex items-center gap-2 text-[13px] font-[760]"><i className={`size-2 rounded-[3px] ${stateTone(server.observed_state)}`} /><span>{state.label}</span></div>
        <span className={scoped ? "mt-[7px] block overflow-hidden text-ellipsis whitespace-nowrap text-xs text-[#c9c1b1] max-[860px]:mt-0" : "mt-[7px] block overflow-hidden text-ellipsis whitespace-nowrap text-xs text-[#c9c1b1]"}>{server.java_address}</span>
        {scoped ? <span className="mt-2 block text-[11px] text-[#bcb4a5] max-[860px]:mt-0 max-[860px]:text-[10px] max-[860px]:leading-tight">Server only • no Kurbis account</span> : null}
      </div>
    ) : null}

    {adminView ? (
      <p className="mx-2 mb-3 rounded-sm bg-[#5b3a1f] px-2.5 py-2 text-[11px] leading-snug font-[700] text-[#f4c08e] max-[860px]:mx-0">
        Admin view, read-only. Nothing here can be changed.
      </p>
    ) : servers.length > 1 ? (
      <label className="mx-2 mb-3 grid gap-1.5 text-[11px] font-[760] text-[#c9c1b1]">
        <span>SERVER</span>
        <select aria-label="Select server" className="min-h-10 min-w-0 rounded-sm border border-white/15 bg-[#403a32] px-2.5 text-[13px] text-[#f8f1df]" value={server?.id ?? ""} onChange={(event) => selectServer(event.target.value)}>
          {servers.map((item) => <option key={item.id} value={item.id}>{item.subdomain}{item.is_default ? " · default" : ""}{serverSessions.some((grant) => grant.server_id === item.id) ? " · shared" : ""}</option>)}
        </select>
      </label>
    ) : null}
  </>;

  const footer = <>
    {!scoped && server ? <div className="flex items-center gap-2 text-xs text-[#bcb4a5]"><i className={`size-[7px] rounded-full ${eventStatus === "live" ? "bg-[#8eb866]" : eventStatus === "reconnecting" ? "animate-[pulse-status_1.2s_infinite] bg-[#d8964e]" : "bg-[#8d8a82]"}`} /><span>{eventStatus === "live" ? "Live updates connected" : eventStatus === "reconnecting" ? "Reconnecting live updates" : "Connecting live updates"}</span></div> : null}
    {scoped ? <p className="m-0 text-xs leading-relaxed text-[#bcb4a5]">Server-only access</p> : null}
    {scopedGrant ? (
      <div className="mt-3"><RevokeServerAccess
        sessionId={scopedGrant.id}
        onRevoked={() => server && afterRevoke(server.id)}
      /></div>
    ) : null}
    {session ? <SidebarAccount name={session.user.display_name} detail={session.user.email ?? session.user.account_status} /> : null}
  </>;

  return (
    <PanelFrame
      homeHref={scoped ? root : "/app/servers"}
      navLabel="Server panel"
      nav={nav}
      header={header}
      footer={footer}
      footerClassName={scoped
        ? "max-[860px]:mx-0 max-[860px]:mt-0 max-[860px]:border-0 max-[860px]:px-0 max-[860px]:pt-0 max-[860px]:pb-3"
        : "max-[860px]:hidden"}
    >
      {children}
    </PanelFrame>
  );
}
