"use client";

import { isAiBudgetSpent, nextAiPeriodStart } from "../../lib/ai";
import type { components } from "../../lib/api/generated";
import { formatCount, formatUsd, formatUtcDay } from "../../lib/format";
import { Meter, Surface } from "../ui";

type AiBudget = components["schemas"]["AiBudget"];
type AiUsageData = components["schemas"]["AiUsage"];

function Budget({
  label,
  note,
  budget,
  capped,
}: {
  label: string;
  note?: string;
  budget: AiBudget;
  capped: boolean;
}) {
  const spent = capped && isAiBudgetSpent(budget);

  return (
    <div className="grid gap-2">
      <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
        <h3 className="m-0 text-sm font-[760]">{label}</h3>
        <span className="text-xs text-ink-soft">
          <strong className="text-sm text-ink">{formatUsd(budget.spent_micros)}</strong>
          {capped ? ` of ${formatUsd(budget.budget_micros)}` : " spent"}
        </span>
      </div>
      {capped ? (
        <Meter
          label={`${label} used`}
          max={budget.budget_micros}
          segments={[
            { value: budget.spent_micros, className: spent ? "bg-danger" : "bg-grass" },
            { value: budget.reserved_micros, className: "bg-grass-bright/50" },
          ]}
        />
      ) : null}
      {spent || budget.reserved_micros > 0 || note ? (
        <p className="m-0 text-xs leading-[1.45] text-ink-soft">
          {spent ? <strong className="text-danger">Used up: new requests are refused until it resets. </strong> : null}
          {budget.reserved_micros > 0 ? `Up to ${formatUsd(budget.reserved_micros)} is held for requests in progress. ` : null}
          {note}
        </p>
      ) : null}
    </div>
  );
}

/** This month's spending against the server and account budgets; `capped` is false while no model provider is set up. */
export function AiUsage({ usage, capped }: { usage: AiUsageData; capped: boolean }) {
  const totals = [
    ["Model calls", usage.model_calls],
    ["Input tokens", usage.input_tokens],
    ["Output tokens", usage.output_tokens],
    ["Cache writes", usage.cache_creation_input_tokens],
    ["Cache reads", usage.cache_read_input_tokens],
  ] as const;

  return (
    <Surface className="p-6">
      <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1">
        <h2 className="m-0 font-display text-xl leading-[1.15] font-[800]">This month</h2>
        <span className="text-xs text-ink-soft">
          Since {formatUtcDay(usage.period_start)} · resets {formatUtcDay(nextAiPeriodStart(usage.period_start))} (UTC)
        </span>
      </div>
      <div className="mt-5 grid gap-5">
        <Budget label="Server budget" budget={usage.server} capped={capped} />
        <Budget label="Account budget" note="Shared by all of the owner's servers." budget={usage.account} capped={capped} />
      </div>
      <h3 className="mt-6 mb-0 text-sm font-[760]">On this server</h3>
      <dl className="mt-2 mb-0 grid grid-cols-2 gap-x-6 border-t border-[#ddd4c3] max-[560px]:grid-cols-1">
        {totals.map(([label, value]) => (
          <div className="flex items-baseline justify-between gap-3 border-b border-[#ddd4c3] py-2.5 text-[13px]" key={label}>
            <dt className="text-ink-soft">{label}</dt>
            <dd className="m-0 font-[760] tabular-nums">{formatCount(value)}</dd>
          </div>
        ))}
      </dl>
    </Surface>
  );
}
