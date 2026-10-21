import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AdminFailuresPage } from "../../components/admin/failures";
import { AdminOverviewPage } from "../../components/admin/overview";
import { isServerFilter } from "../../components/admin/server-filters";
import { AdminServersPage } from "../../components/admin/servers";
import type { components } from "../../lib/api/generated";
import { ApiError } from "../../lib/api/client";
import { browserApi } from "../../lib/api/operations";

type AdminServer = components["schemas"]["AdminServer"];
type AdminFailures = components["schemas"]["AdminFailures"];
type AdminOverview = components["schemas"]["AdminOverview"];

const now = Date.now();
const ago = (ms: number) => new Date(now - ms).toISOString();

const failedServer: AdminServer = {
  id: "00000000-0000-4000-8000-000000000001",
  subdomain: "broken-world",
  owner_id: "00000000-0000-4000-8000-0000000000f1",
  owner_name: "Alex",
  owner_status: "suspended",
  observed_state: "failed",
  desired_state: "running",
  worker_id: null,
  worker_name: null,
  players_online: 0,
  storage_bytes: 3 * 1024 * 1024,
  last_woken_at: ago(3_600_000),
  last_error: "worker refused the placement: out of memory",
  created_at: ago(86_400_000),
  updated_at: ago(60_000),
};
const onlineServer: AdminServer = {
  ...failedServer,
  id: "00000000-0000-4000-8000-000000000002",
  subdomain: "happy-world",
  owner_name: "Steve",
  owner_status: "active",
  observed_state: "online",
  worker_id: "00000000-0000-4000-8000-0000000000b1",
  worker_name: "worker-a",
  players_online: 1234,
  last_error: null,
};

function renderAdmin(ui: ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

beforeEach(() => {
  // No test may reach the network; every query used is mocked below.
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
    throw new Error(`unexpected request ${input instanceof Request ? input.url : String(input)}`);
  }));
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("AdminServersPage", () => {
  it("lists servers with owner, state, worker, players and the last error", async () => {
    vi.spyOn(browserApi, "getAdminServers").mockResolvedValue({ items: [failedServer, onlineServer], total: 2 });
    renderAdmin(<AdminServersPage initialFilter="all" />);

    expect(await screen.findByRole("heading", { name: "2 servers" })).toBeVisible();
    const rows = screen.getAllByRole("row");
    expect(within(rows[0]).getAllByRole("columnheader").map((cell) => cell.textContent)).toEqual([
      "Server", "Owner", "State", "Worker", "Players", "World", "Last woken", "Last error",
    ]);
    const failed = within(rows[1]);
    expect(failed.getByRole("link", { name: "broken-world" })).toHaveAttribute("href", `/manage/${failedServer.id}/lifecycle`);
    expect(failed.getByRole("link", { name: "Alex (suspended)" })).toHaveAttribute("href", `/admin/accounts/${failedServer.owner_id}`);
    expect(failed.getByText("Needs attention")).toBeVisible();
    expect(failed.getByText("worker refused the placement: out of memory", { selector: "summary" })).toBeVisible();
    expect(failed.getByText("3.0 MiB")).toBeVisible();
    const online = within(rows[2]);
    expect(online.getByText("worker-a")).toBeVisible();
    expect(online.getByText("1,234")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Next" })).not.toBeInTheDocument();
  });

  it("asks for the chosen filter, search and page, resetting the page on a new filter", async () => {
    const spy = vi.spyOn(browserApi, "getAdminServers").mockImplementation(async (query) => ({
      items: [query.filter === "failed" ? failedServer : onlineServer],
      total: 120,
    }));
    const user = userEvent.setup();
    renderAdmin(<AdminServersPage initialFilter="online" />);

    await screen.findByRole("heading", { name: "120 servers" });
    expect(spy).toHaveBeenLastCalledWith({ filter: "online", search: undefined, limit: 50, offset: 0 });
    expect(screen.getByRole("radio", { name: "Online" })).toHaveAttribute("aria-checked", "true");

    await user.click(screen.getByRole("button", { name: "Next" }));
    await waitFor(() => expect(spy).toHaveBeenLastCalledWith({ filter: "online", search: undefined, limit: 50, offset: 50 }));
    expect(await screen.findByText("51–100 of 120")).toBeVisible();

    await user.click(screen.getByRole("radio", { name: "Failed" }));
    await waitFor(() => expect(spy).toHaveBeenLastCalledWith({ filter: "failed", search: undefined, limit: 50, offset: 0 }));
    expect(screen.getByRole("radio", { name: "Failed" })).toHaveAttribute("aria-checked", "true");
    expect(await screen.findByRole("link", { name: "broken-world" })).toBeVisible();

    await user.type(screen.getByRole("searchbox", { name: "Search servers" }), "  broken ");
    await waitFor(() => expect(spy).toHaveBeenLastCalledWith({ filter: "failed", search: "broken", limit: 50, offset: 0 }));
  });

  it("says when nothing matches", async () => {
    vi.spyOn(browserApi, "getAdminServers").mockResolvedValue({ items: [], total: 0 });
    renderAdmin(<AdminServersPage initialFilter="stuck" />);
    expect(await screen.findByText("No server matches.")).toBeVisible();
    expect(screen.getByRole("heading", { name: "0 servers" })).toBeVisible();
  });

  it("shows a load failure with a retry that asks again", async () => {
    const spy = vi.spyOn(browserApi, "getAdminServers")
      .mockRejectedValueOnce(new ApiError(403, { error: { code: "forbidden", message: "Admins only." } } as never))
      .mockResolvedValue({ items: [onlineServer], total: 1 });
    const user = userEvent.setup();
    renderAdmin(<AdminServersPage initialFilter="all" />);
    expect(await screen.findByRole("alert")).toHaveTextContent("This could not be loaded");
    await user.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("heading", { name: "1 server" })).toBeVisible();
    expect(spy).toHaveBeenCalledTimes(2);
  });

  it("knows which filters exist", () => {
    expect(["all", "online", "sleeping", "queued", "failed", "stuck", "never_started"].every(isServerFilter)).toBe(true);
    expect(isServerFilter("deleted")).toBe(false);
    expect(isServerFilter(undefined)).toBe(false);
  });
});

