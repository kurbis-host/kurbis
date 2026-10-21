"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronDown, Sparkles, X } from "lucide-react";
import { useId, useState } from "react";
import { aiJobStates, aiModelName, aiToolCallStatus, formatToolInput, isActiveAiJob, withAiJob } from "../../lib/ai";
import { errorMessage } from "../../lib/api/client";
import type { components } from "../../lib/api/generated";
import { browserApi } from "../../lib/api/operations";
import { formatDateTime, formatQuantity, formatRelative, formatUsd } from "../../lib/format";
import { queryKeys } from "../../lib/query";
import { Button, EmptyState, Notice, StatusLabel, Surface } from "../ui";

type AiJob = components["schemas"]["AiJob"];
type AiModel = components["schemas"]["AiModel"];
type AiOverview = components["schemas"]["AiOverview"];

const caption = "mt-2.5 block text-[11px] font-[760] text-ink-soft";
const preformatted =
  "m-0 mt-1 max-h-60 overflow-auto rounded-sm bg-white/60 p-2.5 font-mono text-[11px] leading-[1.5] whitespace-pre-wrap [overflow-wrap:anywhere]";

function ToolCalls({ serverId, jobId }: { serverId: string; jobId: string }) {
  const { data, isPending, isError, error, refetch } = useQuery({
    queryKey: queryKeys.aiJob(serverId, jobId),
    queryFn: () => browserApi.getAiJob(serverId, jobId),
    // A finished request never changes again; live "ai" events refresh one in progress.
    staleTime: ({ state }) => (state.data && !isActiveAiJob(state.data.job.state) ? Infinity : 0),
  });

  if (isPending) return <p className="m-0 text-xs text-ink-soft" role="status">Loading tool calls…</p>;
  if (isError) {
    return (
      <Notice tone="danger">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <span>The tool calls couldn&apos;t be loaded: {errorMessage(error)}</span>
          <Button size="small" variant="secondary" readSafe onClick={() => void refetch()}>Retry</Button>
        </div>
      </Notice>
    );
  }
  return (
    <ol className="m-0 grid list-none gap-2.5 p-0">
      {data.tool_calls.map((call) => {
        const status = aiToolCallStatus(call);
        const input = formatToolInput(call.input);
        return (
          <li className="rounded-[9px] bg-[#eee7d8] p-3" key={call.call_id}>
            <div className="flex flex-wrap items-center justify-between gap-2 text-xs">
              <code className="text-[13px] font-[760] [overflow-wrap:anywhere]">{call.name}</code>
              <StatusLabel tone={status.tone}>{status.label}</StatusLabel>
            </div>
            {input ? (
              <>
                <span className={caption}>Input</span>
                <pre className={preformatted}>{input}</pre>
              </>
            ) : null}
            {call.output ? (
              <>
                <span className={caption}>{call.is_error ? "Error" : "Result"}</span>
                <pre className={preformatted}>{call.output}</pre>
              </>
            ) : null}
          </li>
        );
      })}
    </ol>
  );
}

function JobRow({ serverId, job, models, now }: { serverId: string; job: AiJob; models: AiModel[]; now: number }) {
  const queryClient = useQueryClient();
  const [expanded, setExpanded] = useState(false);
  const detailsId = useId();
  const state = aiJobStates[job.state];
  const cancel = useMutation({
    mutationFn: () => browserApi.cancelAiJob(serverId, job.id),
    onSuccess: (cancelled) => {
      queryClient.setQueryData<AiOverview>(queryKeys.ai(serverId), (overview) => overview && withAiJob(overview, cancelled));
      void queryClient.invalidateQueries({ queryKey: queryKeys.aiJob(serverId, job.id) });
    },
  });

  return (
    <li className="border-t border-[#ddd4c3] py-4">
      <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1 text-xs text-ink-soft">
        <span className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <StatusLabel tone={state.tone}>{state.label}</StatusLabel>
          <strong className="[overflow-wrap:anywhere] text-ink">{job.operator_name}</strong>
        </span>
        <time dateTime={job.created_at} title={formatDateTime(job.created_at)}>
          {formatRelative(job.created_at, now)}
        </time>
      </div>
      <p className="mt-2 mb-0 text-sm leading-[1.45] font-[700] whitespace-pre-wrap [overflow-wrap:anywhere]">{job.prompt}</p>
      {job.answer ? (
        <p className="mt-1.5 mb-0 text-[13px] leading-normal whitespace-pre-wrap text-[#4d4b42] [overflow-wrap:anywhere]">{job.answer}</p>
      ) : null}
      {job.error ? (
        <p className="mt-1.5 mb-0 text-[13px] leading-normal text-danger [overflow-wrap:anywhere]">{job.error}</p>
      ) : null}
      <div className="mt-2.5 flex flex-wrap items-center justify-between gap-2">
        <span className="text-[11px] text-ink-soft">
          {formatUsd(job.cost_micros)} · {formatQuantity(job.model_calls, "model call")} · {formatQuantity(job.tool_calls, "tool call")} · {aiModelName(models, job.model)}
        </span>
        <span className="flex flex-wrap gap-2">
          {isActiveAiJob(job.state) ? (
            <Button size="small" variant="secondary" disabled={cancel.isPending} onClick={() => cancel.mutate()}>
              <X size={14} />
              {cancel.isPending ? "Cancelling…" : "Cancel"}
            </Button>
          ) : null}
          {job.tool_calls > 0 ? (
            <Button size="small" variant="quiet" readSafe aria-expanded={expanded} aria-controls={detailsId} onClick={() => setExpanded(!expanded)}>
              <ChevronDown size={14} className={`transition-transform ${expanded ? "rotate-180" : ""}`} />
              {expanded ? "Hide tool calls" : "Show tool calls"}
            </Button>
          ) : null}
        </span>
      </div>
      {cancel.isError ? <p className="mt-2 mb-0 text-xs text-danger" role="alert">{errorMessage(cancel.error)}</p> : null}
      <div className="mt-3" id={detailsId} hidden={!expanded}>
        {expanded ? <ToolCalls serverId={serverId} jobId={job.id} /> : null}
      </div>
    </li>
  );
}

export function AiJobs({ serverId, jobs, models, now }: { serverId: string; jobs: AiJob[]; models: AiModel[]; now: number }) {
  return (
    <Surface className="p-6">
      <h2 className="m-0 font-display text-xl leading-[1.15] font-[800]">Recent requests</h2>
      <p className="mt-1.5 mb-0 text-xs leading-[1.45] text-ink-soft">
        The latest 20 on this server, with every tool call the assistant made.
      </p>
      {jobs.length ? (
        <ul className="mt-3 mb-0 list-none p-0">
          {jobs.map((job) => <JobRow key={job.id} serverId={serverId} job={job} models={models} now={now} />)}
        </ul>
      ) : (
        <EmptyState
          icon={<Sparkles size={22} />}
          title="No requests yet"
          description="When an operator asks something with /ai in game, the request, the answer and every action the assistant took appear here."
        />
      )}
    </Surface>
  );
}
