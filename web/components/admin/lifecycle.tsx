"use client";

import { useState } from "react";
import { browserApi, type AdminRange } from "../../lib/api/operations";
import { formatBytes, formatCores, formatCount, formatDateTime } from "../../lib/format";
import { queryKeys } from "../../lib/query";
import { TimeChart } from "../charts";
import { PageHeader } from "../ui";
import { AccountLink, Ago, Loaded, Message, RANGE_GAP_MS, RangePicker, Section, ServerState, Stat, StatGrid, Status, Table, useAdminQuery } from "./ui";

function operationTone(status: string) {
  if (status === "succeeded") return "good" as const;
  if (status === "failed") return "danger" as const;
  if (status === "cancelled") return "quiet" as const;
  return "progress" as const;
}

/** Admin only: how a server moves through its lifecycle, and where it runs. */
export function ServerLifecycle({ serverId }: { serverId: string }) {
  const [range, setRange] = useState<AdminRange>("day");
  const query = useAdminQuery(queryKeys.admin.server(serverId, range), () => browserApi.getAdminServer(serverId, range));
  return (
    <>
      <PageHeader title="Lifecycle" description="Admin only. The owner, placement, operations, snapshots and samples of this server." />
      <Loaded query={query}>
        {(detail) => {
          const { server, placement, start_request: queued } = detail;
          const gapMs = RANGE_GAP_MS[range];
          const series = (pick: (sample: (typeof detail.samples)[number]) => number | null | undefined) =>
            detail.samples.map((sample) => ({ at: sample.minute, value: pick(sample) ?? null }));
          return (
            <div className="grid grid-cols-[minmax(0,1fr)] gap-5">
              <StatGrid>
                <Stat label="State" value={<ServerState state={server.observed_state} />} detail={`wants ${server.desired_state}`} />
                <Stat label="Owner" value={<AccountLink id={server.owner_id} name={server.owner_name} />} detail={server.owner_status} />
                <Stat label="Worker" value={placement?.worker_name ?? "none"} detail={placement ? `generation ${placement.generation}, ${placement.active ? "active" : "released"}, ports ${placement.java_port ?? "—"}/${placement.bedrock_port ?? "—"}` : undefined} />
                <Stat label="Last woken" value={<Ago at={server.last_woken_at} />} detail={`created ${formatDateTime(server.created_at)}`} />
                {queued ? <Stat label="Start queue" value={`${formatCount(queued.ahead)} ahead`} tone="warning" detail={<>{queued.reason}, asked <Ago at={queued.requested_at} /></>} /> : null}
              </StatGrid>
              {server.last_error ? (
                <Section title="Last error">
                  <pre className="m-0 overflow-auto rounded-sm bg-[#2b2620] p-3 text-xs whitespace-pre-wrap text-[#f2ead8]">{server.last_error}</pre>
                </Section>
              ) : null}
              <Section title="Running" description="Its most players and average usage, a point per stretch, while it ran." actions={<RangePicker value={range} onChange={setRange} />}>
                <div className={`grid gap-6 sm:grid-cols-2 ${query.isFetching && query.isPlaceholderData ? "opacity-60" : ""}`}>
                  <TimeChart title="Players" points={series((sample) => sample.players_online)} format={formatCount} gapMs={gapMs} emptyText="It did not run in this range." />
                  <TimeChart title="CPU" points={series((sample) => sample.cpu_used_millis)} format={formatCores} axisFormat={formatCores} gapMs={gapMs} emptyText="No CPU samples in this range." />
                  <TimeChart title="Memory" points={series((sample) => sample.memory_used_bytes)} format={formatBytes} axisFormat={formatBytes} gapMs={gapMs} emptyText="No memory samples in this range." />
                  <TimeChart title="TPS" points={series((sample) => sample.tps)} format={(value) => value.toFixed(1)} max={20} gapMs={gapMs} emptyText="No TPS samples in this range." />
                </div>
              </Section>
              <Section title="Operations" description="The latest 50, newest first.">
                <Table
                  head={["Kind", "Status", "Attempt", "Worker", "Asked", "Changed", "Error"]}
                  empty="No operations yet."
                  rows={detail.operations.map((operation) => ({
                    key: operation.id,
                    cells: [
                      `${operation.kind} · gen ${operation.generation}`,
                      <Status key="status" tone={operationTone(operation.status)}>{operation.status}</Status>,
                      formatCount(operation.attempt),
                      operation.worker_name ?? "—",
                      <Ago key="asked" at={operation.created_at} />,
                      <Ago key="changed" at={operation.updated_at} />,
                      <Message key="error" text={operation.last_error} />,
                    ],
                  }))}
                />
              </Section>
              <div className="grid grid-cols-[minmax(0,1fr)] gap-5 lg:grid-cols-2">
                <Section title="Snapshots" description="Every snapshot still kept, complete or not.">
                  <Table
                    head={["Generation", "Reason", "Size", "Taken"]}
                    empty="No snapshots."
                    rows={detail.snapshots.map((snapshot) => ({
                      key: String(snapshot.generation),
                      cells: [`${snapshot.generation}${snapshot.complete ? "" : " (incomplete)"}`, snapshot.reason, formatBytes(snapshot.size_bytes), <Ago key="taken" at={snapshot.created_at} />],
                    }))}
                  />
                </Section>
                <Section title="Plugin errors">
                  <Table
                    head={["Plugin", "Error", "Reported"]}
                    empty="Every plugin loaded."
                    rows={detail.plugin_errors.map((plugin) => ({
                      key: plugin.name,
                      cells: [`${plugin.name} ${plugin.version}${plugin.enabled ? "" : " (off)"}`, <Message key="error" text={plugin.runtime_error} />, <Ago key="reported" at={plugin.reported_at} />],
                    }))}
                  />
                </Section>
              </div>
            </div>
          );
        }}
      </Loaded>
    </>
  );
}
