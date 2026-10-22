"use client";

import { browserApi } from "../../lib/api/operations";
import { formatCount, formatUsd, formatUtcDay } from "../../lib/format";
import { queryKeys } from "../../lib/query";
import { ColumnChart } from "../charts";
import { PageHeader } from "../ui";
import { AccountLink, Ago, Loaded, Message, Section, ServerLink, Stat, StatGrid, Status, Table, useAdminQuery } from "./ui";

const compactTokens = new Intl.NumberFormat("en-US", { notation: "compact", maximumFractionDigits: 1 });

export function AdminAiPage() {
  const query = useAdminQuery(queryKeys.admin.ai, browserApi.getAdminAi);
  return (
    <>
      <PageHeader title="AI assistant" description="What the in-game /ai assistant costs, who uses it, and the requests that went wrong. Months are calendar months." />
      <Loaded query={query}>
        {(ai) => (
          <div className="grid grid-cols-[minmax(0,1fr)] gap-5">
            <StatGrid>
              <Stat label="Spent this month" value={formatUsd(ai.month.cost_micros)} detail={`${formatCount(ai.month.jobs)} requests, ${formatCount(ai.month.model_calls)} model calls`} />
              <Stat label="Tokens this month" value={compactTokens.format(ai.month.input_tokens + ai.month.output_tokens)} detail={`${compactTokens.format(ai.month.input_tokens)} in, ${compactTokens.format(ai.month.output_tokens)} out, ${compactTokens.format(ai.month.cache_read_input_tokens)} from cache`} />
              {ai.budgets ? (
                <>
                  <Stat label="Budget per server" value={formatUsd(ai.budgets.server_monthly_micros)} detail="a month, by default" />
                  <Stat label="Budget per account" value={formatUsd(ai.budgets.account_monthly_micros)} detail={`a month · ${ai.budgets.max_concurrent_calls} calls at once`} />
                </>
              ) : <Stat label="Provider" value="Not configured" tone="warning" detail="Set AI_PROVIDER to offer the assistant." />}
            </StatGrid>
            <Section title="The last 30 days">
              <ColumnChart
                title="Spend per day (UTC)"
                columns={ai.by_day.map((day) => ({ label: formatUtcDay(day.day), value: day.cost_micros }))}
                format={formatUsd}
                axisFormat={formatUsd}
                total={formatUsd(ai.by_day.reduce((sum, day) => sum + day.cost_micros, 0))}
              />
            </Section>
            <div className="grid grid-cols-[minmax(0,1fr)] gap-5 lg:grid-cols-2">
              <Section title="Top servers this month">
                <Table
                  head={["Server", "Spent", "Calls"]}
                  empty="No spend this month."
                  rows={ai.top_servers.map((server) => ({ key: server.id ?? "deleted", cells: [<ServerLink key="server" id={server.id ?? ""} subdomain={server.name} page="ai" />, formatUsd(server.cost_micros), formatCount(server.model_calls)] }))}
                />
              </Section>
              <Section title="Top accounts this month">
                <Table
                  head={["Account", "Spent", "Calls"]}
                  empty="No spend this month."
                  rows={ai.top_accounts.map((account) => ({ key: account.id ?? "deleted", cells: [<AccountLink key="account" id={account.id ?? ""} name={account.name} />, formatUsd(account.cost_micros), formatCount(account.model_calls)] }))}
                />
              </Section>
            </div>
            <Section title="Models this month" description={ai.budgets ? `Offered: ${ai.budgets.models.join(", ")}.` : undefined}>
              <Table
                head={["Model", "Spent", "Calls", "Input", "Output", "Cache read", "Cache write"]}
                empty="No model was called this month."
                rows={ai.models.map((model) => ({
                  key: model.model,
                  cells: [<code key="model" className="text-xs">{model.model}</code>, formatUsd(model.cost_micros), formatCount(model.model_calls), compactTokens.format(model.input_tokens), compactTokens.format(model.output_tokens), compactTokens.format(model.cache_read_input_tokens), compactTokens.format(model.cache_creation_input_tokens)],
                }))}
              />
            </Section>
            <Section title="Requests of the last 30 days">
              <div className="flex flex-wrap gap-2">
                {ai.jobs_by_state.length ? ai.jobs_by_state.map((entry) => (
                  <span key={entry.state} className="rounded-sm bg-[#eee7d8] px-3 py-2 text-sm"><strong>{formatCount(entry.count)}</strong> {entry.state.replaceAll("_", " ")}</span>
                )) : <p className="m-0 text-sm text-ink-soft">No requests.</p>}
              </div>
            </Section>
            <Section title="Failed or stalled" description="The latest 50. Open the server's AI page for the whole conversation.">
              <Table
                head={["When", "Server", "Operator", "Prompt", "State", "Error", "Stalls", "Spent"]}
                empty="Every request went through."
                rows={ai.problem_jobs.map((job) => ({
                  key: job.id,
                  cells: [
                    <Ago key="at" at={job.created_at} />,
                    <ServerLink key="server" id={job.server_id} subdomain={job.subdomain} page="ai" />,
                    job.operator_name,
                    <Message key="prompt" text={job.prompt} />,
                    <Status key="state" tone={job.state === "failed" ? "danger" : "warning"}>{job.state.replaceAll("_", " ")}</Status>,
                    <Message key="error" text={job.error} />,
                    formatCount(job.stalls),
                    formatUsd(job.cost_micros),
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
