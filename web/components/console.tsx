"use client";

import { useQuery } from "@tanstack/react-query";
import { Clipboard, Download, Pause, Play, Search, Send } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type CSSProperties, type FormEvent } from "react";
import type { components } from "../lib/api/generated";
import { errorMessage } from "../lib/api/client";
import { browserApi } from "../lib/api/operations";
import {
  normalizeConsoleCommand,
  parseConsoleText,
  readableConsoleText,
  visibleConsoleText,
} from "../lib/console";
import { queryKeys } from "../lib/query";
import { Button, Notice, PageHeader, useReadOnly } from "./ui";

type LogEntry = components["schemas"]["LogEntry"];
type LogPage = components["schemas"]["LogPage"];
type Server = components["schemas"]["Server"];
type CommandFeedback = {
  tone: "info" | "success" | "danger" | "warning";
  message: string;
};

function tokenStyle(style: ReturnType<typeof parseConsoleText>[number]["style"]): CSSProperties {
  return {
    color: style.foreground,
    backgroundColor: style.background,
    fontWeight: style.bold ? 700 : undefined,
    fontStyle: style.italic ? "italic" : undefined,
    textDecoration: [style.underline ? "underline" : "", style.strike ? "line-through" : ""].filter(Boolean).join(" ") || undefined,
    opacity: style.dim ? 0.72 : undefined,
  };
}

function LogLine({ entry }: { entry: LogEntry }) {
  const tokens = useMemo(() => parseConsoleText(entry.line), [entry.line]);
  return (
    <div className="grid grid-cols-[92px_minmax(0,1fr)] gap-3.5 px-4 py-0.5 hover:bg-white/[0.045] max-[620px]:grid-cols-[72px_minmax(0,1fr)] max-[620px]:gap-2 max-[620px]:px-2.5 max-[620px]:text-[11px]">
      <time
        className="select-none text-[#928c81]"
        dateTime={entry.created_at}
        title={new Date(entry.created_at).toLocaleString()}
      >
        {new Date(entry.created_at).toLocaleTimeString()}
      </time>
      <span className="whitespace-pre-wrap [overflow-wrap:anywhere]">
        {tokens.map((token, index) => (
          <span key={`${entry.id}-${index}`} style={tokenStyle(token.style)}>
            {token.text}
          </span>
        ))}
      </span>
    </div>
  );
}

async function copyPlainText(text: string) {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(text);
    return;
  }
  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.setAttribute("readonly", "true");
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  textarea.select();
  const copied = document.execCommand("copy");
  textarea.remove();
  if (!copied) throw new Error("Clipboard access is unavailable.");
}