const failures: AdminFailures = {
  groups: [
    {
      source: "operation",
      kind: "start",
      signature: "worker refused the placement: out of memory",
      message: "worker refused the placement: out of memory",
      count: 17,
      servers: 4,
      first_at: ago(5 * 3_600_000),
      last_at: ago(120_000),
      server_id: failedServer.id,
      subdomain: "broken-world",
    },
    {
      source: "plugin",
      kind: "load",
      signature: "missing export on-load",
      message: "missing export on-load",
      count: 1,
      servers: 1,
      first_at: ago(3_600_000),
      last_at: ago(3_600_000),
      server_id: onlineServer.id,
      subdomain: "happy-world",
    },
    {
      source: "github_webhook",
      kind: "delivery",
      signature: "signature mismatch",
      message: "signature mismatch",
      count: 2048,
      servers: 0,
      first_at: ago(3_600_000),
      last_at: ago(60_000),
      server_id: null,
      subdomain: null,
    },
  ],
  recent: [
    { at: ago(120_000), source: "operation", kind: "start", message: "worker refused the placement: out of memory", reference: "op-42", server_id: failedServer.id, subdomain: "broken-world" },
    { at: ago(200_000), source: "export", kind: "upload", message: "bucket unavailable", reference: "exports/abc.zip", server_id: failedServer.id, subdomain: null },
  ],
};

describe("AdminFailuresPage", () => {
  it("groups failures with their counts, sources and the page that explains them", async () => {
    const spy = vi.spyOn(browserApi, "getAdminFailures").mockResolvedValue(failures);
    renderAdmin(<AdminFailuresPage />);

    const grouped = (await screen.findByRole("heading", { name: "Grouped" })).closest("section")!;
    expect(spy).toHaveBeenCalledWith("day");
    const rows = within(grouped).getAllByRole("row").slice(1);
    expect(rows).toHaveLength(3);
    expect(within(rows[0]).getByText("Lifecycle")).toBeVisible();
    expect(within(rows[0]).getByText("17")).toBeVisible();
    expect(within(rows[0]).getByText("4")).toBeVisible();
    expect(within(rows[0]).getByRole("link", { name: "broken-world" })).toHaveAttribute("href", `/manage/${failedServer.id}/lifecycle`);
    expect(within(rows[1]).getByText("Plugin")).toBeVisible();
    expect(within(rows[1]).getByRole("link", { name: "happy-world" })).toHaveAttribute("href", `/manage/${onlineServer.id}/plugins`);
    expect(within(rows[2]).getByText("GitHub webhook")).toBeVisible();
    expect(within(rows[2]).getByText("2,048")).toBeVisible();
    expect(within(rows[2]).queryByRole("link")).not.toBeInTheDocument();

    const latest = screen.getByRole("heading", { name: "Latest" }).closest("section")!;
    expect(within(latest).getByText("op-42")).toBeVisible();
    expect(within(latest).getByText("World export")).toBeVisible();
    // A failure whose server is gone says so instead of linking nowhere.
    expect(within(latest).getByText("deleted server")).toBeVisible();
  });

  it("asks for another range when one is picked", async () => {
    const spy = vi.spyOn(browserApi, "getAdminFailures").mockResolvedValue(failures);
    const user = userEvent.setup();
    renderAdmin(<AdminFailuresPage />);
    await screen.findByRole("heading", { name: "Grouped" });
    expect(screen.getAllByRole("radio").map((radio) => radio.textContent)).toEqual(["Hour", "Day", "Week", "Month"]);
    await user.click(screen.getByRole("radio", { name: "Week" }));
    await waitFor(() => expect(spy).toHaveBeenLastCalledWith("week"));
  });

  it("says nothing failed when there is nothing", async () => {
    vi.spyOn(browserApi, "getAdminFailures").mockResolvedValue({ groups: [], recent: [] });
    renderAdmin(<AdminFailuresPage />);
    expect(await screen.findAllByText("Nothing failed.")).toHaveLength(2);
  });
});

