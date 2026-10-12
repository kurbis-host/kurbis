"use client";

import * as Tooltip from "@radix-ui/react-tooltip";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Copy, ExternalLink, Play, RotateCcw, Square } from "lucide-react";
import Link from "next/link";
import { useState } from "react";
import { errorMessage } from "../lib/api/client";
import type { components } from "../lib/api/generated";
import { browserApi } from "../lib/api/operations";
import { formatBytes, formatDateTime, formatRelative } from "../lib/format";
import { lifecycle, lifecycleActions } from "../lib/lifecycle";
import { queryKeys } from "../lib/query";
import { useNow } from "../lib/use-now";
import { Button, EmptyState, Meter, Notice, PageHeader, Surface } from "./ui";

type ActivityItem = components["schemas"]["ActivityEntry"];
type Server = components["schemas"]["Server"];

function AddressRow({ label, value }: { label: string; value: string }) {
  const [copied, setCopied] = useState(false);

  return (
    <div className="grid grid-cols-[70px_minmax(0,1fr)_auto] items-center gap-3 rounded-[9px] bg-[#eee7d8] px-3.5 py-[13px] max-[560px]:grid-cols-[56px_minmax(0,1fr)_auto]">
      <span className="text-[11px] font-[820] text-[#68665d]">{label}</span>
      <code className="min-w-0 overflow-hidden text-xs text-ellipsis" title={value}>
        {value}
      </code>
      <Tooltip.Root>
        <Tooltip.Trigger asChild>
          <button
            className="grid size-9 cursor-pointer place-items-center rounded-[7px] border-0 bg-paper text-[#4e4b43]"
            aria-label={`Copy ${label} address`}
            onClick={async () => {
              await navigator.clipboard.writeText(value);
              setCopied(true);
              window.setTimeout(() => setCopied(false), 1200);
            }}
          >
            <Copy size={15} />
          </button>
        </Tooltip.Trigger>
        <Tooltip.Portal>
          <Tooltip.Content sideOffset={6}>
            {copied ? "Copied" : "Copy address"}
          </Tooltip.Content>
        </Tooltip.Portal>
      </Tooltip.Root>
    </div>
  );
}

function stateTone(tone: string) {
  if (tone === "good") {
    return "bg-success shadow-[0_0_0_5px_rgba(63,113,62,0.13)]";
  }
  if (tone === "danger") {
    return "bg-danger shadow-[0_0_0_5px_rgba(168,61,50,0.13)]";
  }
  if (tone === "progress" || tone === "warning") {
    return "animate-[pulse-status_1.4s_infinite] bg-warning shadow-[0_0_0_5px_rgba(166,109,34,0.13)]";
  }
  return "bg-[#8a8a82] shadow-[0_0_0_5px_rgba(110,110,105,0.12)]";
}

