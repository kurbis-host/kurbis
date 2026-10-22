"use client";

import { useQuery } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import type { components } from "../../lib/api/generated";
import { browserApi } from "../../lib/api/operations";
import { formatBytes, formatCount } from "../../lib/format";
import { queryKeys } from "../../lib/query";
import { Button, PageHeader } from "../ui";
import { Ago, Loaded, Message, Section, Stat, StatGrid, Status, Table, useAdminQuery } from "./ui";

type StateCount = components["schemas"]["AdminStateCount"];

function Counts({ counts, empty }: { counts: StateCount[]; empty: string }) {
  if (counts.length === 0) return <p className="m-0 text-sm text-ink-soft">{empty}</p>;
  return (
    <div className="flex flex-wrap gap-2">
      {counts.map((entry) => (
        <span key={entry.state} className="rounded-sm bg-[#eee7d8] px-3 py-2 text-sm"><strong>{formatCount(entry.count)}</strong> {entry.state}</span>
      ))}
    </div>
  );
}

function stageTone(stage: string) {
  if (stage === "ready") return "good" as const;
  if (stage === "failed") return "danger" as const;
  if (stage === "expired") return "quiet" as const;
  return "progress" as const;
}

function BuildLog({ buildId, onClose }: { buildId: string; onClose: () => void }) {
  const query = useQuery({ queryKey: queryKeys.admin.buildLog(buildId), queryFn: () => browserApi.getAdminBuildLog(buildId) });
  const ref = useRef<HTMLDivElement>(null);
  // The log opens above the builds table; bring it into view from the row that opened it.
  useEffect(() => { ref.current?.scrollIntoView({ block: "start", behavior: "smooth" }); }, [buildId]);
  return (
    <div ref={ref} className="scroll-mt-4">
    <Section title="Build log" actions={<Button size="small" variant="secondary" onClick={onClose}>Close</Button>}>
      <Loaded query={query}>
        {(log) => (
          <>
            <p className="mt-0 text-xs text-ink-soft">{log.commit_sha.slice(0, 12)} · {log.stage}{log.failure_message ? ` · ${log.failure_message}` : ""}</p>
            <pre className="m-0 max-h-[60vh] overflow-auto rounded-sm bg-[#2b2620] p-3 text-[11px] whitespace-pre-wrap text-[#f2ead8]">{log.build_log || "The build left no log."}</pre>
          </>
        )}
      </Loaded>
    </Section>
    </div>
  );
}

export function AdminCiPage() {
  const query = useAdminQuery(queryKeys.admin.ci, browserApi.getAdminCi);
  const [logOf, setLogOf] = useState<string | null>(null);
  return (
    <>
      <PageHeader title="Pumpkin previews" description="Builds of Pumpkin's pull requests and branches, each playable at its own address." />
      <Loaded query={query}>
        {(ci) => (
          <div className="grid grid-cols-[minmax(0,1fr)] gap-5">
            <StatGrid>
              {ci.repositories.map((repository) => (
                <Stat
                  key={repository.repository}
                  label={repository.repository}
                  value={repository.enabled ? "Watched" : "Not watched"}
                  tone={repository.enabled ? "good" : "warning"}
                  detail={<>polled <Ago at={repository.last_successful_poll} />, default branch {repository.default_branch}</>}
                />
              ))}
              {ci.github_rate_limits.map((limit) => {
                const paused = limit.paused_until && new Date(limit.paused_until).getTime() > Date.now();
                return (
                  <Stat
                    key={limit.scope}
                    label={`GitHub ${limit.scope}`}
                    value={limit.remaining == null ? "—" : `${formatCount(limit.remaining)} left`}
                    tone={paused ? "warning" : "quiet"}
                    detail={paused ? <>paused until <Ago at={limit.paused_until} /></> : limit.reset_at ? <>resets <Ago at={limit.reset_at} /></> : undefined}
                  />
                );
              })}
              <Stat
                label="Webhook deliveries"
                value={`${formatCount(ci.webhook_deliveries.pending)} pending`}
                tone={ci.webhook_deliveries.failing ? "warning" : "quiet"}
                detail={`${formatCount(ci.webhook_deliveries.failing)} failing`}
              />
            </StatGrid>
            <div className="grid grid-cols-[minmax(0,1fr)] gap-5 lg:grid-cols-2">
              <Section title="Build queue" description="Pull-request build requests: the live ones, and those finished this week.">
                <Counts counts={ci.queue} empty="Nothing queued this week." />
              </Section>
              <Section title="Builds by stage" description="Unfinished builds, and those finished this week.">
                <Counts counts={ci.builds_by_stage} empty="No builds this week." />
              </Section>
            </div>
            <Section title="Builder slots" description="Builds holding a slot of the builder's memory right now.">
              <Table
                head={["Builder", "Slot", "Memory", "Heartbeat", "Since"]}
                empty="No build is running."
                rows={ci.builder_slots.map((slot) => ({
                  key: `${slot.builder_identity}:${slot.slot}`,
                  cells: [<code key="builder" className="text-xs">{slot.builder_identity}</code>, slot.slot, formatBytes(slot.requested_memory_bytes), <Ago key="heartbeat" at={slot.heartbeat_at} />, <Ago key="since" at={slot.created_at} />],
                }))}
              />
            </Section>
            {logOf ? <BuildLog buildId={logOf} onClose={() => setLogOf(null)} /> : null}
            <Section title="Latest builds">
              <Table
                head={["Commit", "For", "Stage", "Failure", "Retries", "Kept", "Changed", ""]}
                empty="No builds yet."
                rows={ci.builds.map((build) => ({
                  key: build.id,
                  cells: [
                    <code key="sha" className="text-xs">{build.commit_sha.slice(0, 10)}</code>,
                    <span key="for" className="text-xs">{[...build.pull_requests.map((number) => `#${number}`), ...build.aliases.map((alias) => alias.ref_name ?? alias.hostname)].join(", ") || "—"}</span>,
                    <Status key="stage" tone={stageTone(build.stage)}>{build.stage}</Status>,
                    build.failure_message ? <Message key="failure" text={`${build.failure_stage ?? ""} ${build.failure_class ?? ""}: ${build.failure_message}`.trim()} /> : "—",
                    formatCount(build.retry_count),
                    build.retention_policy,
                    <Ago key="changed" at={build.updated_at} />,
                    build.has_log ? <Button key="log" size="small" variant="quiet" onClick={() => setLogOf(build.id)}>Log</Button> : "",
                  ],
                }))}
              />
            </Section>
            <Section title="Previews" description="Addresses not yet expired, the latest change first.">
              <Table
                head={["Address", "Ref", "State", "Runtime", "Worker", "Players", "Last played", "Expires"]}
                empty="No previews."
                rows={ci.previews.map((preview) => ({
                  key: preview.id,
                  cells: [
                    <code key="host" className="text-xs">{preview.hostname}</code>,
                    <span key="ref" className="text-xs">{[preview.kind, preview.ref_name ?? preview.commit_sha.slice(0, 10)].join(" ")}</span>,
                    <Status key="state" tone={preview.state === "running" || preview.state === "ready" ? "good" : preview.state === "failed" ? "danger" : "quiet"}>{preview.state}</Status>,
                    preview.runtime_state ?? "—",
                    preview.worker_name ?? "—",
                    formatCount(preview.players_online),
                    <Ago key="activity" at={preview.last_activity_at} />,
                    <Ago key="expires" at={preview.expires_at} />,
                  ],
                }))}
              />
            </Section>
          </div>
        )}
      </Loaded>
    </>
  );
}
