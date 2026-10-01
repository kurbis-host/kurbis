"use client";

import { useQuery } from "@tanstack/react-query";
import { Clipboard, Download, Pause, Play, Search } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { fetchLogs } from "../lib/api/queries";
import type { LogEntry, LogPage } from "../lib/contracts";
import { queryKeys } from "../lib/query";
import { Button, PageHeader } from "./ui";

function toText(items: LogEntry[]) {
  return items
    .map(
      (entry) =>
        `[${new Date(entry.created_at).toISOString()}] ${entry.line}`,
    )
    .join("\n");
}

export function Console({ serverId, initialLogs }: { serverId: string; initialLogs: LogPage }) {
  const { data = initialLogs } = useQuery({
    queryKey: queryKeys.logs(serverId),
    queryFn: () => fetchLogs(serverId),
    initialData: initialLogs,
  });
  const [paused, setPaused] = useState(false);
  const [pausedItems, setPausedItems] = useState<LogEntry[] | null>(null);
  const [autoscroll, setAutoscroll] = useState(true);
  const [filter, setFilter] = useState("");
  const viewport = useRef<HTMLDivElement>(null);
  const source = paused && pausedItems ? pausedItems : data.items;
  const filtered = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    return needle
      ? source.filter((entry) => entry.line.toLowerCase().includes(needle))
      : source;
  }, [filter, source]);

  useEffect(() => {
    if (autoscroll && !paused && viewport.current) {
      viewport.current.scrollTop = viewport.current.scrollHeight;
    }
  }, [data.items.length, autoscroll, paused]);

  function togglePause() {
    if (paused) {
      setPaused(false);
      setPausedItems(null);
      return;
    }
    setPausedItems(data.items);
    setPaused(true);
  }

  async function copy() {
    await navigator.clipboard.writeText(toText(filtered));
  }

  function download() {
    const blob = new Blob([toText(filtered)], {
      type: "text/plain;charset=utf-8",
    });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `kurbis-console-${new Date()
      .toISOString()
      .replaceAll(":", "-")}.log`;
    anchor.click();
    URL.revokeObjectURL(url);
  }

  return (
    <>
      <PageHeader
        title="Console"
        description="Read-only live runtime logs. SSE keeps the cache current while the cursor response provides the initial history."
      />
      <div className="mb-3.5 flex flex-wrap items-center justify-between gap-2.5 max-[620px]:flex-col max-[620px]:items-stretch">
        <div className="flex flex-wrap items-center gap-2 max-[620px]:w-full max-[620px]:[&>button]:flex-1">
          <label className="relative max-[620px]:w-full">
            <span className="absolute top-[11px] left-[11px] text-[#777]">
              <Search size={17} />
            </span>
            <input
              className="min-h-[42px] min-w-[min(320px,100%)] rounded-sm border border-[#bdb5a4] bg-[#fffdf8] py-0 pr-3 pl-9 max-[620px]:w-full"
              placeholder="Filter log lines"
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
              aria-label="Filter console logs"
            />
          </label>
          <Button variant="secondary" size="small" onClick={togglePause}>
            {paused ? (
              <>
                <Play size={15} />
                Resume
              </>
            ) : (
              <>
                <Pause size={15} />
                Pause
              </>
            )}
          </Button>
          <Button variant="secondary" size="small" onClick={() => void copy()}>
            <Clipboard size={15} />
            Copy
          </Button>
          <Button variant="secondary" size="small" onClick={download}>
            <Download size={15} />
            Download
          </Button>
        </div>

        <div className="flex items-center gap-3.5 text-xs text-ink-soft">
          <span className="inline-flex items-center gap-1.5">
            <i
              className={`size-2 rounded-full ${paused ? "bg-warning" : "bg-success"}`}
            />
            {paused ? "Paused locally" : "Live"}
          </span>
          <label>
            <input
              type="checkbox"
              checked={autoscroll}
              onChange={(event) => setAutoscroll(event.target.checked)}
            />{" "}
            Autoscroll
          </label>
          <span>{filtered.length} lines</span>
        </div>
      </div>

      <div
        ref={viewport}
        className="h-[min(62vh,620px)] overflow-auto rounded-md bg-[#211f1c] py-3 font-mono text-xs leading-[1.55] text-[#e8e0d2] shadow-[inset_0_0_0_1px_rgba(255,255,255,0.06),var(--shadow-low)] max-[620px]:h-[58vh]"
        role="log"
        aria-live="off"
        aria-label="Server console log"
      >
        {filtered.length ? (
          filtered.map((entry) => (
            <div
              className="grid grid-cols-[92px_minmax(0,1fr)] gap-3.5 px-4 py-0.5 hover:bg-white/[0.045] max-[620px]:grid-cols-[72px_minmax(0,1fr)] max-[620px]:gap-2 max-[620px]:px-2.5 max-[620px]:text-[11px]"
              key={entry.id}
            >
              <time
                className="select-none text-[#928c81]"
                dateTime={entry.created_at}
              >
                {new Date(entry.created_at).toLocaleTimeString()}
              </time>
              <span className="whitespace-pre-wrap [overflow-wrap:anywhere]">{entry.line}</span>
            </div>
          ))
        ) : (
          <div className="grid min-h-60 place-items-center p-5 text-center text-[#aaa296]">
            {filter
              ? "No log lines match this filter."
              : "No runtime logs yet. Start the world and output will appear here."}
          </div>
        )}
      </div>
    </>
  );
}
