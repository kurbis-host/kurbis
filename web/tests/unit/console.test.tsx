import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Console } from "../../components/console";
import { normalizeConsoleCommand, parseConsoleText, readableConsoleText, visibleConsoleText } from "../../lib/console";
import { queryKeys } from "../../lib/query";
import { server } from "./fixtures";

const logs = {
  items: [
    { id: 1, created_at: "2026-10-03T10:15:00.000Z", line: "\u001b[32mServer ready\u001b[0m" },
    { id: 2, created_at: "2026-10-03T10:15:01.000Z", line: "§cPlayer Alex joined" },
  ],
  next_after_id: 2,
};

function renderConsole({
  online = true,
  submitCommand = vi.fn(async () => undefined),
}: {
  online?: boolean;
  submitCommand?: (serverId: string, command: string, requestId: string) => Promise<void>;
} = {}) {
  const client = new QueryClient({
    defaultOptions: { queries: { staleTime: Infinity, retry: false }, mutations: { retry: false } },
  });
  const initialServer = { ...server, observed_state: online ? "online" as const : "sleeping" as const };
  function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  }
  const view = render(
    <Console serverId={server.id} initialServer={initialServer} initialLogs={logs} submitCommand={submitCommand} />,
    { wrapper: Wrapper },
  );
  return { ...view, client, submitCommand };
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("console text parsing", () => {
  it("renders ANSI SGR colors and formatting as separate safe text runs", () => {
    expect(parseConsoleText("plain \u001b[1;31mred bold\u001b[0m end")).toEqual([
      { text: "plain ", style: {} },
      { text: "red bold", style: { bold: true, foreground: "#aa0000" } },
      { text: " end", style: {} },
    ]);
  });

  it("supports standard Minecraft colors, formatting, reset, and hex color sequences", () => {
    expect(parseConsoleText("§aGreen §lbold§r normal §x§1§2§a§B§c§Dhex")).toEqual([
      { text: "Green ", style: { foreground: "#55ff55" } },
      { text: "bold", style: { foreground: "#55ff55", bold: true } },
      { text: " normal ", style: {} },
      { text: "hex", style: { foreground: "#12aBcD" } },
    ]);
  });

  it("removes ANSI control sequences, carriage returns, and formatting codes from readable text", () => {
    const source = "\u001b]0;private title\u0007§cred\u001b[2K\r text\tend";
    expect(visibleConsoleText(source)).toBe("red text\tend");
    expect(visibleConsoleText("\u009b32mgreen\u009b0m")).toBe("green");
    expect(readableConsoleText([{ created_at: "2026-10-03T10:15:00.000Z", line: source }]))
      .toBe("[2026-10-03T10:15:00.000Z] red text\tend");
  });

  it("normalizes one valid command line and rejects empty, control, and overlong input", () => {
    expect(normalizeConsoleCommand("  //say hello  ")).toEqual({ success: true, command: "say hello" });
    expect(normalizeConsoleCommand(" // ")).toEqual({ success: false, reason: "empty" });
    expect(normalizeConsoleCommand("say hi\nlist")).toEqual({ success: false, reason: "control_character" });
    expect(normalizeConsoleCommand(`say ${"x".repeat(509)}`)).toEqual({ success: false, reason: "too_long" });
  });

  it("supports 256-color and true-color ANSI foreground and background codes", () => {
    expect(parseConsoleText("\u001b[38;5;196mred\u001b[48;2;10;20;30m back")).toEqual([
      { text: "red", style: { foreground: "rgb(255, 0, 0)" } },
      { text: " back", style: { foreground: "rgb(255, 0, 0)", background: "rgb(10, 20, 30)" } },
    ]);
  });
});

