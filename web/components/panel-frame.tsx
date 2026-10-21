"use client";

import type { LucideIcon } from "lucide-react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import type { ReactNode } from "react";
import { KurbisLogo } from "./kurbis-logo";

export type PanelNavItem = {
  href: string;
  label: string;
  Icon: LucideIcon;
  /** Also current on the pages below `href`. */
  prefix?: boolean;
};

function isActive(pathname: string, item: PanelNavItem) {
  return pathname === item.href || (item.prefix === true && pathname.startsWith(`${item.href}/`));
}

/**
 * The panel's page frame: the dark sidebar (logo, whatever goes above the navigation,
 * the navigation, a footer) and the main column. The server panel and the admin
 * dashboard both use it.
 */
export function PanelFrame({
  homeHref,
  navLabel,
  nav,
  header,
  footer,
  footerClassName = "max-[860px]:hidden",
  children,
}: {
  homeHref: string;
  navLabel: string;
  nav: PanelNavItem[];
  header?: ReactNode;
  footer?: ReactNode;
  /** How the footer behaves on narrow screens. */
  footerClassName?: string;
  children: ReactNode;
}) {
  const pathname = usePathname();
  return (
    <div className="grid min-h-screen grid-cols-[236px_minmax(0,1fr)] max-[860px]:block">
      <aside className="sticky top-0 flex h-screen flex-col border-r border-white/8 bg-[#302b25] px-4 pt-[22px] pb-[18px] text-[#f8f1df] max-[860px]:static max-[860px]:h-auto max-[860px]:border-r-0 max-[860px]:px-3.5 max-[860px]:pt-3 max-[860px]:pb-0">
        <Link className="flex items-center gap-[11px] px-2 pt-[5px] pb-[18px] font-display text-[21px] font-[820] tracking-[-0.02em] no-underline" href={homeHref}>
          <KurbisLogo size={30} className="size-[30px] rounded-[6px] shadow-[0_3px_0_rgba(90,35,12,0.24)]" />
          <span>kurbis</span>
        </Link>

        {header}

        <nav className="grid gap-[3px] max-[860px]:flex max-[860px]:gap-1 max-[860px]:overflow-x-auto max-[860px]:pb-2.5 max-[860px]:[scrollbar-width:none] max-[860px]:[&::-webkit-scrollbar]:hidden max-[640px]:grid max-[640px]:grid-cols-3 max-[640px]:overflow-visible max-[640px]:pb-3" aria-label={navLabel}>
          {nav.map((item) => {
            const active = isActive(pathname, item);
            const { href, label, Icon } = item;
            return <Link key={href} href={href} aria-current={active ? "page" : undefined} className={`flex min-h-11 items-center gap-[11px] rounded-sm px-[11px] text-sm font-[660] no-underline max-[860px]:min-h-10 max-[860px]:min-w-max max-[860px]:px-2.5 max-[640px]:min-h-11 max-[640px]:min-w-0 max-[640px]:justify-center max-[640px]:gap-1.5 max-[640px]:px-1.5 max-[640px]:text-[13px] ${active ? "bg-canvas text-[#302b25] hover:bg-[#fff7e8]" : "text-[#d8d0c0] hover:bg-white/7 hover:text-white"}`}><Icon size={18} strokeWidth={2.1} /><span>{label}</span></Link>;
          })}
        </nav>

        {footer ? <div className={`mt-auto border-t border-white/8 px-2 pt-4 ${footerClassName}`}>{footer}</div> : null}
      </aside>
      <main className="min-w-0 px-[clamp(20px,4vw,58px)] pt-[42px] pb-[72px] max-[860px]:px-4 max-[860px]:pt-7 max-[860px]:pb-[54px]"><div className="mx-auto w-[min(1180px,100%)]">{children}</div></main>
    </div>
  );
}

/** The signed-in account at the foot of the sidebar. */
export function SidebarAccount({ name, detail }: { name: string; detail: string }) {
  return (
    <div className="mt-3 flex min-w-0 items-center gap-[9px]">
      <span className="grid size-8 shrink-0 place-items-center rounded-sm bg-[#4d453b] font-[800] text-[#f4c08e]">{name.slice(0, 1).toUpperCase()}</span>
      <span className="grid min-w-0 gap-0.5">
        <strong className="overflow-hidden text-ellipsis whitespace-nowrap text-xs">{name}</strong>
        <span className="overflow-hidden text-[11px] text-ellipsis whitespace-nowrap text-[#aaa294]">{detail}</span>
      </span>
    </div>
  );
}
