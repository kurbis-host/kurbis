"use client";

import Link from "next/link";
import { useState } from "react";
import type { components } from "../../lib/api/generated";
import { browserApi, type AdminRange } from "../../lib/api/operations";
import { formatBytes, formatCores, formatCount, formatShare, formatUsd } from "../../lib/format";
import { queryKeys } from "../../lib/query";
import { TimeChart } from "../charts";
import { PageHeader } from "../ui";
import { Ago, Loaded, Message, RANGE_GAP_MS, RangePicker, Section, ServerLink, Stat, StatGrid, Status, Table, useAdminQuery } from "./ui";

type Overview = components["schemas"]["AdminOverview"];
type FleetSample = components["schemas"]["FleetSample"];

type Check = { label: string; tone: "good" | "warning" | "danger"; value: string; detail?: React.ReactNode; href?: string };

function checks(overview: Overview): Check[] {
  const tasks = overview.tasks;
  const troubled = tasks.filter((task) => task.state !== "running" || task.consecutive_failures > 0);
  const failingRequired = troubled.some((task) => task.required && task.state !== "running");
  const pausedLimits = overview.github_rate_limits.filter((limit) => limit.paused_until && new Date(limit.paused_until).getTime() > Date.now());
  const { workers, start_queue: queue, servers, operations } = overview;
  return [
    {
      label: "Background tasks",
      tone: failingRequired ? "danger" : troubled.length ? "warning" : "good",
      value: `${tasks.length - troubled.length} of ${tasks.length} running cleanly`,
    },
    {
      label: "Workers",
      tone: workers.live === 0 ? "danger" : workers.live < workers.total ? "warning" : "good",
      value: `${workers.live} of ${workers.total} live`,
      detail: workers.draining ? `${workers.draining} draining` : undefined,
      href: "/admin/workers",
    },
    {
      label: "Start queue",
      tone: queue.depth > 0 ? "warning" : "good",
      value: queue.depth ? `${formatCount(queue.depth)} waiting` : "empty",
      detail: queue.oldest_requested_at ? <>oldest asked <Ago at={queue.oldest_requested_at} /></> : undefined,
      href: "/admin/servers?filter=queued",
    },
    {
      label: "Failed servers",
      tone: servers.failed > 0 ? "danger" : "good",
      value: formatCount(servers.failed),
      href: "/admin/servers?filter=failed",
    },
    {
      label: "Stuck servers",
      tone: servers.stuck > 0 ? "warning" : "good",
      value: formatCount(servers.stuck),
      detail: "in a passing state for over 10 minutes",
      href: "/admin/servers?filter=stuck",
    },
    {
      label: "Operations",
      tone: operations.failed_last_day > 0 ? "warning" : "good",
      value: `${formatCount(operations.running)} running, ${formatCount(operations.retrying)} retrying`,
      detail: `${formatCount(operations.failed_last_day)} failed in the last day`,
      href: "/admin/failures",
    },
    {
      label: "GitHub",
      tone: pausedLimits.length ? "warning" : "good",
      value: pausedLimits.length ? "paused by the rate limit" : overview.github_rate_limits.length ? "within its rate limit" : "not polled",
      detail: pausedLimits[0]?.paused_until ? <>until <Ago at={pausedLimits[0].paused_until} /></> : undefined,
      href: "/admin/ci",
    },
  ];
}

const toneLabel = { good: "Fine", warning: "Look", danger: "Broken" } as const;

function Health({ overview }: { overview: Overview }) {
  const troubled = overview.tasks.filter((task) => task.state !== "running" || task.consecutive_failures > 0);
  return (
    <Section title="Health" description="Red needs action now, amber needs a look.">
      <ul className="m-0 grid list-none gap-2 p-0 sm:grid-cols-2 xl:grid-cols-3">
        {checks(overview).map((check) => {
          const body = (
            <>
              <span className="flex items-center justify-between gap-2 text-xs font-[760] text-ink-soft">
                {check.label}
                <Status tone={check.tone === "good" ? "good" : check.tone === "warning" ? "warning" : "danger"}>{toneLabel[check.tone]}</Status>
              </span>
              <strong className="mt-1 block text-[15px] font-[780]">{check.value}</strong>
              {check.detail ? <span className="mt-0.5 block text-xs text-ink-soft">{check.detail}</span> : null}
            </>
          );
          const className = `block rounded-sm px-3 py-2.5 text-ink no-underline ${check.tone === "danger" ? "bg-[#f4dfd8]" : check.tone === "warning" ? "bg-[#f3e8d0]" : "bg-[#eef0e2]"}`;
          return <li key={check.label}>{check.href ? <Link className={`${className} hover:brightness-[0.98]`} href={check.href}>{body}</Link> : <div className={className}>{body}</div>}</li>;
        })}
      </ul>
      {troubled.length ? (
        <div className="mt-4">
          <Table
            head={["Task", "State", "Failures", "Last success", "Last error"]}
            empty=""
            rows={troubled.map((task) => ({
              key: task.name,
              cells: [
                <span key="name" className="font-mono text-xs">{task.name}{task.required ? "" : " (optional)"}</span>,
                <Status key="state" tone={task.state === "running" ? "good" : task.state === "fatal" ? "danger" : "warning"}>{task.state.replaceAll("_", " ")}</Status>,
                formatCount(task.consecutive_failures),
                <Ago key="success" at={task.last_success} />,
                <Message key="error" text={task.last_error} />,
              ],
            }))}
          />
        </div>
      ) : null}
    </Section>
  );
}

function share(used?: number | null, total?: number | null) {
  return used !== null && used !== undefined && total ? used / total : null;
}

