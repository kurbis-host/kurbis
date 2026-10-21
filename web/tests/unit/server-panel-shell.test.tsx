import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ServerPanelShell } from "../../components/server-panel-shell";
import { server, session } from "./fixtures";

const navigation = vi.hoisted(() => ({
  pathname: "/manage/server-1/players",
  push: vi.fn(),
}));

vi.mock("next/navigation", () => ({
  usePathname: () => navigation.pathname,
  useRouter: () => ({ push: navigation.push }),
}));

function renderShell({ scoped }: { scoped: boolean }) {
  const second = { ...server, id: "server-2", subdomain: "shared-world", is_default: false };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  return render(
    <QueryClientProvider client={client}>
      <ServerPanelShell
        scoped={scoped}
        server={server}
        servers={[server, second]}
        serverSessions={[{ id: "grant-2", server_id: second.id, created_at: "2026-10-01T00:00:00Z" }]}
      >
        <p>Server panel content</p>
      </ServerPanelShell>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  navigation.pathname = "/manage/server-1/players";
  navigation.push.mockReset();
});

describe("shared server navigation", () => {
  it("lets a scoped session switch between its accessible grants while staying on the current view", () => {
    renderShell({ scoped: true });

    const selector = screen.getByRole("combobox", { name: "Select server" });
    expect(selector.querySelectorAll("option")).toHaveLength(2);
    expect(selector.querySelector("option[value='server-2']")).toHaveTextContent("shared");
    expect(screen.queryByRole("link", { name: "Account" })).not.toBeInTheDocument();

    fireEvent.change(selector, { target: { value: "server-2" } });
    expect(navigation.push).toHaveBeenCalledWith("/manage/server-2/players");
  });

  it("keeps account navigation and the account server-list entry available", () => {
    renderShell({ scoped: false });

    expect(screen.getByRole("link", { name: "Account" })).toBeVisible();
    expect(screen.getByRole("link", { name: "kurbis" })).toHaveAttribute("href", "/app/servers");
  });

  it("links every server page, with the AI assistant before settings", () => {
    renderShell({ scoped: false });

    const navigation = screen.getByRole("navigation", { name: "Server panel" });
    expect(within(navigation).getAllByRole("link").map((link) => link.textContent)).toEqual([
      "Overview", "Console", "Backups", "Players", "Plugins", "AI", "Settings", "Account",
    ]);
    expect(within(navigation).getByRole("link", { name: "Plugins" })).toHaveAttribute("href", "/manage/server-1/plugins");
    expect(within(navigation).getByRole("link", { name: "AI" })).toHaveAttribute("href", "/manage/server-1/ai");
  });
});

describe("admin entry points in the server panel", () => {
  function renderWith({ isAdmin, adminView = false }: { isAdmin: boolean; adminView?: boolean }) {
    const second = { ...server, id: "server-2", subdomain: "shared-world", is_default: false };
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    return render(
      <QueryClientProvider client={client}>
        <ServerPanelShell
          server={server}
          servers={[server, second]}
          serverSessions={[]}
          session={{ ...session, is_admin: isAdmin }}
          adminView={adminView}
        >
          <p>Server panel content</p>
        </ServerPanelShell>
      </QueryClientProvider>,
    );
  }

  it("shows neither the Lifecycle tab nor the Admin link to a regular account", () => {
    renderWith({ isAdmin: false });
    const nav = screen.getByRole("navigation", { name: "Server panel" });
    expect(within(nav).queryByRole("link", { name: "Lifecycle" })).not.toBeInTheDocument();
    expect(within(nav).queryByRole("link", { name: "Admin" })).not.toBeInTheDocument();
  });

  it("adds the Lifecycle tab after Settings and the Admin link last for an admin", () => {
    renderWith({ isAdmin: true });
    const nav = screen.getByRole("navigation", { name: "Server panel" });
    expect(within(nav).getAllByRole("link").map((link) => link.textContent)).toEqual([
      "Overview", "Console", "Backups", "Players", "Plugins", "AI", "Settings", "Lifecycle", "Account", "Admin",
    ]);
    expect(within(nav).getByRole("link", { name: "Lifecycle" })).toHaveAttribute("href", "/manage/server-1/lifecycle");
    expect(within(nav).getByRole("link", { name: "Admin" })).toHaveAttribute("href", "/admin");
  });

  it("keeps the server switcher and no read-only note outside the admin view", () => {
    renderWith({ isAdmin: true });
    expect(screen.getByRole("combobox", { name: "Select server" })).toBeVisible();
    expect(screen.queryByText(/Admin view, read-only/)).not.toBeInTheDocument();
  });

  it("replaces the server switcher with the read-only note in the admin view", () => {
    renderWith({ isAdmin: true, adminView: true });
    expect(screen.getByText("Admin view, read-only. Nothing here can be changed.")).toBeVisible();
    expect(screen.queryByRole("combobox", { name: "Select server" })).not.toBeInTheDocument();
    expect(screen.getByText("Server panel content")).toBeVisible();
  });
});