export function Overview({
  initialServer,
  initialActivity,
  renderedAt,
  panelBasePath,
}: {
  initialServer: Server;
  initialActivity: ActivityItem[];
  renderedAt: number;
  panelBasePath?: string;
}) {
  const queryClient = useQueryClient();
  const serverId = initialServer.id;
  const { data: server = initialServer } = useQuery({
    queryKey: queryKeys.server(serverId),
    queryFn: () => browserApi.getServer(serverId),
    initialData: initialServer,
  });
  const { data: activity = initialActivity } = useQuery({
    queryKey: queryKeys.activity(serverId),
    queryFn: () => browserApi.getActivity(serverId),
    initialData: initialActivity,
  });
  const [requestError, setRequestError] = useState<string | null>(null);
  const relativeNow = useNow(renderedAt);
  const action = useMutation({
    mutationFn: async (name: "start" | "stop" | "restart") =>
      browserApi.command(serverId, { command: name }),
    onMutate: async (name) => {
      setRequestError(null);
      const serverKey = queryKeys.server(serverId);
      await queryClient.cancelQueries({ queryKey: serverKey });
      const previous = queryClient.getQueryData<Server | null>(serverKey);
      queryClient.setQueryData<Server | null>(serverKey, (current) => {
        if (!current) return current;
        return name === "start"
          ? { ...current, desired_state: "running", observed_state: "queued" }
          : {
              ...current,
              desired_state: name === "stop" ? "stopped" : "running",
              observed_state: "stopping",
            };
      });
      return { previous };
    },
    onError: (error, _name, context) => {
      if (context?.previous !== undefined) {
        queryClient.setQueryData(queryKeys.server(serverId), context.previous);
      }
      setRequestError(errorMessage(error));
    },
    onSettled: () =>
      void queryClient.invalidateQueries({ queryKey: queryKeys.server(serverId) }),
  });

  if (!server) {
    return (
      <EmptyState
        title="No server yet"
        description="Create your first world to open the overview."
      />
    );
  }

  const view = lifecycle[server.observed_state];
  const allowed = lifecycleActions(server);

  return (
    <>
      <PageHeader
        title={server.subdomain}
        description="Live world state, connection details, recovery status, and recent control-plane activity."
      />
      {requestError ? (
        <div className="mb-[18px]">
          <Notice tone="danger">{requestError}</Notice>
        </div>
      ) : null}

      <div className="grid grid-cols-[minmax(0,1.45fr)_minmax(300px,0.8fr)] gap-[18px] max-[970px]:grid-cols-1">
        <Surface className="relative min-h-[300px] overflow-hidden p-[clamp(22px,4vw,34px)] after:absolute after:right-[-50px] after:bottom-[-74px] after:size-[180px] after:rotate-[22deg] after:rounded-[28px] after:bg-[#e3d8c4] after:opacity-75 after:content-['']">
          <div className="relative z-1 flex items-start justify-between gap-4 max-[560px]:flex-col">
            <div className="flex items-center gap-[13px]">
              <span
                className={`size-4 rounded-[5px] ${stateTone(view.tone)}`}
                aria-hidden="true"
              />
              <div>
                <h2 className="m-0 font-display text-[clamp(34px,5vw,56px)] leading-[0.95] font-[850] tracking-[-0.035em]">
                  {view.label}
                </h2>
                <div className="mt-[7px] text-[13px] font-bold text-ink-soft">
                  {server.current_operation?.stage ?? view.short}
                </div>
              </div>
            </div>
            <span>
              {server.player_count}{" "}
              {server.player_count === 1 ? "player" : "players"}
            </span>
          </div>
          <p className="relative z-1 mt-[30px] mb-0 max-w-[60ch] leading-[1.6] text-[#50544b]">
            {server.current_operation?.message ?? view.detail}
          </p>
          <div className="relative z-1 mt-7 flex flex-wrap gap-2.5 max-[560px]:[&>button]:flex-[1_1_120px]">
            <Button
              disabled={!allowed.canStart || action.isPending}
              onClick={() => action.mutate("start")}
            >
              <Play size={16} />
              Start
            </Button>
            <Button
              variant="secondary"
              disabled={!allowed.canRestart || action.isPending}
              onClick={() => action.mutate("restart")}
            >
              <RotateCcw size={16} />
              Restart
            </Button>
            <Button
              variant="secondary"
              disabled={!allowed.canStop || action.isPending}
              onClick={() => action.mutate("stop")}
            >
              <Square size={15} />
              Stop
            </Button>
          </div>
        </Surface>

        <div className="grid gap-[18px]">
          <Surface className="p-6">
            <div className="flex items-center justify-between gap-3">
              <h2 className="m-0 font-display text-xl leading-[1.15] font-[800]">
                Connect
              </h2>
              <span>
                {server.observed_state === "online" ? "Ready" : "Wake on connect"}
              </span>
            </div>
            <div className="mt-[18px] grid gap-2.5">
              <AddressRow label="JAVA" value={server.java_address} />
              <AddressRow label="BEDROCK" value={server.bedrock_address} />
            </div>
          </Surface>

          <Surface className="p-6">
            <h2 className="m-0 font-display text-xl leading-[1.15] font-[800]">
              World resources
            </h2>
            <div className="mt-[22px] grid gap-[18px]">
              <div className="grid gap-2">
                <div className="flex justify-between gap-3 text-xs text-ink-soft">
                  <strong className="text-sm text-ink">
                    {formatBytes(server.storage_bytes)}
                  </strong>
                  <span>{formatBytes(server.storage_limit_bytes)} storage</span>
                </div>
                <Meter
                  label="Storage used"
                  max={server.storage_limit_bytes}
                  segments={[{ value: server.storage_bytes, className: "bg-grass" }]}
                />
              </div>
              <div className="grid grid-cols-2 gap-2.5 max-[560px]:grid-cols-1">
                <div className="rounded-[9px] bg-[#eee7d8] p-3.5">
                  <strong className="block font-display text-[21px] leading-[1.1] font-[800]">
                    {formatBytes(server.memory_limit_bytes)}
                  </strong>
                  <span className="mt-[5px] block text-xs text-ink-soft">
                    memory limit
                  </span>
                </div>
                <div className="rounded-[9px] bg-[#eee7d8] p-3.5">
                  <strong className="block font-display text-[21px] leading-[1.1] font-[800]">
                    {server.player_count}
                  </strong>
                  <span className="mt-[5px] block text-xs text-ink-soft">
                    online now
                  </span>
                </div>
              </div>
            </div>
          </Surface>
        </div>

        {server.error ? (
          <section
            className="col-[1/-1] rounded-md bg-[#f3ddd6] p-[22px] shadow-[inset_0_0_0_1px_#d9aaa0]"
            role="alert"
          >
            <h2 className="m-0 font-display text-[21px] leading-[1.2] font-[800] text-[#792f27]">
              {server.error.message}
            </h2>
            <p className="mt-2 mb-0 leading-normal text-[#713f37]">
              {server.error.retryable
                ? "The control plane marked this failure as recoverable. You can retry from the lifecycle controls."
                : "This failure needs intervention before another lifecycle operation can proceed."}
            </p>
            {server.error.detail ? (
              <details className="mt-3.5 text-[13px] text-[#624b46]">
                <summary>Technical detail</summary>
                <pre className="mt-2 whitespace-pre-wrap [overflow-wrap:anywhere] rounded-sm bg-white/45 p-3">
                  {server.error.code}
                  {"\n"}
                  {server.error.detail}
                </pre>
              </details>
            ) : null}
          </section>
        ) : null}
      </div>

      <div className="mt-[18px] grid grid-cols-[minmax(0,1.2fr)_minmax(280px,0.8fr)] gap-[18px] max-[970px]:grid-cols-1">
        <Surface className="p-6">
          <h2 className="m-0 font-display text-xl leading-[1.15] font-[800]">
            Recent activity
          </h2>
          {activity.length ? (
            <ul className="mt-[18px] mb-0 list-none p-0">
              {activity.slice(0, 8).map((item) => (
                <li
                  className="grid grid-cols-[11px_minmax(0,1fr)_auto] items-start gap-3 border-t border-[#ded6c7] py-[13px] max-[560px]:grid-cols-[11px_minmax(0,1fr)]"
                  key={item.id}
                >
                  <i className="mt-[5px] size-[9px] rounded-[3px] bg-stone-dark" />
                  <div>
                    <strong className="block text-[13px]">{item.summary}</strong>
                    {item.detail ? (
                      <p className="mt-1 mb-0 text-xs leading-[1.4] text-ink-soft">
                        {item.detail}
                      </p>
                    ) : null}
                  </div>
                  <time
                    className="whitespace-nowrap text-[11px] text-[#706e65] max-[560px]:col-start-2"
                    dateTime={item.created_at}
                  >
                    {formatRelative(item.created_at, relativeNow)}
                  </time>
                </li>
              ))}
            </ul>
          ) : (
            <EmptyState
              title="No activity yet"
              description="Lifecycle and backup events will appear here as the world is used."
            />
          )}
        </Surface>

        <Surface className="p-6">
          <h2 className="m-0 font-display text-xl leading-[1.15] font-[800]">
            Latest safe snapshot
          </h2>
          {server.latest_snapshot ? (
            <div className="mt-6">
              <strong className="block font-display text-[32px] leading-none font-[820]">
                Generation {server.latest_snapshot.generation}
              </strong>
              <span className="mt-[7px] block text-xs text-ink-soft">
                {formatBytes(server.latest_snapshot.size_bytes)} ·{" "}
                {formatDateTime(server.latest_snapshot.created_at)}
              </span>
            </div>
          ) : (
            <p className="leading-normal text-ink-soft">
              No completed snapshot has been published yet.
            </p>
          )}
          <Link
            className="mt-[22px] inline-flex items-center gap-[7px] text-[13px] font-[780] text-sky-deep no-underline"
            href={`${panelBasePath ?? `/manage/${serverId}`}/backups`}
          >
            Open backup history <ExternalLink size={14} />
          </Link>
        </Surface>
      </div>
    </>
  );
}