function FleetCharts() {
  const [range, setRange] = useState<AdminRange>("day");
  const query = useAdminQuery(queryKeys.admin.fleet(range), () => browserApi.getAdminFleet(range));
  const gapMs = RANGE_GAP_MS[range];
  const series = (pick: (sample: FleetSample) => number | null) =>
    (query.data ?? []).map((sample) => ({ at: sample.minute, value: pick(sample) }));
  return (
    <Section
      title="The service over time"
      description="Each point is the busiest minute of its stretch for servers and players, and the average for usage."
      actions={<RangePicker value={range} onChange={setRange} />}
    >
      <div className={`grid gap-6 sm:grid-cols-2 xl:grid-cols-3 ${query.isFetching && query.isPlaceholderData ? "opacity-60" : ""}`}>
        <TimeChart title="Servers online" points={series((sample) => sample.servers_online)} format={formatCount} gapMs={gapMs} />
        <TimeChart title="Players online" points={series((sample) => sample.players_online)} format={formatCount} gapMs={gapMs} />
        <TimeChart title="CPU of live workers" points={series((sample) => share(sample.cpu_used_millis, sample.cpu_total_millis))} format={formatShare} axisFormat={formatShare} max={1} gapMs={gapMs} />
        <TimeChart title="Memory of live workers" points={series((sample) => share(sample.memory_used_bytes, sample.memory_total_bytes))} format={formatShare} axisFormat={formatShare} max={1} gapMs={gapMs} />
        <TimeChart title="Average TPS" points={series((sample) => sample.tps ?? null)} format={(value) => value.toFixed(1)} max={20} gapMs={gapMs} />
      </div>
    </Section>
  );
}

function Storage({ overview }: { overview: Overview }) {
  const { storage } = overview;
  return (
    <Section title="Storage" description="The database's biggest tables include their indexes.">
      <StatGrid>
        <Stat label="Snapshots" value={formatBytes(storage.snapshot_bytes)} detail={`${formatCount(storage.snapshots)} complete`} />
        <Stat label="Worlds as reported" value={formatBytes(storage.server_bytes)} />
        <Stat label="Database" value={formatBytes(storage.database_bytes)} />
      </StatGrid>
      <div className="mt-4 grid gap-5 lg:grid-cols-2">
        <Table
          head={["Table", "Rows", "Size"]}
          empty="No tables."
          rows={storage.largest_tables.map((table) => ({ key: table.name, cells: [<span key="name" className="font-mono text-xs">{table.name}</span>, formatCount(table.rows), formatBytes(table.bytes)] }))}
        />
        <Table
          head={["Largest worlds", "Size"]}
          empty="No servers yet."
          rows={storage.largest_servers.map((server) => ({ key: server.server_id, cells: [<ServerLink key="server" id={server.server_id} subdomain={server.subdomain} />, formatBytes(server.storage_bytes)] }))}
        />
      </div>
    </Section>
  );
}

export function AdminOverviewPage() {
  const query = useAdminQuery(queryKeys.admin.overview, browserApi.getAdminOverview);
  return (
    <>
      <PageHeader title="Overview" description="Whether everything is fine, and how busy the service is. It refreshes every 15 seconds." />
      <Loaded query={query}>
        {(overview) => {
          const { fleet, accounts } = overview;
          const cpu = share(fleet.cpu_used_millis, fleet.cpu_total_millis);
          const memory = share(fleet.memory_used_bytes, fleet.memory_total_bytes);
          return (
            <div className="grid grid-cols-[minmax(0,1fr)] gap-5">
              <Health overview={overview} />
              <StatGrid>
                <Stat label="Servers online" value={formatCount(fleet.servers_online)} detail={`of ${formatCount(overview.servers.total)}`} href="/admin/servers?filter=online" />
                <Stat label="Players online" value={formatCount(fleet.players_online)} />
                <Stat label="CPU in use" value={cpu === null ? "—" : formatShare(cpu)} detail={fleet.cpu_used_millis != null && fleet.cpu_total_millis ? `${formatCores(fleet.cpu_used_millis)} of ${formatCores(fleet.cpu_total_millis)}` : "no fresh samples"} />
                <Stat label="Memory in use" value={memory === null ? "—" : formatShare(memory)} detail={fleet.memory_used_bytes != null && fleet.memory_total_bytes ? `${formatBytes(fleet.memory_used_bytes)} of ${formatBytes(fleet.memory_total_bytes)}` : "no fresh samples"} />
                <Stat label="Average TPS" value={fleet.tps == null ? "—" : fleet.tps.toFixed(1)} tone={fleet.tps == null ? "quiet" : fleet.tps < 15 ? "danger" : fleet.tps < 19 ? "warning" : "good"} />
                <Stat label="Accounts" value={formatCount(accounts.total)} detail={`${formatCount(accounts.signups_last_day)} today, ${formatCount(accounts.signups_last_week)} this week${accounts.suspended ? `, ${formatCount(accounts.suspended)} suspended` : ""}`} href="/admin/accounts" />
                <Stat label="AI this month" value={formatUsd(overview.ai_cost_this_month_micros)} href="/admin/ai" />
              </StatGrid>
              <FleetCharts />
              <Section title="Servers by state">
                <div className="flex flex-wrap gap-2">
                  {overview.servers_by_state.map((entry) => (
                    <Link key={entry.state} href={`/admin/servers?filter=${["online", "sleeping", "failed", "queued"].includes(entry.state) ? entry.state : "all"}`} className="rounded-sm bg-[#eee7d8] px-3 py-2 text-sm text-ink no-underline hover:bg-[#e7dfcd]">
                      <strong>{formatCount(entry.count)}</strong> {entry.state}
                    </Link>
                  ))}
                </div>
              </Section>
              <Storage overview={overview} />
            </div>
          );
        }}
      </Loaded>
    </>
  );
}
