"use client";

import { Activity, AlertTriangle, Bot, GitPullRequest, LayoutDashboard, Server, Sprout, UsersRound, Cpu, ArrowLeft } from "lucide-react";
import Link from "next/link";
import type { ReactNode } from "react";
import type { components } from "../../lib/api/generated";
import { PanelFrame, SidebarAccount, type PanelNavItem } from "../panel-frame";

type Session = components["schemas"]["Session"];

const nav: PanelNavItem[] = [
  { href: "/admin", label: "Overview", Icon: LayoutDashboard },
  { href: "/admin/servers", label: "Servers", Icon: Server, prefix: true },
  { href: "/admin/accounts", label: "Accounts", Icon: UsersRound, prefix: true },
  { href: "/admin/workers", label: "Workers", Icon: Cpu },
  { href: "/admin/failures", label: "Failures", Icon: AlertTriangle },
  { href: "/admin/ci", label: "Previews", Icon: GitPullRequest },
  { href: "/admin/ai", label: "AI", Icon: Bot },
  { href: "/admin/growth", label: "Growth", Icon: Sprout },
];

export function AdminShell({ session, children }: { session: Session; children: ReactNode }) {
  return (
    <PanelFrame
      homeHref="/admin"
      navLabel="Admin"
      nav={nav}
      header={
        <p className="mx-2 mb-4 flex items-center gap-2 text-[11px] font-[800] tracking-[0.08em] text-[#f4c08e] uppercase max-[860px]:mx-0 max-[860px]:mb-2">
          <Activity size={14} aria-hidden="true" /> Operator dashboard
        </p>
      }
      footer={<>
        <Link href="/app/servers" className="inline-flex items-center gap-1.5 text-xs text-[#d8d0c0] no-underline hover:text-white"><ArrowLeft size={14} aria-hidden="true" /> Back to the panel</Link>
        <SidebarAccount name={session.user.display_name} detail="Admin · read-only" />
      </>}
    >
      {children}
    </PanelFrame>
  );
}
