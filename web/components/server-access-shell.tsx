"use client";

import { useQuery } from "@tanstack/react-query";
import { ArchiveRestore, Gauge, ScrollText, Settings2, UserRoundCheck } from "lucide-react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import type { ReactNode } from "react";
import { fetchServer } from "../lib/api/queries";
import type { Server } from "../lib/contracts";
import { lifecycle } from "../lib/lifecycle";
import { queryKeys } from "../lib/query";

const nav = [
  { suffix: "", label: "Overview", Icon: Gauge },
  { suffix: "/console", label: "Logs", Icon: ScrollText },
  { suffix: "/backups", label: "Snapshots", Icon: ArchiveRestore },
  { suffix: "/players", label: "Players & whitelist", Icon: UserRoundCheck },
  { suffix: "/settings", label: "Settings", Icon: Settings2 },
];

function tone(state: string) {
  if (state === "online") return "bg-[#91bd67]";
  if (state === "sleeping") return "bg-[#94a4a5]";
  if (state === "failed") return "bg-[#df7468]";
  return "bg-[#df9d58]";
}

export function ServerAccessShell({ server: initialServer, children }: { server: Server; children: ReactNode }) {
  const pathname = usePathname();
  const { data: server = initialServer } = useQuery({
    queryKey: queryKeys.server(initialServer.id),
    queryFn: () => fetchServer(initialServer.id),
    initialData: initialServer,
  });
  const state = lifecycle[server.observed_state];
  const root = `/manage/${server.id}`;

  return (
    <div className="grid min-h-screen grid-cols-[236px_minmax(0,1fr)] max-[860px]:block">
      <aside className="sticky top-0 flex h-screen flex-col border-r border-white/8 bg-[#302b25] px-4 pt-[22px] pb-[18px] text-[#f8f1df] max-[860px]:static max-[860px]:h-auto max-[860px]:border-r-0 max-[860px]:px-3.5 max-[860px]:pt-3 max-[860px]:pb-0">
        <Link className="flex items-center gap-[11px] px-2 pt-[5px] pb-[18px] font-display text-[21px] font-[820] tracking-[-0.02em] no-underline" href={root}>
          <span aria-hidden="true" className="relative grid size-[30px] place-items-center rounded-[7px] bg-pumpkin shadow-[inset_-5px_-5px_0_rgba(90,35,12,0.24)] before:absolute before:top-[-4px] before:left-1/2 before:size-[7px] before:-translate-x-1/2 before:rounded-t-[2px] before:bg-grass" />
          <span>kurbis</span>
        </Link>
        <div className="mx-2 mb-[22px] rounded-xl bg-white/7 p-[13px] max-[860px]:mx-0 max-[860px]:mb-2 max-[860px]:grid max-[860px]:grid-cols-[auto_minmax(0,1fr)] max-[860px]:items-center max-[860px]:gap-x-3 max-[860px]:gap-y-0.5 max-[860px]:rounded-sm max-[860px]:p-2.5">
          <div className="flex items-center gap-2 text-[13px] font-[760] max-[860px]:row-span-2"><i className={`size-2 rounded-[3px] ${tone(server.observed_state)}`} /><span>{state.label}</span></div>
          <span className="mt-[7px] block overflow-hidden text-ellipsis whitespace-nowrap text-xs text-[#c9c1b1] max-[860px]:mt-0">{server.java_address}</span>
          <span className="mt-2 block text-[11px] text-[#bcb4a5] max-[860px]:mt-0 max-[860px]:text-[10px] max-[860px]:leading-tight">Server only • no Kurbis account</span>
        </div>
        <nav className="grid gap-[3px] max-[860px]:flex max-[860px]:gap-1 max-[860px]:overflow-x-auto max-[860px]:pb-2.5 max-[640px]:grid max-[640px]:grid-cols-3" aria-label="Server panel">
          {nav.map(({ suffix, label, Icon }) => {
            const href = `${root}${suffix}`;
            const active = pathname === href;
            return <Link key={href} href={href} aria-current={active ? "page" : undefined} className={`flex min-h-11 items-center gap-[11px] rounded-sm px-[11px] text-sm font-[660] no-underline max-[860px]:min-w-max max-[860px]:px-2.5 max-[640px]:min-w-0 max-[640px]:justify-center max-[640px]:px-1.5 max-[640px]:text-[12px] ${active ? "bg-canvas text-[#302b25]" : "text-[#d8d0c0] hover:bg-white/7 hover:text-white"}`}>
              <Icon size={18} strokeWidth={2.1} /><span>{label}</span>
            </Link>;
          })}
        </nav>
        <div className="mt-auto border-t border-white/8 px-2 pt-4 text-xs leading-relaxed text-[#bcb4a5] max-[860px]:hidden">
          This temporary link grants access to this server for up to one hour.
        </div>
      </aside>
      <main className="min-w-0 px-[clamp(20px,4vw,58px)] pt-[42px] pb-[72px] max-[860px]:px-4 max-[860px]:pt-7 max-[860px]:pb-[54px]">
        <div className="mx-auto w-[min(1180px,100%)]">{children}</div>
      </main>
    </div>
  );
}
