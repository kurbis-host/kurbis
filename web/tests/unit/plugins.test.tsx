import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { File as NodeFile } from "node:buffer";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Plugins, syncNotice } from "../../components/plugins";
import type { components } from "../../lib/api/generated";
import { server } from "./fixtures";

type MarketPlugin = components["schemas"]["MarketPlugin"];
type ServerPlugin = components["schemas"]["ServerPlugin"];
type ServerPlugins = components["schemas"]["ServerPlugins"];

const stamp = "2026-10-13T12:00:00Z";
const tpsBar: ServerPlugin = {
  id: "00000000-0000-0000-0000-0000000000a1",
  name: "TPSBar",
  version: "0.1",
  source: "market",
  market_plugin_id: 7,
  enabled: true,
  state: "failed",
  error: "missing export on-load",
  sha256: "ab".repeat(32),
  size_bytes: 2048,
  created_at: stamp,
  updated_at: stamp,
};
const listing = (overrides: Partial<MarketPlugin>): MarketPlugin => ({
  id: 7,
  name: "TPSBar",
  version: "0.2",
  developer: "PING",
  category: "Admin Tools",
  description: "Performance in a boss bar.",
  downloads: 1200,
  icon_url: null,
  installed_plugin_id: tpsBar.id,
  installed_version: "0.1",
  ...overrides,
});
const market = [listing({}), listing({ id: 9, name: "Homes", version: "1.0", installed_plugin_id: null, installed_version: null })];
const installed = (plugins: ServerPlugin[], synchronized = true): ServerPlugins => ({
  plugins,
  revision: 2,
  applied_revision: synchronized ? 2 : 1,
  synchronized,
  max_plugins: 16,
  max_plugin_bytes: 4096,
});

type Request = { path: string; method: string; search: string; body: unknown; contentType: string | null };

function stubApi(handle: (request: Request) => Response | undefined) {
  const requests: Request[] = [];
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const raw = input instanceof Request ? input : new Request(new URL(String(input), "http://localhost"), init);
    const url = new URL(raw.url, "http://localhost");
    const contentType = raw.headers.get("content-type");
    const body = contentType === "application/json" ? await raw.clone().json() : await raw.clone().arrayBuffer().then((bytes) => bytes.byteLength || undefined);
    const request = { path: url.pathname, method: raw.method, search: url.search, body, contentType };
    requests.push(request);
    if (request.path.endsWith("/plugins/market") && request.method === "GET") return Response.json({ plugins: market });
    const response = handle(request);
    if (!response) throw new Error(`unexpected request ${request.method} ${request.path}`);
    return response;
  }));
  return requests;
}

function setup(plugins = installed([tpsBar])) {
  const client = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, retry: false }, mutations: { retry: false } } });
  render(<Plugins initialServer={server} initialPlugins={plugins} />, {
    wrapper: ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}>{children}</QueryClientProvider>,
  });
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("plugins page", () => {
  it("shows what the server does with each plugin and offers the market update", async () => {
    stubApi(() => undefined);
    setup();

    const installedList = screen.getByRole("heading", { name: "Installed" }).parentElement!;
    expect(within(installedList).getByText("Failed to load")).toBeVisible();
    expect(within(installedList).getByText("missing export on-load")).toBeVisible();
    expect(await within(installedList).findByRole("button", { name: /Update to 0.2/ })).toBeVisible();
    expect(screen.getByRole("button", { name: "Update TPSBar" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Install Homes" })).toBeEnabled();
  });

  it("installs a market listing and refreshes the installed list", async () => {
    const homes: ServerPlugin = { ...tpsBar, id: "00000000-0000-0000-0000-0000000000b2", name: "Homes", version: "1.0", market_plugin_id: 9, state: "loaded", error: null };
    let plugins = installed([tpsBar]);
    const requests = stubApi((request) => {
      if (request.path.endsWith("/plugins/market") && request.method === "POST") {
        plugins = installed([tpsBar, homes]);
        return Response.json(homes, { status: 201 });
      }
      if (request.path.endsWith("/plugins") && request.method === "GET") return Response.json(plugins);
      return undefined;
    });
    setup();
    const user = userEvent.setup();

    await user.click(await screen.findByRole("button", { name: "Install Homes" }));
    await waitFor(() => expect(screen.getByText("Running")).toBeVisible());
    expect(requests.find((request) => request.method === "POST")?.body).toEqual({ market_plugin_id: 9 });
  });

  it("uploads a wasm file as the raw request body", async () => {
    const requests = stubApi((request) => {
      if (request.path.endsWith("/plugins/uploads")) return Response.json({ ...tpsBar, source: "upload", market_plugin_id: null }, { status: 201 });
      if (request.path.endsWith("/plugins") && request.method === "GET") return Response.json(installed([tpsBar]));
      return undefined;
    });
    setup();
    const user = userEvent.setup();

    // jsdom's File is not a Blob to Node's fetch, which a browser's File is.
    const file = new NodeFile([new Uint8Array(100)], "mine.wasm", { type: "application/wasm" }) as unknown as File;
    await user.upload(screen.getByLabelText("Plugin file"), file);
    await waitFor(() => expect(requests.some((request) => request.path.endsWith("/plugins/uploads"))).toBe(true));
    const upload = requests.find((request) => request.path.endsWith("/plugins/uploads"))!;
    expect(upload).toMatchObject({ method: "POST", search: "?file_name=mine.wasm", body: 100, contentType: "application/wasm" });
  });

  it("refuses an oversized file before uploading it", async () => {
    const requests = stubApi(() => undefined);
    setup();
    const user = userEvent.setup();

    await user.upload(screen.getByLabelText("Plugin file"), new File([new Uint8Array(5000)], "big.wasm", { type: "application/wasm" }));
    expect(await screen.findByText(/big.wasm is larger than/)).toBeVisible();
    expect(requests.some((request) => request.path.endsWith("/plugins/uploads"))).toBe(false);
  });

  it("explains when the server has not loaded the latest list", () => {
    expect(syncNotice(installed([], true), server)).toBeNull();
    expect(syncNotice(installed([], false), { ...server, observed_state: "online" })).toMatch(/running server/);
    expect(syncNotice(installed([], false), { ...server, observed_state: "sleeping" })).toMatch(/next starts/);
  });
});
