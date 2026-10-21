"use client";

import { useState } from "react";
import { browserApi, type AdminRange } from "../../lib/api/operations";
import { formatCount } from "../../lib/format";
import { queryKeys } from "../../lib/query";
import { PageHeader } from "../ui";
import { Ago, Loaded, Message, RangePicker, Section, ServerLink, Table, useAdminQuery } from "./ui";

/** What each source of failures is, in a word or two. */
export const failureSources: Record<string, string> = {
  operation: "Lifecycle",
  operation_retry: "Retrying",
  server: "Server",
  export: "World export",
  plugin: "Plugin",
  console: "Console",
  ai: "AI",
  ci_build: "Preview build",
  ci_queue: "Build queue",
  github_webhook: "GitHub webhook",
  snapshot_gc: "Snapshot cleanup",
};

/** The server page that shows a failure's source best. */
function pageFor(source: string) {
  if (source === "plugin") return "plugins" as const;
  if (source === "ai") return "ai" as const;
  if (source === "console") return "console" as const;
  return "lifecycle" as const;
}

function Source({ source, kind }: { source: string; kind: string }) {
  return (
    <span className="grid gap-0.5">
      <strong className="text-xs">{failureSources[source] ?? source}</strong>
      <span className="text-[11px] text-ink-soft">{kind}</span>
    </span>
  );
}

export function AdminFailuresPage() {
  const [range, setRange] = useState<AdminRange>("day");
  const query = useAdminQuery(queryKeys.admin.failures(range), () => browserApi.getAdminFailures(range));
  return (
    <>
      <PageHeader
        title="Failures"
        description="Everything that failed, from every place a failure is recorded. Failures that say the same thing, ids and numbers aside, are one group. Failed servers, plugin errors and stuck webhooks show for as long as they last."
        actions={<RangePicker value={range} onChange={setRange} options={["hour", "day", "week", "month"]} />}
      />
      <Loaded query={query}>
        {(failures) => (
          <div className={`grid grid-cols-[minmax(0,1fr)] gap-5 ${query.isFetching && query.isPlaceholderData ? "opacity-60" : ""}`}>
            <Section title="Grouped" description="The groups seen last first.">
              <Table
                head={["Where", "Message", "Times", "Servers", "First", "Last", "Latest server"]}
                empty="Nothing failed."
                rows={failures.groups.map((group) => ({
                  key: `${group.source}:${group.kind}:${group.signature}`,
                  cells: [
                    <Source key="source" source={group.source} kind={group.kind} />,
                    <Message key="message" text={group.message} />,
                    formatCount(group.count),
                    formatCount(group.servers),
                    <Ago key="first" at={group.first_at} />,
                    <Ago key="last" at={group.last_at} />,
                    group.server_id ? <ServerLink key="server" id={group.server_id} subdomain={group.subdomain} page={pageFor(group.source)} /> : "—",
                  ],
                }))}
              />
            </Section>
            <Section title="Latest">
              <Table
                head={["When", "Where", "Message", "Server", "Reference"]}
                empty="Nothing failed."
                rows={failures.recent.map((failure, index) => ({
                  key: `${failure.source}:${failure.reference}:${index}`,
                  cells: [
                    <Ago key="at" at={failure.at} />,
                    <Source key="source" source={failure.source} kind={failure.kind} />,
                    <Message key="message" text={failure.message} />,
                    failure.server_id ? <ServerLink key="server" id={failure.server_id} subdomain={failure.subdomain} page={pageFor(failure.source)} /> : "—",
                    <code key="reference" className="text-[11px] break-all">{failure.reference}</code>,
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
