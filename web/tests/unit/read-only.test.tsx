import * as Switch from "@radix-ui/react-switch";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, within } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Console } from "../../components/console";
import { Players } from "../../components/players";
import { Plugins } from "../../components/plugins";
import { Settings } from "../../components/settings";
import { Button, ReadOnlyProvider, SwitchField, useReadOnly } from "../../components/ui";
import type { components } from "../../lib/api/generated";
import type { OperatorRoster } from "../../lib/player-roster";
import { queryKeys } from "../../lib/query";
import { server } from "./fixtures";

vi.mock("next/navigation", () => ({ useRouter: () => ({ replace: vi.fn(), push: vi.fn() }) }));

type Whitelist = components["schemas"]["Whitelist"];
type ServerPlugin = components["schemas"]["ServerPlugin"];

const stamp = "2026-10-13T12:00:00Z";
const online = { ...server, observed_state: "online" as const, desired_state: "running" as const, settings: { ...server.settings, whitelist: true } };
const alex = { player_id: "00000000-0000-0000-0000-000000000001", name: "Alex", edition: "java" as const, connected_at: stamp, last_seen_at: stamp };
const whitelist: Whitelist = {
  enabled: true,
  whitelist_by_members: true,
  owner_has_minecraft_account: true,
  requests: [{ player_id: "00000000-0000-0000-0000-000000000002", name: "Steve", attempts: 2, first_attempt_at: stamp, last_attempt_at: stamp }],
  revision: 1,
  applied_revision: 1,
  synchronized: true,
  entries: [{ player_id: "00000000-0000-0000-0000-000000000003", name: "Notch", added_by: null, added_in_game: false, created_at: stamp, updated_at: stamp }],
};
const operators: OperatorRoster = {
  revision: 1,
  applied_revision: 1,
  synchronized: true,
  entries: [{ player_id: "00000000-0000-0000-0000-000000000004", name: "Jeb", created_at: stamp, updated_at: stamp, is_owner: false }],
};
const plugin: ServerPlugin = {
  id: "00000000-0000-0000-0000-0000000000a1",
  name: "TPSBar",
  version: "0.1",
  source: "upload",
  market_plugin_id: null,
  enabled: true,
  state: "loaded",
  error: null,
  sha256: "ab".repeat(32),
  size_bytes: 2048,
  created_at: stamp,
  updated_at: stamp,
};