export function Console({
  serverId,
  initialLogs,
  initialServer,
  submitCommand = browserApi.sendConsoleCommand,
}: {
  serverId: string;
  initialLogs: LogPage;
  initialServer?: Server;
  submitCommand?: (serverId: string, command: string, requestId: string) => Promise<void>;
}) {
  const { data = initialLogs } = useQuery({
    queryKey: queryKeys.logs(serverId),
    queryFn: () => browserApi.getLogs(serverId),
    initialData: initialLogs,
  });
  const { data: server = initialServer } = useQuery({
    queryKey: queryKeys.server(serverId),
    queryFn: () => browserApi.getServer(serverId),
    enabled: Boolean(serverId),
    initialData: initialServer,
  });
  const [paused, setPaused] = useState(false);
  const [pausedItems, setPausedItems] = useState<LogEntry[] | null>(null);
  const [autoscroll, setAutoscroll] = useState(true);
  const [filter, setFilter] = useState("");
  const [command, setCommand] = useState("");
  const [feedback, setFeedback] = useState<CommandFeedback | null>(null);
  const [sendingCommand, setSendingCommand] = useState(false);
  const pendingCommand = useRef<{ command: string; requestId: string } | null>(null);
  const viewport = useRef<HTMLDivElement>(null);
  const source = paused && pausedItems ? pausedItems : data.items;
  const needle = filter.trim().toLocaleLowerCase();
  const filtered = useMemo(
    () => needle
      ? source.filter((entry) => visibleConsoleText(entry.line).toLocaleLowerCase().includes(needle))
      : source,
    [needle, source],
  );
  const online = server?.observed_state === "online";
  const readOnly = useReadOnly();

  useEffect(() => {
    if (autoscroll && !paused && viewport.current) {
      viewport.current.scrollTop = viewport.current.scrollHeight;
    }
  }, [data.items, autoscroll, paused]);

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
    if (!filtered.length) return;
    try {
      await copyPlainText(readableConsoleText(filtered));
      setFeedback({ tone: "success", message: `Copied ${filtered.length} ${filtered.length === 1 ? "line" : "lines"}.` });
    } catch (error) {
      setFeedback({ tone: "danger", message: errorMessage(error) });
    }
  }

  function download() {
    if (!filtered.length) return;
    const blob = new Blob([readableConsoleText(filtered)], { type: "text/plain;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `kurbis-console-${new Date().toISOString().replaceAll(":", "-")}.log`;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
    setFeedback({ tone: "success", message: `Downloaded ${filtered.length} ${filtered.length === 1 ? "line" : "lines"}.` });
  }

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const validation = normalizeConsoleCommand(command);
    if (!validation.success) {
      const message = validation.reason === "control_character"
        ? "Commands must be a single line without control characters."
        : validation.reason === "too_long"
          ? "Commands must be 512 characters or fewer."
          : "Enter a Minecraft command first.";
      setFeedback({ tone: "warning", message });
      return;
    }
    if (!online) {
      setFeedback({ tone: "warning", message: "Commands are available when the runtime is online." });
      return;
    }
    if (sendingCommand) return;

    const normalizedCommand = validation.command;
    if (pendingCommand.current?.command !== normalizedCommand) {
      pendingCommand.current = { command: normalizedCommand, requestId: globalThis.crypto.randomUUID() };
    }
    const requestId = pendingCommand.current.requestId;
    setCommand("");
    setSendingCommand(true);
    setFeedback({ tone: "info", message: `Sending /${normalizedCommand}…` });
    void submitCommand(serverId, normalizedCommand, requestId)
      .then(() => {
        if (pendingCommand.current?.requestId === requestId) pendingCommand.current = null;
        setFeedback({ tone: "success", message: `Command queued. Output will appear in the live log.` });
      })
      .catch((error: unknown) => {
        setFeedback({ tone: "danger", message: `Command failed: ${errorMessage(error)}` });
        setCommand((current) => current || command.trim());
      })
      .finally(() => setSendingCommand(false));
  }

  return (
    <>
      <PageHeader
        title="Console"
        description="Read live runtime output, send a command while the world is online, and copy a clean text version for troubleshooting."
      />

      <div className="mb-3.5 flex flex-wrap items-center justify-between gap-2.5 max-[620px]:flex-col max-[620px]:items-stretch">
        <div className="flex flex-wrap items-center gap-2 max-[620px]:w-full max-[620px]:[&>button]:flex-1">
          <label className="relative max-[620px]:w-full">
            <span className="absolute top-[11px] left-[11px] text-[#777]">
              <Search size={17} aria-hidden="true" />
            </span>
            <input
              className="min-h-[42px] min-w-[min(320px,100%)] rounded-sm border border-[#bdb5a4] bg-[#fffdf8] py-0 pr-3 pl-9 max-[620px]:w-full"
              placeholder="Filter visible log text"
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
              aria-label="Filter console logs"
            />
          </label>
          <Button type="button" variant="secondary" size="small" readSafe onClick={togglePause}>
            {paused ? <><Play size={15} aria-hidden="true" /> Resume</> : <><Pause size={15} aria-hidden="true" /> Pause</>}
          </Button>
          <Button type="button" variant="secondary" size="small" readSafe onClick={() => void copy()} disabled={!filtered.length}>
            <Clipboard size={15} aria-hidden="true" /> Copy
          </Button>
          <Button type="button" variant="secondary" size="small" readSafe onClick={download} disabled={!filtered.length}>
            <Download size={15} aria-hidden="true" /> Download
          </Button>
        </div>

        <div className="flex items-center gap-3.5 text-xs text-ink-soft">
          <span className="inline-flex items-center gap-1.5">
            <i className={`size-2 rounded-full ${paused ? "bg-warning" : "bg-success"}`} aria-hidden="true" />
            {paused ? "Paused locally" : "Live"}
          </span>
          <label>
            <input type="checkbox" checked={autoscroll} onChange={(event) => setAutoscroll(event.target.checked)} /> Autoscroll
          </label>
          <span>{filtered.length} lines</span>
        </div>
      </div>

      {readOnly ? null : <section className="mb-4 rounded-md bg-paper p-4 shadow-[inset_0_0_0_1px_rgba(83,70,49,0.13),var(--shadow-low)] max-[620px]:p-3.5" aria-labelledby="console-command-heading">
        <div className="mb-2 flex flex-wrap items-baseline justify-between gap-2">
          <div>
            <h2 id="console-command-heading" className="m-0 font-display text-base font-[800]">Run a Minecraft command</h2>
            <p className="mt-1 mb-0 text-xs text-ink-soft">Commands are sent to Pumpkin without waiting for their output.</p>
          </div>
          <span className={`inline-flex items-center gap-1.5 text-xs font-[700] ${online ? "text-success" : "text-ink-soft"}`}>
            <i className={`size-2 rounded-full ${online ? "bg-success" : "bg-[#96958b]"}`} aria-hidden="true" />
            {online ? "Runtime online" : `Unavailable while ${server?.observed_state ?? "state is loading"}`}
          </span>
        </div>
        <form className="flex items-stretch gap-2 max-[620px]:flex-col" onSubmit={submit}>
          <div className="flex min-w-0 flex-1 items-center rounded-sm border border-[#bdb5a4] bg-[#fffdf8] px-3 font-mono text-sm focus-within:outline-3 focus-within:outline-offset-2 focus-within:outline-focus">
            <span className="select-none text-[#8e8779]" aria-hidden="true">/</span>
            <input
              id="console-command"
              aria-label="Minecraft command"
              className="min-h-11 min-w-0 flex-1 border-0 bg-transparent px-1.5 py-2.5 outline-none disabled:cursor-not-allowed disabled:text-[#777366]"
              value={command}
              onChange={(event) => setCommand(event.target.value)}
              placeholder={online ? "say Welcome to Kurbis" : "Start the runtime to enable commands"}
              disabled={!online}
              maxLength={513}
              autoComplete="off"
              spellCheck={false}
              enterKeyHint="send"
            />
          </div>
          <Button type="submit" disabled={!online || !command.trim() || sendingCommand}>
            <Send size={16} aria-hidden="true" />
            {sendingCommand ? "Sending…" : "Send command"}
          </Button>
        </form>
        {feedback ? <div className="mt-3" role={feedback.tone === "danger" ? undefined : "status"} aria-live="polite"><Notice tone={feedback.tone === "success" ? "info" : feedback.tone}>{feedback.message}</Notice></div> : null}
      </section>}

      <div
        ref={viewport}
        className="h-[min(62vh,620px)] overflow-auto rounded-md bg-[#211f1c] py-3 font-mono text-xs leading-[1.55] text-[#e8e0d2] shadow-[inset_0_0_0_1px_rgba(255,255,255,0.06),var(--shadow-low)] max-[620px]:h-[58vh]"
        role="log"
        aria-live="off"
        aria-label="Server console log"
      >
        {filtered.length ? filtered.map((entry) => <LogLine entry={entry} key={entry.id} />) : (
          <div className="grid min-h-60 place-items-center p-5 text-center text-[#aaa296]">
            {filter ? "No log lines match this filter." : "No runtime logs yet. Start the world and output will appear here."}
          </div>
        )}
      </div>
    </>
  );
}
