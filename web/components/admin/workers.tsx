"use client";

import type { components } from "../../lib/api/generated";
import { browserApi } from "../../lib/api/operations";
import { formatBytes, formatCores, formatCount } from "../../lib/format";
import { queryKeys } from "../../lib/query";
import { EmptyState, Meter, PageHeader } from "../ui";
import { Ago, Loaded, Section, ServerLink, ServerState, Status, Table, useAdminQuery } from "./ui";

type Worker = components["schemas"]["AdminWorker"];

/** Reserved for placements, and of that what running servers use. */
function Capacity({ label, total, reserved, used, format }: { label: string; total: number; reserved: number; used?: number | null; format: (value: number) => string }) {
  return (
    <div className="grid gap-1.5">
      <div className="flex justify-between gap-3 text-xs">
        <span className="font-[760]">{label}</span>
        <span className="text-ink-soft">
          {used != null ? `${format(used)} used, ` : ""}{format(reserved)} reserved of {format(total)}
        </span>
      </div>
      <Meter
        label={`${label} reserved`}
        max={total}
        segments={[
          { value: Math.min(used ?? 0, reserved), className: "bg-[#2a78d6]" },
          { value: Math.max(reserved - (used ?? 0), 0), className: "bg-[#9ec0ea]" },
        ]}
      />
    </div>
  );
}

function WorkerCard({ worker }: { worker: Worker }) {
  return (
    <Section
      title={worker.name}
      description={`${worker.address} · ${worker.runtime_architecture}${worker.supports_ci_previews ? " · runs previews" : ""} · ports ${worker.port_min}–${worker.port_max}, ${formatCount(worker.ports_leased)} leased`}
      actions={
        <span className="grid justify-items-end gap-1 text-xs text-ink-soft">
          <Status tone={!worker.live ? "danger" : worker.draining ? "warning" : "good"}>{!worker.live ? "Not live" : worker.draining ? "Draining" : "Live"}</Status>
          <span>heartbeat <Ago at={worker.heartbeat_at} /></span>
          <span>lease {worker.lease_expires_at ? <>until <Ago at={worker.lease_expires_at} /></> : "none"}</span>
        </span>
      }
    >
      <div className="grid grid-cols-[minmax(0,1fr)] gap-3 md:grid-cols-3">
        <Capacity label="CPU" total={worker.total_cpu_millis} reserved={worker.reserved_cpu_millis} used={worker.cpu_used_millis} format={formatCores} />
        <Capacity label="Memory" total={worker.total_memory_bytes} reserved={worker.reserved_memory_bytes} used={worker.memory_used_bytes} format={formatBytes} />
        <Capacity label="Disk" total={worker.total_disk_bytes} reserved={worker.reserved_disk_bytes} format={formatBytes} />
      </div>
      <div className="mt-4 grid gap-4 lg:grid-cols-2">
        <Table
          head={["Server", "State", "Placement", "Players"]}
          empty="No servers placed here."
          rows={worker.servers.map((server) => ({
            key: server.id,
            cells: [<ServerLink key="server" id={server.id} subdomain={server.subdomain} />, <ServerState key="state" state={server.observed_state} />, server.active ? "active" : "released", formatCount(server.players_online)],
          }))}
        />
        <Table
          head={["Preview", "State", "Players"]}
          empty="No previews placed here."
          rows={worker.previews.map((preview) => ({
            key: preview.preview_id,
            cells: [preview.hostname ?? preview.preview_id.slice(0, 8), preview.state, formatCount(preview.players_online)],
          }))}
        />
      </div>
    </Section>
  );
}

export function AdminWorkersPage() {
  const query = useAdminQuery(queryKeys.admin.workers, browserApi.getAdminWorkers);
  return (
    <>
      <PageHeader title="Workers" description="The machines that run servers. Each server reserves 2 GiB of memory, 5 GiB of disk and one core while it is placed; dark blue is what running servers use of that." />
      <Loaded query={query}>
        {(workers) => workers.length === 0
          ? <EmptyState title="No workers" description="Provision one with worker-bundle." />
          : <div className="grid grid-cols-[minmax(0,1fr)] gap-5">{workers.map((worker) => <WorkerCard key={worker.id} worker={worker} />)}</div>}
      </Loaded>
    </>
  );
}
