"use client";

import { useDeferredValue, useState } from "react";
import { browserApi } from "../../lib/api/operations";
import { formatCount, formatDateTime, formatUsd } from "../../lib/format";
import { queryKeys } from "../../lib/query";
import { controlStyles, PageHeader } from "../ui";
import { serverHead, serverRows } from "./servers";
import { AccountLink, Ago, Loaded, Pager, Section, Stat, StatGrid, Status, Table, useAdminQuery } from "./ui";

const LIMIT = 50;

const providerNames: Record<string, string> = {
  google: "Google",
  discord: "Discord",
  minecraft_java: "Java",
  minecraft_bedrock: "Bedrock",
};

function AccountStatus({ status }: { status: string }) {
  return <Status tone={status === "active" ? "good" : "danger"}>{status}</Status>;
}

export function AdminAccountsPage() {
  const [search, setSearch] = useState("");
  const [offset, setOffset] = useState(0);
  const deferredSearch = useDeferredValue(search.trim());
  const params = { search: deferredSearch || undefined, limit: LIMIT, offset };
  const query = useAdminQuery(queryKeys.admin.accounts(params), () => browserApi.getAdminAccounts(params));
  return (
    <>
      <PageHeader title="Accounts" description="Every account, the newest first. An account is made when a verified player claims a server." />
      <input
        className={`${controlStyles.input} mb-4 max-w-96 min-h-10`}
        type="search"
        aria-label="Search accounts"
        placeholder="Name, email, account id or provider id"
        value={search}
        onChange={(event) => { setSearch(event.target.value); setOffset(0); }}
      />
      <Loaded query={query}>
        {(page) => (
          <Section title={`${formatCount(page.total)} ${page.total === 1 ? "account" : "accounts"}`}>
            <div className={query.isFetching && query.isPlaceholderData ? "opacity-60" : ""}>
              <Table
                head={["Account", "Status", "Sign-ins", "Servers", "AI this month", "Joined", "Last seen"]}
                empty="No account matches."
                rows={page.items.map((account) => ({
                  key: account.id,
                  cells: [
                    <AccountLink key="account" id={account.id} name={account.display_name} />,
                    <AccountStatus key="status" status={account.account_status} />,
                    <span key="identities" className="text-xs">{account.identities.map((identity) => `${providerNames[identity.provider] ?? identity.provider} ${identity.display_name}`).join(", ") || "—"}</span>,
                    formatCount(account.servers),
                    formatUsd(account.ai_cost_this_month_micros),
                    <Ago key="joined" at={account.created_at} />,
                    <Ago key="seen" at={account.last_seen_at} empty="never in the panel" />,
                  ],
                }))}
              />
            </div>
            <Pager offset={offset} limit={LIMIT} total={page.total} onChange={setOffset} />
          </Section>
        )}
      </Loaded>
    </>
  );
}

export function AdminAccountPage({ accountId }: { accountId: string }) {
  const query = useAdminQuery(queryKeys.admin.account(accountId), () => browserApi.getAdminAccount(accountId));
  return (
    <Loaded query={query}>
      {(detail) => {
        const { account } = detail;
        return (
          <>
            <PageHeader title={account.display_name} description={`Account ${account.id}${detail.is_admin ? " · admin" : ""}`} />
            <div className="grid grid-cols-[minmax(0,1fr)] gap-5">
              <StatGrid>
                <Stat label="Status" value={account.account_status} tone={account.account_status === "active" ? "good" : "danger"} />
                <Stat label="Joined" value={<Ago at={account.created_at} />} detail={formatDateTime(account.created_at)} />
                <Stat label="Last seen in the panel" value={<Ago at={account.last_seen_at} />} />
                <Stat label="AI this month" value={formatUsd(account.ai_cost_this_month_micros)} detail={`${formatUsd(detail.ai_cost_total_micros)} in all`} />
              </StatGrid>
              <Section title="Servers">
                <Table head={serverHead(false)} empty="No servers." rows={serverRows(detail.servers, { owner: false })} />
              </Section>
              <Section title="Sign-ins" description="The provider id is what ADMIN_IDENTITIES names, as provider:id.">
                <Table
                  head={["Provider", "Name", "Email", "Provider id", "Linked"]}
                  empty="No linked sign-ins."
                  rows={detail.identities.map((identity) => ({
                    key: `${identity.provider}:${identity.subject}`,
                    cells: [
                      providerNames[identity.provider] ?? identity.provider,
                      identity.display_name,
                      identity.email ? `${identity.email}${identity.email_verified ? "" : " (unverified)"}` : "—",
                      <code key="subject" className="text-xs break-all">{identity.subject}</code>,
                      <Ago key="linked" at={identity.created_at} />,
                    ],
                  }))}
                />
              </Section>
              <Section title="Browser sessions">
                <Table
                  head={["Session", "Started", "Last request"]}
                  empty="Not signed in anywhere."
                  rows={detail.sessions.map((session) => ({
                    key: session.id,
                    cells: [<code key="id" className="text-xs">{session.id.slice(0, 8)}</code>, <Ago key="created" at={session.created_at} />, <Ago key="seen" at={session.last_seen_at} />],
                  }))}
                />
              </Section>
            </div>
          </>
        );
      }}
    </Loaded>
  );
}
