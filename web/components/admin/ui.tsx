"use client";

import { useQuery, type QueryKey } from "@tanstack/react-query";
import Link from "next/link";
import type { ReactNode } from "react";
import { errorMessage } from "../../lib/api/client";
import type { AdminRange } from "../../lib/api/operations";
import { formatDateTime, formatRelative } from "../../lib/format";
import { lifecycle } from "../../lib/lifecycle";
import { useNow } from "../../lib/use-now";
import { Button, Notice, StatusLabel, Surface } from "../ui";

/** The dashboard refreshes what it shows this often while the page is open. */
const REFRESH_MS = 15_000;

/** A dashboard query: refreshed while open; a refetch keeps the previous data on screen. */
export function useAdminQuery<T>(queryKey: QueryKey, queryFn: () => Promise<T>) {
  return useQuery({ queryKey, queryFn, refetchInterval: REFRESH_MS, placeholderData: (previous) => previous });
}

/** Loading and failure for a dashboard query; the content renders once there is data. */
export function Loaded<T>({ query, children }: { query: { data?: T; isPending: boolean; isError: boolean; error: unknown; refetch: () => unknown }; children: (data: T) => ReactNode }) {
  if (query.data !== undefined) return <>{children(query.data)}</>;
  if (query.isError) {
    return (
      <Notice tone="danger">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <span>This could not be loaded: {errorMessage(query.error)}</span>
          <Button size="small" variant="secondary" onClick={() => void query.refetch()}>Retry</Button>
        </div>
      </Notice>
    );
  }
  return <p className="m-0 text-sm text-ink-soft" role="status">Loading…</p>;
}

export function Section({ title, description, actions, children, className = "" }: { title: string; description?: string; actions?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <Surface className={`p-5 ${className}`}>
      <div className="mb-4 flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="m-0 font-display text-lg leading-tight font-[800]">{title}</h2>
          {description ? <p className="mt-1 mb-0 max-w-[70ch] text-xs leading-[1.45] text-ink-soft">{description}</p> : null}
        </div>
        {actions}
      </div>
      {children}
    </Surface>
  );
}

const toneText = {
  good: "text-success",
  warning: "text-warning",
  danger: "text-danger",
  quiet: "text-ink",
} as const;

/** A labelled figure. The tone colours the figure only when it means good or bad. */
export function Stat({ label, value, detail, tone = "quiet", href }: { label: string; value: ReactNode; detail?: ReactNode; tone?: keyof typeof toneText; href?: string }) {
  const body = (
    <>
      <span className="block text-xs font-[700] text-ink-soft">{label}</span>
      <strong className={`mt-1 block text-[26px] leading-tight font-[800] ${toneText[tone]}`}>{value}</strong>
      {detail ? <span className="mt-1 block text-xs text-ink-soft">{detail}</span> : null}
    </>
  );
  const className = "block rounded-md bg-paper p-4 text-ink no-underline shadow-[inset_0_0_0_1px_rgba(83,70,49,0.13),var(--shadow-low)]";
  return href ? <Link href={href} className={`${className} hover:bg-[#fffdf7]`}>{body}</Link> : <div className={className}>{body}</div>;
}

export function StatGrid({ children }: { children: ReactNode }) {
  return <div className="grid grid-cols-[repeat(auto-fill,minmax(170px,1fr))] gap-3">{children}</div>;
}

export type TableRow = { key: string; cells: ReactNode[] };

