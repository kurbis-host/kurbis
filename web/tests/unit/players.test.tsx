import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Players } from "../../components/players";
import type { PlayerPresence, WhitelistState } from "../../lib/contracts";
import { queryKeys } from "../../lib/query";
import { server } from "./fixtures";

const player: PlayerPresence = {
  player_id: "00000000-0000-0000-0000-000000000001",
  name: "Alex",
  edition: "java",
  connected_at: "2026-09-30T18:20:00Z",
  last_seen_at: "2026-09-30T18:20:01Z",
};
const emptyWhitelist: WhitelistState = {
  enabled: true,
  revision: 0,
  applied_revision: 0,
  synchronized: true,
  entries: [],
};

function wrapper(client: QueryClient) {
  return function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  };
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("whitelist interactions", () => {
  it("whitelists a live player using the persisted game-profile UUID and removes them", async () => {
    let present = false;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      const method = init?.method ?? "GET";
      if (path === "/api/v1/server/whitelist" && method === "POST") {
        expect(JSON.parse(String(init?.body))).toEqual({ player_id: player.player_id, name: "Alex" });
        present = true;
        return new Response("{}", { status: 201, headers: { "content-type": "application/json" } });
      }
      if (path.startsWith("/api/v1/server/whitelist?player_id=") && method === "DELETE") {
        expect(path).toContain(player.player_id);
        present = false;
        return new Response("{}", { status: 200, headers: { "content-type": "application/json" } });
      }
      if (path === "/api/v1/server/whitelist") {
        const data: WhitelistState = present
          ? {
              enabled: true,
              revision: 1,
              applied_revision: 0,
              synchronized: false,
              entries: [{
                player_id: player.player_id,
                name: player.name,
                created_at: "2026-09-30T18:21:00Z",
                updated_at: "2026-09-30T18:21:00Z",
              }],
            }
          : emptyWhitelist;
        return Response.json(data);
      }
      throw new Error(`unexpected request ${method} ${path}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const client = new QueryClient({
      defaultOptions: { queries: { staleTime: Infinity, retry: false }, mutations: { retry: false } },
    });
    const online = {
      ...server,
      observed_state: "online" as const,
      desired_state: "running" as const,
      settings: { ...server.settings, whitelist: true },
    };
    client.setQueryData(queryKeys.server, online);
    client.setQueryData(queryKeys.players, [player]);
    client.setQueryData(queryKeys.whitelist, emptyWhitelist);
    render(
      <Players initialServer={online} initialPlayers={[player]} initialWhitelist={emptyWhitelist} />,
      { wrapper: wrapper(client) },
    );
    const user = userEvent.setup();

    await user.click(screen.getByRole("button", { name: "Allow java" }));
    await waitFor(() => expect(screen.getByText("Allowed")).toBeVisible());
    await user.click(screen.getByRole("button", { name: "Remove Alex from whitelist" }));
    await waitFor(() => expect(screen.getByText("Whitelist is empty")).toBeVisible());
  });

  it("shows API failures and validates manual UUID input", async () => {
    cleanup();
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json({ message: "player name is already whitelisted" }, { status: 409 }),
      ),
    );
    const client = new QueryClient({
      defaultOptions: { queries: { staleTime: Infinity, retry: false }, mutations: { retry: false } },
    });
    client.setQueryData(queryKeys.server, server);
    client.setQueryData(queryKeys.players, [player]);
    client.setQueryData(queryKeys.whitelist, emptyWhitelist);
    render(
      <Players initialServer={server} initialPlayers={[player]} initialWhitelist={emptyWhitelist} />,
      { wrapper: wrapper(client) },
    );
    const user = userEvent.setup();
    await user.type(screen.getByRole("textbox", { name: "Whitelist player name" }), "Manual Alex");
    await user.type(screen.getByPlaceholderText("00000000-0000-0000-0000-000000000000"), "not-a-uuid");
    await user.click(screen.getByRole("button", { name: "Add player" }));
    expect(screen.getByText("Enter a valid Minecraft player UUID.")).toBeVisible();
    expect(fetch).not.toHaveBeenCalled();

    await user.clear(screen.getByPlaceholderText("00000000-0000-0000-0000-000000000000"));
    await user.type(
      screen.getByPlaceholderText("00000000-0000-0000-0000-000000000000"),
      "00000000-0000-4000-8000-000000000002",
    );
    await user.click(screen.getByRole("button", { name: "Add player" }));
    await waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(screen.getByText("player name is already whitelisted")).toBeVisible(),
    );
  });
});
