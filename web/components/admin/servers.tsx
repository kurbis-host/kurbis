"use client";

import { useDeferredValue, useState } from "react";
import type { components } from "../../lib/api/generated";
import { browserApi } from "../../lib/api/operations";
import { formatBytes, formatCount } from "../../lib/format";
import { queryKeys } from "../../lib/query";
import { controlStyles, PageHeader } from "../ui";
import { serverFilters, type ServerFilter as Filter } from "./server-filters";
import { AccountLink, Ago, Loaded, Message, Pager, Section, Segmented, ServerLink, ServerState, Table, useAdminQuery } from "./ui";

type AdminServer = components["schemas"]["AdminServer"];

const LIMIT = 50;

/** The columns every server list shows. */
export function serverRows(servers: AdminServer[], { owner = true } = {}) {
  return servers.map((server) => ({
    key: server.id,
    cells: [
      <ServerLink key="server" id={server.id} subdomain={server.subdomain} />,
      ...(owner ? [<AccountLink key="owner" id={server.owner_id} name={`${server.owner_name}${server.owner_status === "suspended" ? " (suspended)" : ""}`} />] : []),
      <ServerState key="state" state={server.observed_state} />,
      server.worker_name ?? <span key="worker" className="text-ink-soft">—</span>,
      formatCount(server.players_online),
      formatBytes(server.storage_bytes),
      <Ago key="woken" at={server.last_woken_at} />,
      <Message key="error" text={server.last_error} />,
    ],
  }));
}

export const serverHead = (owner = true) => ["Server", ...(owner ? ["Owner"] : []), "State", "Worker", "Players", "World", "Last woken", "Last error"];

export function AdminServersPage({ initialFilter }: { initialFilter: Filter }) {
  const [filter, setFilter] = useState<Filter>(initialFilter);
  const [search, setSearch] = useState("");
  const [offset, setOffset] = useState(0);
  const deferredSearch = useDeferredValue(search.trim());
  const params = { filter, search: deferredSearch || undefined, limit: LIMIT, offset };
  const query = useAdminQuery(queryKeys.admin.servers(params), () => browserApi.getAdminServers(params));
  return (
    <>
      <PageHeader title="Servers" description="Every server, the latest change first. Open one to read its lifecycle, console and players." />
      <div className="mb-4 flex flex-wrap items-center gap-3">
        <Segmented label="Server state" options={serverFilters} value={filter} onChange={(value) => { setFilter(value); setOffset(0); }} />
        <input
          className={`${controlStyles.input} max-w-80 min-h-10`}
          type="search"
          aria-label="Search servers"
          placeholder="Address, owner or id"
          value={search}
          onChange={(event) => { setSearch(event.target.value); setOffset(0); }}
        />
      </div>
      <Loaded query={query}>
        {(page) => (
          <Section title={`${formatCount(page.total)} ${page.total === 1 ? "server" : "servers"}`}>
            <div className={query.isFetching && query.isPlaceholderData ? "opacity-60" : ""}>
              <Table head={serverHead()} empty="No server matches." rows={serverRows(page.items)} />
            </div>
            <Pager offset={offset} limit={LIMIT} total={page.total} onChange={setOffset} />
          </Section>
        )}
      </Loaded>
    </>
  );
}