describe("console command and log actions", () => {
  it("disables command input when the runtime is not online", () => {
    renderConsole({ online: false });
    expect(screen.getByRole("textbox", { name: "Minecraft command" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Send command" })).toBeDisabled();
    expect(screen.getByText("Unavailable while sleeping")).toBeVisible();
  });

  it("enables command input when the runtime is online", () => {
    renderConsole({ online: true });
    expect(screen.getByRole("textbox", { name: "Minecraft command" })).toBeEnabled();
  });

  it("tracks the server state updated in the shared live query cache", async () => {
    const { client } = renderConsole({ online: false });
    const input = screen.getByRole("textbox", { name: "Minecraft command" });
    expect(input).toBeDisabled();

    act(() => client.setQueryData(queryKeys.server(server.id), { ...server, observed_state: "online" }));
    await waitFor(() => expect(input).toBeEnabled());

    act(() => client.setQueryData(queryKeys.server(server.id), { ...server, observed_state: "stopping" }));
    await waitFor(() => expect(input).toBeDisabled());
  });

  it("sends a trimmed bare command without waiting for runtime output", async () => {
    const submitCommand = vi.fn(async () => undefined);
    renderConsole({ submitCommand });
    const input = screen.getByRole("textbox", { name: "Minecraft command" });
    await userEvent.type(input, "  /say Hello players  ");
    await userEvent.click(screen.getByRole("button", { name: "Send command" }));

    expect(submitCommand).toHaveBeenCalledWith(server.id, "say Hello players", expect.any(String));
    expect(await screen.findByText("Command queued. Output will appear in the live log.")).toBeVisible();
    expect(input).toHaveValue("");
  });

  it("retains a rejected command and explains the failure", async () => {
    const submitCommand = vi.fn(async () => { throw new Error("runtime unavailable"); });
    renderConsole({ submitCommand });
    const input = screen.getByRole("textbox", { name: "Minecraft command" });
    await userEvent.type(input, "list");
    await userEvent.click(screen.getByRole("button", { name: "Send command" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Command failed: runtime unavailable");
    expect(input).toHaveValue("list");
  });

  it("reuses the same idempotency key when retrying an uncertain submission", async () => {
    const submitCommand = vi.fn()
      .mockRejectedValueOnce(new Error("connection lost"))
      .mockResolvedValueOnce(undefined);
    renderConsole({ submitCommand });
    const input = screen.getByRole("textbox", { name: "Minecraft command" });
    const send = screen.getByRole("button", { name: "Send command" });
    await userEvent.type(input, "list");
    await userEvent.click(send);
    await screen.findByRole("alert");
    await userEvent.click(send);

    expect(submitCommand).toHaveBeenCalledTimes(2);
    expect(submitCommand.mock.calls[1][2]).toBe(submitCommand.mock.calls[0][2]);
    expect(await screen.findByText("Command queued. Output will appear in the live log.")).toBeVisible();
  });

  it("renders styled log text without displaying ANSI or Minecraft formatting codes", () => {
    renderConsole();
    expect(screen.getByText("Server ready")).toHaveStyle({ color: "rgb(0, 170, 0)" });
    expect(screen.getByText("Player Alex joined")).toHaveStyle({ color: "rgb(255, 85, 85)" });
    expect(screen.getByRole("log")).not.toHaveTextContent("§");
    expect(screen.getByRole("log")).not.toHaveTextContent("\u001b");
  });

  it("filters by visible text, not ANSI or section-sign format codes", async () => {
    renderConsole();
    await userEvent.type(screen.getByRole("textbox", { name: "Filter console logs" }), "player alex");
    expect(screen.getByRole("log")).toHaveTextContent("Player Alex joined");
    expect(screen.getByRole("log")).not.toHaveTextContent("Server ready");
  });

  it("copies only matching lines as readable, timestamped plain text", async () => {
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    renderConsole();
    await userEvent.type(screen.getByRole("textbox", { name: "Filter console logs" }), "ready");
    await userEvent.click(screen.getByRole("button", { name: "Copy" }));

    await waitFor(() => expect(writeText).toHaveBeenCalledWith("[2026-10-03T10:15:00.000Z] Server ready"));
    expect(screen.getByRole("status")).toHaveTextContent("Copied 1 line.");
  });

  it("downloads filtered text without terminal formatting", async () => {
    const createObjectURL = vi.fn(() => "blob:console");
    const revokeObjectURL = vi.fn();
    vi.stubGlobal("URL", class extends URL {
      static createObjectURL = createObjectURL;
      static revokeObjectURL = revokeObjectURL;
    });
    const click = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => undefined);
    renderConsole();
    await userEvent.type(screen.getByRole("textbox", { name: "Filter console logs" }), "Alex");
    await userEvent.click(screen.getByRole("button", { name: "Download" }));

    expect(createObjectURL).toHaveBeenCalledWith(expect.any(Blob));
    expect(click).toHaveBeenCalledOnce();
    expect(await screen.findByText("Downloaded 1 line.")).toBeVisible();
    await waitFor(() => expect(revokeObjectURL).toHaveBeenCalledWith("blob:console"), { timeout: 2000 });
  });
});
