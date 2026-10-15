import * as Tooltip from "@radix-ui/react-tooltip";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ClaimServer } from "../../components/claim-server";
import { keepChosenName } from "../../lib/chosen-name";

const replace = vi.fn();
vi.mock("next/navigation", () => ({ useRouter: () => ({ replace }) }));

const emptyAccount = { identities: [], sessions: [], worlds: [] };

/** Answers the account poll with `account()` and name lookups from `taken`. */
function stubApi(account: () => unknown, taken: Set<string>) {
  const lookups: string[] = [];
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = input instanceof Request ? input : new Request(new URL(String(input), "http://localhost"), init);
    const path = new URL(request.url).pathname;
    if (path === "/api/v1/account") return Response.json(account());
    const name = path.match(/^\/api\/v1\/server-addresses\/(.+)$/)?.[1];
    if (name) {
      lookups.push(name);
      return Response.json({ subdomain: name, address: `${name}.mc.example.test`, available: !taken.has(name) });
    }
    return Response.json({ code: "not_found", message: "not found" }, { status: 404 });
  }));
  return lookups;
}

function renderClaim() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}><Tooltip.Provider>{children}</Tooltip.Provider></QueryClientProvider>;
  }
  render(<ClaimServer serverDomain="mc.example.test" />, { wrapper: Wrapper });
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  replace.mockReset();
  localStorage.clear();
});

describe("claiming a server name", () => {
  it("says a name is claimed by joining it and only looks names up", async () => {
    const lookups = stubApi(() => emptyAccount, new Set(["taken-world"]));
    renderClaim();
    const user = userEvent.setup();
    expect(screen.getByLabelText("Server address")).toHaveTextContent("your-name.mc.example.test");
    expect(screen.queryByRole("button", { name: /create/i })).toBeNull();

    const field = screen.getByRole("textbox", { name: /server name/i });
    await user.type(field, "Taken-World!");
    expect(field).toHaveValue("taken-world");
    expect(await screen.findByText(/taken-world\.mc\.example\.test is taken/)).toBeInTheDocument();

    await user.clear(field);
    await user.type(field, "pumpkin-patch");
    expect(screen.getByLabelText("Server address")).toHaveTextContent("pumpkin-patch.mc.example.test");
    expect(
      await screen.findByText("pumpkin-patch.mc.example.test is free. Join it in Minecraft to claim it."),
    ).toBeInTheDocument();
    expect(screen.getByText("Join pumpkin-patch.mc.example.test from Minecraft Java or Bedrock.")).toBeInTheDocument();
    expect(lookups).toEqual(["taken-world", "pumpkin-patch"]);

    await user.clear(field);
    await user.type(field, "-x");
    expect(await screen.findByText(/at least 3 characters/)).toBeInTheDocument();
  });

  it("opens the panel once the join has linked a server to this account", async () => {
    let account: unknown = emptyAccount;
    stubApi(() => account, new Set());
    renderClaim();
    await waitFor(() => expect(fetch).toHaveBeenCalled());
    expect(replace).not.toHaveBeenCalled();
    account = {
      ...emptyAccount,
      worlds: [{ id: "00000000-0000-4000-8000-000000000001", subdomain: "pumpkin-patch", observed_state: "starting", is_default: true, created_at: "2026-10-15T17:00:00Z" }],
    };
    await waitFor(
      () => expect(replace).toHaveBeenCalledWith("/manage/00000000-0000-4000-8000-000000000001"),
      { timeout: 6000 },
    );
  }, 10_000);
});

describe("a name picked on the landing page", () => {
  it("is in the name field when the claim page opens, and is looked up", async () => {
    keepChosenName("pumpkin-patch");
    const lookups = stubApi(() => emptyAccount, new Set());
    renderClaim();
    expect(screen.getByRole("textbox", { name: /server name/i })).toHaveValue("pumpkin-patch");
    expect(
      await screen.findByText("pumpkin-patch.mc.example.test is free. Join it in Minecraft to claim it."),
    ).toBeInTheDocument();
    expect(lookups).toEqual(["pumpkin-patch"]);
  });

  it("is left out when it is not a valid name", () => {
    localStorage.setItem("kurbis.name", "-bad-");
    stubApi(() => emptyAccount, new Set());
    renderClaim();
    expect(screen.getByRole("textbox", { name: /server name/i })).toHaveValue("");
  });
});