function overview(overrides: Partial<AdminOverview> = {}): AdminOverview {
  return {
    measured_at: ago(0),
    accounts: { total: 1520, signups_last_day: 3, signups_last_week: 41, suspended: 2 },
    ai_cost_this_month_micros: 12_340_000,
    fleet: {
      servers_online: 12,
      players_online: 37,
      cpu_used_millis: 2500,
      cpu_total_millis: 8000,
      memory_used_bytes: 6 * 1024 ** 3,
      memory_total_bytes: 16 * 1024 ** 3,
      tps: 19.6,
    },
    github_rate_limits: [{ scope: "app", consecutive_failures: 0, remaining: 4000, reset_at: ago(-600_000), paused_until: null }],
    operations: { running: 1, retrying: 0, failed_last_day: 0 },
    servers: { total: 40, failed: 0, stuck: 0 },
    servers_by_state: [{ state: "online", count: 12 }, { state: "sleeping", count: 27 }, { state: "stopping", count: 1 }],
    start_queue: { depth: 0, oldest_requested_at: null },
    storage: {
      database_bytes: 512 * 1024 ** 2,
      server_bytes: 20 * 1024 ** 3,
      snapshot_bytes: 30 * 1024 ** 3,
      snapshots: 88,
      largest_tables: [{ name: "server_logs", rows: 1_200_000, bytes: 300 * 1024 ** 2 }],
      largest_servers: [{ server_id: onlineServer.id, subdomain: "happy-world", storage_bytes: 2 * 1024 ** 3 }],
    },
    tasks: [
      { name: "start_queue", required: true, state: "running", consecutive_failures: 0, last_success: ago(1000), last_heartbeat: ago(1000), last_error: null },
      { name: "maintenance", required: true, state: "running", consecutive_failures: 0, last_success: ago(1000), last_heartbeat: ago(1000), last_error: null },
    ],
    workers: { total: 3, live: 3, draining: 0 },
    ...overrides,
  };
}

/** A stat tile in the figures grid, not a chart that shares its title. */
function stat(label: string) {
  return screen.getByText(label, { selector: "span.block" }).parentElement!;
}

function healthCheck(label: string) {
  const health = screen.getByRole("heading", { name: "Health" }).closest("section")!;
  return within(health).getByText(label).closest("li")!;
}