/** A plain table; rows scroll sideways on narrow screens. */
export function Table({ head, rows, empty }: { head: string[]; rows: TableRow[]; empty: string }) {
  if (rows.length === 0) return <p className="m-0 rounded-sm bg-[#f4eee1] px-4 py-5 text-center text-sm text-ink-soft">{empty}</p>;
  return (
    <div className="-mx-1 overflow-x-auto">
      <table className="w-full border-collapse text-left text-[13px]">
        <thead>
          <tr>{head.map((label) => <th key={label} scope="col" className="border-b border-[#ddd5c4] px-2 py-2 text-[11px] font-[800] tracking-[0.02em] whitespace-nowrap text-ink-soft uppercase">{label}</th>)}</tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={row.key} className="border-b border-[#ece5d6] last:border-0 hover:bg-[#f8f2e5]">
              {row.cells.map((cell, index) => <td key={index} className="px-2 py-2 align-top tabular-nums">{cell}</td>)}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

const ranges: { value: AdminRange; label: string }[] = [
  { value: "hour", label: "Hour" },
  { value: "day", label: "Day" },
  { value: "week", label: "Week" },
  { value: "month", label: "Month" },
];

export const RANGE_GAP_MS: Record<AdminRange, number> = {
  hour: 90_000,
  day: 7.5 * 60_000,
  week: 45 * 60_000,
  month: 3 * 3_600_000,
};

/** A row of mutually exclusive choices. */
export function Segmented<T extends string>({ label, options, value, onChange }: { label: string; options: { value: T; label: string }[]; value: T; onChange: (value: T) => void }) {
  return (
    <div className="inline-flex flex-wrap rounded-sm bg-[#e7dfcd] p-[3px]" role="radiogroup" aria-label={label}>
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          role="radio"
          aria-checked={value === option.value}
          onClick={() => onChange(option.value)}
          className={`min-h-8 cursor-pointer rounded-[6px] border-0 px-3 text-[13px] font-[720] ${value === option.value ? "bg-paper text-ink shadow-[0_1px_3px_rgba(60,47,30,0.15)]" : "bg-transparent text-ink-soft hover:text-ink"}`}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

export function RangePicker({ value, onChange, options = ranges.map((range) => range.value) }: { value: AdminRange; onChange: (range: AdminRange) => void; options?: AdminRange[] }) {
  return <Segmented label="Time range" options={ranges.filter((range) => options.includes(range.value))} value={value} onChange={onChange} />;
}

/** "3 minutes ago", with the exact time on hover. */
export function Ago({ at, empty = "never" }: { at?: string | null; empty?: string }) {
  const now = useNow(Date.now(), 15_000);
  if (!at) return <span className="text-ink-soft">{empty}</span>;
  return <time dateTime={at} title={formatDateTime(at)}>{formatRelative(at, now)}</time>;
}

/** A server's observed state, worded as the panel words it. */
export function ServerState({ state }: { state: string }) {
  const presentation = lifecycle[state as keyof typeof lifecycle];
  return <StatusLabel tone={presentation?.tone ?? "quiet"}>{presentation?.label ?? state}</StatusLabel>;
}

/** A short status word with its dot: good, in progress, failed or idle. */
export function Status({ tone, children }: { tone: "good" | "progress" | "danger" | "quiet" | "warning"; children: ReactNode }) {
  return <StatusLabel tone={tone}>{children}</StatusLabel>;
}

/** A server, linked to one of its panel pages (read-only for an admin). */
export function ServerLink({ id, subdomain, page = "lifecycle" }: { id: string; subdomain?: string | null; page?: "lifecycle" | "ai" | "console" | "plugins" }) {
  if (!subdomain) return <span className="text-ink-soft">deleted server</span>;
  return <Link className="font-[700] text-sky-deep" href={`/manage/${encodeURIComponent(id)}/${page}`}>{subdomain}</Link>;
}

export function AccountLink({ id, name }: { id: string; name?: string | null }) {
  if (!name) return <span className="text-ink-soft">deleted account</span>;
  return <Link className="font-[700] text-sky-deep" href={`/admin/accounts/${encodeURIComponent(id)}`}>{name}</Link>;
}

/** Long text such as an error, cut to a line until it is opened. */
export function Message({ text }: { text?: string | null }) {
  if (!text) return <span className="text-ink-soft">—</span>;
  return (
    <details className="max-w-[60ch]">
      <summary className="cursor-pointer overflow-hidden text-ellipsis whitespace-nowrap [&::-webkit-details-marker]:hidden">{text}</summary>
      <pre className="mt-1.5 mb-0 max-h-72 overflow-auto rounded-sm bg-[#2b2620] p-2.5 text-[11px] whitespace-pre-wrap text-[#f2ead8]">{text}</pre>
    </details>
  );
}

export function Pager({ offset, limit, total, onChange }: { offset: number; limit: number; total: number; onChange: (offset: number) => void }) {
  if (total <= limit) return null;
  return (
    <div className="mt-3 flex items-center justify-between gap-3 text-xs text-ink-soft">
      <span>{offset + 1}–{Math.min(offset + limit, total)} of {total}</span>
      <span className="flex gap-2">
        <Button size="small" variant="secondary" disabled={offset === 0} onClick={() => onChange(Math.max(offset - limit, 0))}>Previous</Button>
        <Button size="small" variant="secondary" disabled={offset + limit >= total} onClick={() => onChange(offset + limit)}>Next</Button>
      </span>
    </div>
  );
}
