"use client";

import { useQuery } from "@tanstack/react-query";
import type { components } from "../lib/api/generated";
import { browserApi } from "../lib/api/operations";
import { queryKeys } from "../lib/query";
import { useNow } from "../lib/use-now";
import { AiJobs } from "./ai/jobs";
import { AiSettingsForm } from "./ai/settings-form";
import { AiUsage } from "./ai/usage";
import { Notice, PageHeader, Surface } from "./ui";

type AiOverview = components["schemas"]["AiOverview"];

const commands = [
  ["/ai ask <question>", "Ask the assistant. A follow-up within an hour continues the same conversation."],
  ["/ai new <question>", "Start a fresh conversation."],
  ["/ai status", "See the model, this month's spending and your current request."],
  ["/ai cancel", "Stop your current request."],
] as const;

const columns =
  "grid grid-cols-[minmax(0,1.25fr)_minmax(280px,0.75fr)] items-start gap-[18px] max-[900px]:grid-cols-1";

function InGameCommands() {
  return (
    <Surface className="p-6">
      <h2 className="m-0 font-display text-xl leading-[1.15] font-[800]">Use it in game</h2>
      <p className="mt-1.5 mb-0 text-[13px] leading-normal text-ink-soft">
        Operators type these in chat. The assistant acts with the permissions of whoever asked.
      </p>
      <dl className="mt-4 mb-0 grid gap-3">
        {commands.map(([command, description]) => (
          <div key={command}>
            <dt>
              <code className="rounded bg-[#eee7d8] px-1.5 py-1 text-[13px] font-[700]">{command}</code>
            </dt>
            <dd className="m-0 mt-1.5 text-[13px] leading-[1.45] text-ink-soft">{description}</dd>
          </div>
        ))}
      </dl>
      <p className="mt-4 mb-0 border-t border-[#ddd4c3] pt-4 text-[13px] leading-normal text-ink-soft">
        It reads the server status, players, worlds and entities, and runs commands such as effects, time, weather,
        gamemode, give, tp and summon. It never changes operators, bans, the whitelist or plugins, and never stops the server.
      </p>
    </Surface>
  );
}

export function AiAssistant({
  serverId,
  initialOverview,
  renderedAt,
}: {
  serverId: string;
  initialOverview: AiOverview;
  renderedAt: number;
}) {
  // Live "ai" server events keep this cache current; nothing polls.
  const { data: overview = initialOverview } = useQuery({
    queryKey: queryKeys.ai(serverId),
    queryFn: () => browserApi.getAiOverview(serverId),
    initialData: initialOverview,
  });
  const now = useNow(renderedAt);
  // Without a model provider there are no defaults, models or budgets to show.
  const defaults = overview.available ? overview.defaults : null;
  const jobs = <AiJobs serverId={serverId} jobs={overview.jobs} models={overview.models} now={now} />;

  return (
    <>
      <PageHeader
        title="AI assistant"
        description="Operators ask it for help in game with /ai. Choose its model and monthly budget, and review every request and what the assistant did."
      />
      {!overview.available ? (
        <div className="mb-[18px]">
          <Notice>
            The AI assistant isn&apos;t set up on this Kurbis installation, so /ai requests can&apos;t run. This
            server&apos;s usage and past requests are still listed below.
          </Notice>
        </div>
      ) : !overview.settings.enabled ? (
        <div className="mb-[18px]">
          <Notice tone="warning">
            The assistant is turned off for this server. Operators&apos; /ai requests are refused until you enable it.
          </Notice>
        </div>
      ) : null}

      {defaults ? (
        <>
          <div className={columns}>
            <AiUsage usage={overview.usage} capped />
            <AiSettingsForm serverId={serverId} settings={overview.settings} models={overview.models} defaults={defaults} />
          </div>
          <div className={`mt-[18px] ${columns}`}>
            {jobs}
            <InGameCommands />
          </div>
        </>
      ) : (
        <div className="grid gap-[18px]">
          <AiUsage usage={overview.usage} capped={false} />
          {jobs}
        </div>
      )}
    </>
  );
}