describe("AdminOverviewPage", () => {
  beforeEach(() => {
    vi.spyOn(browserApi, "getAdminFleet").mockResolvedValue([]);
  });

  it("calls everything fine when it is, with the fleet figures", async () => {
    vi.spyOn(browserApi, "getAdminOverview").mockResolvedValue(overview());
    renderAdmin(<AdminOverviewPage />);
    await screen.findByRole("heading", { name: "Health" });

    for (const label of ["Background tasks", "Workers", "Start queue", "Failed servers", "Stuck servers", "Operations", "GitHub"]) {
      expect(healthCheck(label)).toHaveTextContent("Fine");
    }
    expect(healthCheck("Background tasks")).toHaveTextContent("2 of 2 running cleanly");
    expect(healthCheck("Workers")).toHaveTextContent("3 of 3 live");
    expect(healthCheck("Start queue")).toHaveTextContent("empty");
    expect(healthCheck("GitHub")).toHaveTextContent("within its rate limit");
    expect(screen.queryByRole("columnheader", { name: "Task" })).not.toBeInTheDocument();

    expect(stat("CPU in use")).toHaveTextContent("31%2.5 cores of 8 cores");
    expect(stat("Memory in use")).toHaveTextContent("38%6.00 GiB of 16.00 GiB");
    expect(stat("Average TPS")).toHaveTextContent("19.6");
    expect(screen.getByText("Accounts").closest("a")).toHaveTextContent("1,5203 today, 41 this week, 2 suspended");
    expect(screen.getByText("AI this month").closest("a")).toHaveTextContent("$12.34");
    expect(screen.getByRole("link", { name: "12 online" })).toHaveAttribute("href", "/admin/servers?filter=online");
    expect(screen.getByRole("link", { name: "1 stopping" })).toHaveAttribute("href", "/admin/servers?filter=all");
    expect(screen.getByText("server_logs")).toBeVisible();
    // An empty fleet history leaves every chart empty rather than broken.
    expect(browserApi.getAdminFleet).toHaveBeenCalledWith("day");
    expect((await screen.findAllByText("No samples in this range.")).length).toBeGreaterThan(0);
  });

  it("marks what needs action as Broken and what needs a look as Look", async () => {
    vi.spyOn(browserApi, "getAdminOverview").mockResolvedValue(overview({
      tasks: [
        { name: "start_queue", required: true, state: "fatal", consecutive_failures: 5, last_success: ago(3_600_000), last_heartbeat: null, last_error: "database is gone" },
        { name: "ci_poller", required: false, state: "backing_off", consecutive_failures: 2, last_success: null, last_heartbeat: null, last_error: null },
        { name: "maintenance", required: true, state: "running", consecutive_failures: 0, last_success: ago(1000), last_heartbeat: ago(1000), last_error: null },
      ],
      workers: { total: 3, live: 2, draining: 1 },
      start_queue: { depth: 4, oldest_requested_at: ago(300_000) },
      servers: { total: 40, failed: 2, stuck: 1 },
      operations: { running: 3, retrying: 2, failed_last_day: 6 },
      github_rate_limits: [{ scope: "app", consecutive_failures: 1, remaining: 0, reset_at: null, paused_until: ago(-900_000) }],
      fleet: { servers_online: 0, players_online: 0, cpu_used_millis: null, cpu_total_millis: null, memory_used_bytes: null, memory_total_bytes: null, tps: null },
    }));
    renderAdmin(<AdminOverviewPage />);
    await screen.findByRole("heading", { name: "Health" });

    expect(healthCheck("Background tasks")).toHaveTextContent("Broken");
    expect(healthCheck("Background tasks")).toHaveTextContent("1 of 3 running cleanly");
    expect(healthCheck("Workers")).toHaveTextContent("Look");
    expect(healthCheck("Workers")).toHaveTextContent("1 draining");
    expect(healthCheck("Start queue")).toHaveTextContent("Look");
    expect(healthCheck("Start queue")).toHaveTextContent("4 waiting");
    expect(healthCheck("Start queue")).toHaveTextContent("oldest asked");
    expect(healthCheck("Failed servers")).toHaveTextContent("Broken");
    expect(within(healthCheck("Failed servers")).getByRole("link")).toHaveAttribute("href", "/admin/servers?filter=failed");
    expect(healthCheck("Stuck servers")).toHaveTextContent("Look");
    expect(healthCheck("Operations")).toHaveTextContent("Look");
    expect(healthCheck("Operations")).toHaveTextContent("3 running, 2 retrying");
    expect(healthCheck("GitHub")).toHaveTextContent("Look");
    expect(healthCheck("GitHub")).toHaveTextContent("paused by the rate limit");

    const tasks = screen.getByRole("columnheader", { name: "Task" }).closest("table")!;
    const rows = within(tasks).getAllByRole("row").slice(1);
    expect(rows.map((row) => within(row).getAllByRole("cell")[0].textContent)).toEqual(["start_queue", "ci_poller (optional)"]);
    expect(rows[0]).toHaveTextContent("fatal");
    expect(rows[1]).toHaveTextContent("backing off");
    expect(within(rows[0]).getByText("database is gone", { selector: "summary" })).toBeVisible();

    expect(stat("CPU in use")).toHaveTextContent("—no fresh samples");
    expect(stat("Average TPS")).toHaveTextContent("—");
  });

  it("calls a fleet without live workers Broken", async () => {
    vi.spyOn(browserApi, "getAdminOverview").mockResolvedValue(overview({ workers: { total: 2, live: 0, draining: 0 }, github_rate_limits: [] }));
    renderAdmin(<AdminOverviewPage />);
    await screen.findByRole("heading", { name: "Health" });
    expect(healthCheck("Workers")).toHaveTextContent("Broken");
    expect(healthCheck("GitHub")).toHaveTextContent("not polled");
  });
});
