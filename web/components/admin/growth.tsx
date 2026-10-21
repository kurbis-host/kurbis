"use client";

import { useState } from "react";
import type { components } from "../../lib/api/generated";
import { browserApi } from "../../lib/api/operations";
import { formatCount, formatShare, formatUsd, formatUtcDay } from "../../lib/format";
import { queryKeys } from "../../lib/query";
import { ColumnChart } from "../charts";
import { PageHeader } from "../ui";
import { Loaded, Section, Segmented, Table, useAdminQuery } from "./ui";

type Day = components["schemas"]["AdminDay"];

const windows = [
  { value: "30", label: "30 days" },
  { value: "90", label: "90 days" },
  { value: "365", label: "A year" },
  { value: "all", label: "Everything" },
] as const;

type Window = (typeof windows)[number]["value"];

function sum(days: Day[], pick: (day: Day) => number) {
  return days.reduce((total, day) => total + pick(day), 0);
}

/** A cohort's share, shaded darker the more of it came back. */
function Retention({ returned, accounts }: { returned?: number; accounts: number }) {
  if (returned === undefined) return <span className="text-ink-soft">·</span>;
  const share = accounts ? returned / accounts : 0;
  return (
    <span
      className="inline-block min-w-12 rounded-[4px] px-1.5 py-0.5 text-center"
      style={{ background: `rgba(42, 120, 214, ${0.08 + share * 0.5})` }}
      title={`${formatCount(returned)} of ${formatCount(accounts)}`}
    >
      {formatShare(share)}
    </span>
  );
}

export function AdminGrowthPage() {
  const query = useAdminQuery(queryKeys.admin.growth, browserApi.getAdminGrowth);
  const [shown, setShown] = useState<Window>("30");
  return (
    <>
      <PageHeader
        title="Growth"
        description="Each UTC day, kept for good. Days before the dashboard existed are counted from what is still there, so deleted servers and their wakes are missing from them, and so are their peaks."
        actions={<Segmented label="Days shown" options={[...windows]} value={shown} onChange={setShown} />}
      />
      <Loaded query={query}>
        {(growth) => {
          const days = shown === "all" ? growth.days : growth.days.slice(-Number(shown));
          const columns = (pick: (day: Day) => number) => days.map((day) => ({ label: formatUtcDay(day.day), value: pick(day) }));
          return (
            <div className="grid grid-cols-[minmax(0,1fr)] gap-5">
              <Section title="Every day">
                <div className="grid grid-cols-[minmax(0,1fr)] gap-6 lg:grid-cols-2">
                  <ColumnChart title="Signups" columns={columns((day) => day.signups)} format={formatCount} total={formatCount(sum(days, (day) => day.signups))} />
                  <ColumnChart title="Servers created" columns={columns((day) => day.servers_created)} format={formatCount} total={formatCount(sum(days, (day) => day.servers_created))} />
                  <ColumnChart title="Wakes" columns={columns((day) => day.wakes)} format={formatCount} total={formatCount(sum(days, (day) => day.wakes))} />
                  <ColumnChart title="Servers woken" columns={columns((day) => day.servers_woken)} format={formatCount} />
                  <ColumnChart title="Most players at once" columns={columns((day) => day.peak_players_online ?? 0)} format={formatCount} total={`peak ${formatCount(Math.max(0, ...days.map((day) => day.peak_players_online ?? 0)))}`} />
                  <ColumnChart title="Most servers online at once" columns={columns((day) => day.peak_servers_online ?? 0)} format={formatCount} />
                  <ColumnChart title="AI spend" columns={columns((day) => day.ai_cost_micros)} format={formatUsd} axisFormat={formatUsd} total={formatUsd(sum(days, (day) => day.ai_cost_micros))} />
                </div>
              </Section>
              <Section title="Do they come back?" description="Of the accounts made each UTC week, the share whose server was woken in each week after signing up. Week 0 is the week of signup.">
                <Table
                  head={["Signup week", "Accounts", ...Array.from({ length: 8 }, (_, index) => `Week ${index}`)]}
                  empty="No signups in the last twelve weeks."
                  rows={growth.cohorts.map((cohort) => ({
                    key: cohort.week,
                    cells: [
                      formatUtcDay(cohort.week),
                      formatCount(cohort.accounts),
                      ...Array.from({ length: 8 }, (_, index) => <Retention key={index} returned={cohort.returned[index]} accounts={cohort.accounts} />),
                    ],
                  }))}
                />
              </Section>
            </div>
          );
        }}
      </Loaded>
    </>
  );
}