function renderIn(readOnly: boolean, ui: ReactNode, seed?: (client: QueryClient) => void) {
  const client = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, retry: false }, mutations: { retry: false } } });
  client.setQueryData(queryKeys.server(server.id), online);
  seed?.(client);
  return render(
    <QueryClientProvider client={client}>
      <ReadOnlyProvider value={readOnly}>{ui}</ReadOnlyProvider>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("read-only primitives", () => {
  function Probe() {
    return <span>{useReadOnly() ? "reading" : "editing"}</span>;
  }

  it("is off outside a provider and on inside one", () => {
    render(<Probe />);
    expect(screen.getByText("editing")).toBeVisible();
    cleanup();
    render(<ReadOnlyProvider value><Probe /></ReadOnlyProvider>);
    expect(screen.getByText("reading")).toBeVisible();
  });

  it("disables mutating buttons and switches but keeps readSafe buttons usable", () => {
    const onChange = vi.fn();
    render(
      <ReadOnlyProvider value>
        <Button>Delete</Button>
        <Button readSafe>Copy</Button>
        <Button readSafe disabled>Download</Button>
        <SwitchField checked={false} onCheckedChange={onChange} label="Hardcore" description="One life." />
      </ReadOnlyProvider>,
    );
    expect(screen.getByRole("button", { name: "Delete" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Copy" })).toBeEnabled();
    // readSafe never overrides an explicit disabled.
    expect(screen.getByRole("button", { name: "Download" })).toBeDisabled();
    expect(screen.getByRole("switch", { name: "Hardcore" })).toBeDisabled();
  });

  it("leaves everything enabled when the panel is editable", () => {
    render(
      <ReadOnlyProvider value={false}>
        <Button>Delete</Button>
        <SwitchField checked={false} onCheckedChange={() => undefined} label="Hardcore" description="One life." />
        <Switch.Root aria-label="raw" />
      </ReadOnlyProvider>,
    );
    expect(screen.getByRole("button", { name: "Delete" })).toBeEnabled();
    expect(screen.getByRole("switch", { name: "Hardcore" })).toBeEnabled();
  });
});

describe("feature panels in read-only mode", () => {
  const logs = { items: [{ id: 1, created_at: stamp, line: "Server ready" }], next_after_id: 1 };

  it("console hides the command form but keeps reading tools", () => {
    renderIn(true, <Console serverId={server.id} initialServer={online} initialLogs={logs} submitCommand={vi.fn()} />);
    expect(screen.queryByRole("textbox", { name: "Minecraft command" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Send command" })).not.toBeInTheDocument();
    expect(screen.getByRole("log")).toHaveTextContent("Server ready");
    expect(screen.getByRole("button", { name: "Copy" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Download" })).toBeEnabled();
    expect(screen.getByRole("textbox", { name: "Filter console logs" })).toBeEnabled();
  });

  it("console keeps the command form when editable", () => {
    renderIn(false, <Console serverId={server.id} initialServer={online} initialLogs={logs} submitCommand={vi.fn()} />);
    expect(screen.getByRole("textbox", { name: "Minecraft command" })).toBeEnabled();
  });

  function renderPlayers(readOnly: boolean) {
    renderIn(readOnly, <Players initialServer={online} initialPlayers={[alex]} initialWhitelist={whitelist} />, (client) => {
      client.setQueryData(queryKeys.players(server.id), [alex]);
      client.setQueryData(queryKeys.whitelist(server.id), whitelist);
      client.setQueryData(queryKeys.operators(server.id), operators);
    });
  }

  it("players disables every add, answer and remove control", () => {
    renderPlayers(true);
    expect(screen.getByRole("textbox", { name: /Add by player name/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Add to whitelist" })).toBeDisabled();
    expect(screen.getByRole("textbox", { name: /Add trusted player by name/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Grant operator" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Remove Notch from whitelist" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Remove Jeb as operator" })).toBeDisabled();
    expect(screen.getByRole("button", { name: /Grant OP/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: /Let in/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Dismiss Steve's request" })).toBeDisabled();
  });

  it("players keeps the same controls enabled when editable", () => {
    renderPlayers(false);
    expect(screen.getByRole("textbox", { name: /Add by player name/ })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Remove Notch from whitelist" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Remove Jeb as operator" })).toBeEnabled();
    expect(screen.getByRole("button", { name: /Let in/ })).toBeEnabled();
  });

  it("settings disables every field, switch and the save button", () => {
    const sleeping = { ...server };
    renderIn(true, <Settings initialServer={sleeping} />, (client) => client.setQueryData(queryKeys.server(server.id), sleeping));
    for (const name of ["Address", "MOTD"]) expect(screen.getByRole("textbox", { name })).toBeDisabled();
    expect(screen.getByRole("combobox", { name: "Difficulty" })).toBeDisabled();
    expect(screen.getByRole("combobox", { name: "Gamemode" })).toBeDisabled();
    for (const name of ["Hardcore", "Whitelist", "Members can let players in"]) expect(screen.getByRole("switch", { name })).toBeDisabled();
    expect(screen.getByRole("button", { name: /save configuration/i })).toBeDisabled();
    for (const button of screen.getAllByRole("button")) expect(button).toBeDisabled();
  });

  it("settings fields are editable for the owner of a sleeping server", () => {
    const sleeping = { ...server };
    renderIn(false, <Settings initialServer={sleeping} />, (client) => client.setQueryData(queryKeys.server(server.id), sleeping));
    expect(screen.getByRole("textbox", { name: "MOTD" })).toBeEnabled();
    expect(screen.getByRole("switch", { name: "Hardcore" })).toBeEnabled();
  });

  function stubPluginApi() {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = input instanceof Request ? input : new Request(new URL(String(input), "http://localhost"), init);
      if (new URL(request.url).pathname.endsWith("/plugins/market")) return Response.json({ plugins: [] });
      throw new Error(`unexpected request ${request.method} ${request.url}`);
    }));
  }
  const plugins = { plugins: [plugin], revision: 1, applied_revision: 1, synchronized: true, max_plugins: 16, max_plugin_bytes: 4096 };

  it("plugins disables the on/off switch and the remove button", () => {
    stubPluginApi();
    renderIn(true, <Plugins initialServer={online} initialPlugins={plugins} />);
    const installed = screen.getByRole("heading", { name: "Installed" }).parentElement!;
    expect(within(installed).getByRole("switch", { name: "Turn off TPSBar" })).toBeDisabled();
    expect(within(installed).getByRole("button", { name: "Remove TPSBar" })).toBeDisabled();
  });

  it("plugins keeps the switch and remove enabled when editable", () => {
    stubPluginApi();
    renderIn(false, <Plugins initialServer={online} initialPlugins={plugins} />);
    expect(screen.getByRole("switch", { name: "Turn off TPSBar" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Remove TPSBar" })).toBeEnabled();
  });
});
