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
import { usePathname } from "next/navigation";
import type { ReactNode } from "react";
import { fetchServer, fetchSession } from "../lib/api/queries";
import { lifecycle } from "../lib/lifecycle";
import { queryKeys } from "../lib/query";

const fullNav = [
  { href: "/app", label: "Overview", Icon: Gauge },
  { href: "/app/console", label: "Console", Icon: ScrollText },
  { href: "/app/backups", label: "Backups", Icon: ArchiveRestore },
  { href: "/app/players", label: "Players", Icon: UserRoundCheck },
  { href: "/app/settings", label: "Settings", Icon: Settings2 },
  { href: "/app/account", label: "Account", Icon: CircleUserRound },
];

function isActive(pathname: string, href: string) {
  return href === "/app" ? pathname === href : pathname.startsWith(href);
}

function stateTone(state: string | undefined) {
  if (state === "online") {
    return "bg-[#91bd67] shadow-[0_0_0_3px_rgba(145,189,103,0.13)]";
  }
  if (state === "sleeping") return "bg-[#94a4a5]";
  if (state === "failed") return "bg-[#df7468]";
  return "bg-[#df9d58]";
}

export function AppShell({ children }: { children: ReactNode }) {
  const pathname = usePathname();
  const { data: session } = useQuery({
    queryKey: queryKeys.session,
    queryFn: fetchSession,
  });
  const { data: server } = useQuery({
    queryKey: queryKeys.server,
    queryFn: fetchServer,
  });
  const { data: eventStatus = "idle" } = useQuery<string>({
    queryKey: queryKeys.eventStatus,
    queryFn: async () => "idle",
    enabled: false,
  });
  const nav = server
    ? fullNav
    : [
        { href: "/app/setup", label: "Setup", Icon: WandSparkles },
        { href: "/app/account", label: "Account", Icon: CircleUserRound },
      ];
  const state = server ? lifecycle[server.observed_state] : null;

  return (
    <div className="grid min-h-screen grid-cols-[236px_minmax(0,1fr)] max-[860px]:block">
      <aside className="sticky top-0 flex h-screen flex-col border-r border-white/8 bg-[#302b25] px-4 pt-[22px] pb-[18px] text-[#f8f1df] max-[860px]:static max-[860px]:h-auto max-[860px]:border-r-0 max-[860px]:px-3.5 max-[860px]:pt-3 max-[860px]:pb-0">
        <Link
          className="flex items-center gap-[11px] px-2 pt-[5px] pb-[18px] font-display text-[21px] leading-none font-[820] tracking-[-0.02em] no-underline max-[860px]:px-[5px] max-[860px]:pt-1 max-[860px]:pb-2.5"
          href="/app"
        >
          <span
            className="relative grid size-[30px] place-items-center rounded-[7px] bg-pumpkin shadow-[inset_-5px_-5px_0_rgba(90,35,12,0.24)] before:absolute before:top-[-4px] before:left-1/2 before:size-[7px] before:-translate-x-1/2 before:rounded-t-[2px] before:bg-grass"
            aria-hidden="true"
          />
          <span>kurbis</span>
        </Link>

        {server && state ? (
          <div className="mx-2 mb-[22px] rounded-xl bg-white/7 p-[13px] max-[860px]:hidden">
            <div className="flex items-center gap-2 text-[13px] font-[760]">
              <i
                className={`size-2 rounded-[3px] ${stateTone(server.observed_state)}`}
              />
              <span>{state.label}</span>
            </div>
            <span className="mt-[7px] block overflow-hidden text-ellipsis whitespace-nowrap text-xs text-[#c9c1b1]">
              {server.java_address}
            </span>
          </div>
        ) : null}

        <nav
          className="grid gap-[3px] max-[860px]:flex max-[860px]:gap-1 max-[860px]:overflow-x-auto max-[860px]:pb-2.5 max-[860px]:[scrollbar-width:none] max-[860px]:[&::-webkit-scrollbar]:hidden"
          aria-label="Server panel"
        >
          {nav.map(({ href, label, Icon }) => {
            const active = isActive(pathname, href);
            return (
              <Link
                key={href}
                href={href}
                className={`flex min-h-11 items-center gap-[11px] rounded-sm px-[11px] text-sm font-[660] no-underline max-[860px]:min-h-10 max-[860px]:min-w-max max-[860px]:px-2.5 ${
                  active
                    ? "bg-canvas text-[#302b25] hover:bg-[#fff7e8]"
                    : "text-[#d8d0c0] hover:bg-white/7 hover:text-white"
                }`}
                aria-current={active ? "page" : undefined}
              >
                <Icon size={18} strokeWidth={2.1} />
                <span>{label}</span>
              </Link>
            );
          })}
        </nav>

        <div className="mt-auto border-t border-white/8 px-2 pt-4 max-[860px]:hidden">
          {server ? (
            <div className="flex items-center gap-2 text-xs text-[#bcb4a5]">
              <i
                className={`size-[7px] rounded-full ${
                  eventStatus === "live"
                    ? "bg-[#8eb866]"
                    : eventStatus === "reconnecting"
                      ? "animate-[pulse-status_1.2s_infinite] bg-[#d8964e]"
                      : "bg-[#8d8a82]"
                }`}
              />
              <span>
                {eventStatus === "live"
                  ? "Live updates connected"
                  : eventStatus === "reconnecting"
                    ? "Reconnecting live updates"
                    : "Connecting live updates"}
              </span>
            </div>
          ) : null}
          {session ? (
            <div className="mt-3 flex min-w-0 items-center gap-[9px]">
              <span className="grid size-8 shrink-0 place-items-center rounded-sm bg-[#4d453b] font-[800] text-[#f4c08e]">
                {session.user.display_name.slice(0, 1).toUpperCase()}
              </span>
              <span className="grid min-w-0 gap-0.5">
                <strong className="overflow-hidden text-ellipsis whitespace-nowrap text-xs">
                  {session.user.display_name}
                </strong>
                <span className="overflow-hidden text-[11px] text-ellipsis whitespace-nowrap text-[#aaa294]">
                  {session.user.email ?? session.user.account_status}
                </span>
              </span>
            </div>
          ) : null}
        </div>
      </aside>
      <main className="min-w-0 px-[clamp(20px,4vw,58px)] pt-[42px] pb-[72px] max-[860px]:px-4 max-[860px]:pt-7 max-[860px]:pb-[54px]">
        <div className="mx-auto w-[min(1180px,100%)]">{children}</div>
      </main>
    </div>
  );
}
