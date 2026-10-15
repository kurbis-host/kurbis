import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Players } from "../../components/players";
import type { components } from "../../lib/api/generated";
import { queryKeys } from "../../lib/query";
import type { OperatorEntry, OperatorRoster } from "../../lib/player-roster";
import { server } from "./fixtures";

type PlayerPresence = components["schemas"]["PlayerPresence"];
type WhitelistState = components["schemas"]["Whitelist"];
const player: PlayerPresence = {
  player_id: "00000000-0000-0000-0000-000000000001",
  name: "Alex",
  edition: "java",
  connected_at: "2026-09-30T18:20:00Z",
  last_seen_at: "2026-09-30T18:20:01Z",
};
const emptyWhitelist: WhitelistState = {
  enabled: true,
  whitelist_by_members: true,
  owner_has_minecraft_account: true,
  requests: [],
  revision: 0,
  applied_revision: 0,
  synchronized: true,
  entries: [],
};
const emptyOperators: OperatorRoster = {
  revision: 1,
  applied_revision: 1,
  synchronized: true,
  entries: [],
};
const createdAt = "2026-09-30T18:21:00Z";

function wrapper(client: QueryClient) {
  return function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  };
}

function setup(initial: {
  players?: PlayerPresence[];
  whitelist?: WhitelistState;
  operators?: OperatorRoster;
} = {}) {
  const client = new QueryClient({
    defaultOptions: { queries: { staleTime: Infinity, retry: false }, mutations: { retry: false } },
  });
  const onlineServer = {
    ...server,
    observed_state: "online" as const,
    desired_state: "running" as const,
    settings: { ...server.settings, whitelist: true },
  };
  const players = initial.players ?? [player];
  const whitelist = initial.whitelist ?? emptyWhitelist;
  const operators = initial.operators ?? emptyOperators;
  client.setQueryData(queryKeys.server(server.id), onlineServer);
  client.setQueryData(queryKeys.players(server.id), players);
  client.setQueryData(queryKeys.whitelist(server.id), whitelist);
  client.setQueryData(queryKeys.operators(server.id), operators);
  render(
    <Players initialServer={onlineServer} initialPlayers={players} initialWhitelist={whitelist} />,
    { wrapper: wrapper(client) },
  );
  return client;
}

type MockRequest = { url: string; method: string; body: unknown };
async function requestFrom(input: RequestInfo | URL, init?: RequestInit): Promise<MockRequest> {
  const request = input instanceof Request ? input : undefined;
  const url = request?.url ?? String(input);
  const method = request?.method ?? init?.method ?? "GET";
  const rawBody = request ? await request.clone().json().catch(() => undefined) : init?.body;
  const body = typeof rawBody === "string" ? JSON.parse(rawBody) : rawBody;
  return { url, method, body };
}

function whitelistResponse(entries: WhitelistState["entries"]): WhitelistState {
  return {
    ...emptyWhitelist,
    revision: entries.length,
    entries,
  };
}
function operatorsResponse(entries: OperatorRoster["entries"]): OperatorRoster {
  return {
    revision: entries.length + 1,
    applied_revision: entries.length + 1,
    synchronized: true,
    entries,
  };
}

function whitelistEntry(id: string, name: string, extra: Partial<WhitelistState["entries"][number]> = {}) {
  return { player_id: id, name, added_by: null, added_in_game: false, created_at: createdAt, updated_at: createdAt, ...extra };
}
function operatorEntry(id: string, name: string, extra: Partial<OperatorRoster["entries"][number]> = {}) {
  return { player_id: id, name, created_at: createdAt, updated_at: createdAt, is_owner: false, ...extra };
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("player and access management", () => {
  it("uses one-click name-only actions for online players and updates both access lists", async () => {
    let whitelist: WhitelistState = emptyWhitelist;
    let operators: OperatorRoster = emptyOperators;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = await requestFrom(input, init);
      const path = new URL(request.url, "http://localhost").pathname;
      if (path.endsWith("/whitelist") && request.method === "POST") {
        expect(request.body).toEqual({ name: "Alex" });
        whitelist = whitelistResponse([whitelistEntry(player.player_id, "Alex")]);
        return Response.json(whitelist, { status: 201 });
      }
      if (path.endsWith("/whitelist") && request.method === "GET") return Response.json(whitelist);
      if (path.endsWith("/operators") && request.method === "POST") {
        expect(request.body).toEqual({ name: "Alex" });
        operators = operatorsResponse([operatorEntry(player.player_id, "Alex")]);
        return Response.json(operators, { status: 201 });
      }
      if (path.endsWith("/operators") && request.method === "GET") return Response.json(operators);
      throw new Error(`unexpected request ${request.method} ${path}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    setup();
    const user = userEvent.setup();

    await user.click(screen.getByRole("button", { name: "Allow" }));
    await waitFor(() => expect(screen.getByText("Allowed")).toBeVisible());
    await user.click(screen.getByRole("button", { name: "Grant OP" }));
    await waitFor(() => expect(screen.getByText("Operator", { selector: "span" })).toBeVisible());
    expect(await Promise.all(fetchMock.mock.calls.map(async ([input, init]) => (await requestFrom(input, init)).body)).then((bodies) => bodies.filter(Boolean))).toEqual([
      { name: "Alex" },
      { name: "Alex" },
    ]);
  });

  it("supports offline name-only additions to both lists", async () => {
    let whitelist: WhitelistState = emptyWhitelist;
    let operators: OperatorRoster = emptyOperators;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = await requestFrom(input, init);
      const path = new URL(request.url, "http://localhost").pathname;
      if (path.endsWith("/whitelist") && request.method === "POST") {
        expect(request.body).toEqual({ name: "OfflineJava" });
        whitelist = whitelistResponse([whitelistEntry("8667ba71-b85a-4004-af54-457a9734eed7", "OfflineJava")]);
        return Response.json(whitelist, { status: 201 });
      }
      if (path.endsWith("/whitelist") && request.method === "GET") return Response.json(whitelist);
      if (path.endsWith("/operators") && request.method === "POST") {
        expect(request.body).toEqual({ name: "OfflineJava" });
        operators = operatorsResponse([operatorEntry("8667ba71-b85a-4004-af54-457a9734eed7", "OfflineJava")]);
        return Response.json(operators, { status: 201 });
      }
      if (path.endsWith("/operators") && request.method === "GET") return Response.json(operators);
      throw new Error(`unexpected request ${request.method} ${path}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    setup({ players: [] });
    const user = userEvent.setup();

    await user.type(screen.getByRole("textbox", { name: /Add by player name/ }), "OfflineJava");
    await user.click(screen.getByRole("button", { name: "Add to whitelist" }));
    await waitFor(() => expect(screen.getByText("OfflineJava")).toBeVisible());
    await user.type(screen.getByRole("textbox", { name: /Add trusted player by name/ }), "OfflineJava");
    await user.click(screen.getByRole("button", { name: "Grant operator" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Remove OfflineJava as operator" })).toBeVisible());
  });

  it("keeps owner operator access visibly locked and non-removable", () => {
    setup({ operators: operatorsResponse([operatorEntry(player.player_id, "Alex", { is_owner: true })]) });
    expect(screen.getByText("Owner", { selector: "span" })).toBeVisible();
    expect(screen.getByText("Locked")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Remove Alex as operator" })).not.toBeInTheDocument();
  });

  it("lists join requests and lets the owner answer them", async () => {
    const steve = "00000000-0000-4000-8000-000000000201";
    const recently = new Date(Date.now() - 3 * 60_000).toISOString();
    const pending: WhitelistState = {
      ...emptyWhitelist,
      owner_has_minecraft_account: false,
      requests: [{ player_id: steve, name: "Steve", attempts: 2, first_attempt_at: recently, last_attempt_at: recently }],
    };
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = await requestFrom(input, init);
      const url = new URL(request.url, "http://localhost");
      if (url.pathname.endsWith(`/whitelist/requests/${steve}/let-in`) && request.method === "POST") {
        return Response.json(whitelistResponse([whitelistEntry(steve, "Steve", { added_by: "Owner" })]), { status: 201 });
      }
      if (url.pathname.endsWith("/whitelist") && request.method === "GET") {
        return Response.json(whitelistResponse([whitelistEntry(steve, "Steve", { added_by: "Owner" })]));
      }
      throw new Error(`unexpected request ${request.method} ${url.pathname}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    setup({ whitelist: pending });
    const user = userEvent.setup();

    expect(screen.getByRole("heading", { name: "Asked to join" })).toBeVisible();
    expect(screen.getByText(/Tried to join 3 minutes ago · 2 tries/)).toBeVisible();
    expect(screen.getByRole("button", { name: /That's me/ })).toBeVisible();
    expect(screen.getByRole("button", { name: "Dismiss Steve's request" })).toBeVisible();

    await user.click(screen.getByRole("button", { name: /Let in/ }));
    await waitFor(() => expect(screen.queryByRole("heading", { name: "Asked to join" })).not.toBeInTheDocument());
    expect(screen.getByText("Saved access · added by Owner")).toBeVisible();
  });

  it("offers That's me only while the owner has no linked Minecraft account", () => {
    const recently = new Date().toISOString();
    setup({
      whitelist: {
        ...emptyWhitelist,
        requests: [{ player_id: "00000000-0000-4000-8000-000000000202", name: "Alex2", attempts: 1, first_attempt_at: recently, last_attempt_at: recently }],
      },
    });
    expect(screen.getByRole("button", { name: /Let in/ })).toBeVisible();
    expect(screen.queryByRole("button", { name: /That's me/ })).not.toBeInTheDocument();
  });

  it("says who added a whitelist entry and whether it was in game", () => {
    setup({
      whitelist: whitelistResponse([
        whitelistEntry("00000000-0000-4000-8000-000000000101", "Steve", { added_by: "Alex", added_in_game: true }),
        whitelistEntry("00000000-0000-4000-8000-000000000102", "Notch", { added_by: "Owner" }),
        whitelistEntry("00000000-0000-4000-8000-000000000103", "Herobrine"),
      ]),
    });
    expect(screen.getByText("Saved access · added by Alex in game")).toBeVisible();
    expect(screen.getByText("Saved access · added by Owner")).toBeVisible();
    expect(screen.getAllByText("Saved access", { exact: true }).length).toBeGreaterThan(0);
  });

  it("removes unprotected whitelist and operator entries by player name", async () => {
    const whitelist: WhitelistState = whitelistResponse([whitelistEntry(player.player_id, "Alex")]);
    const operators = operatorsResponse([operatorEntry(player.player_id, "Alex")]);
    let currentWhitelist = whitelist;
    let currentOperators = operators;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = await requestFrom(input, init);
      const url = new URL(request.url, "http://localhost");
      if (url.pathname.endsWith("/whitelist") && request.method === "DELETE") {
        expect(url.searchParams.get("player_id")).toBe(player.player_id);
        currentWhitelist = emptyWhitelist;
        return Response.json(currentWhitelist);
      }
      if (url.pathname.endsWith("/whitelist") && request.method === "GET") return Response.json(currentWhitelist);
      if (url.pathname.endsWith("/operators") && request.method === "DELETE") {
        expect(url.searchParams.get("player_id")).toBe(player.player_id);
        currentOperators = emptyOperators;
        return Response.json(currentOperators);
      }
      if (url.pathname.endsWith("/operators") && request.method === "GET") return Response.json(currentOperators);
      throw new Error(`unexpected request ${request.method} ${url.pathname}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    setup({ whitelist, operators });
    const user = userEvent.setup();

    await user.click(screen.getByRole("button", { name: "Remove Alex from whitelist" }));
    await waitFor(() => expect(screen.getByText("Whitelist is empty")).toBeVisible());
    await user.click(screen.getByRole("button", { name: "Remove Alex as operator" }));
    await waitFor(() => expect(screen.getByText("No extra operators")).toBeVisible());
  });

  it("surfaces a mutation failure and keeps the submitted name for a retry", async () => {
    const fetchMock = vi.fn(async () => Response.json({ code: "minecraft_player_not_found", message: "no Java Minecraft profile exists for that player name" }, { status: 422 }));
    vi.stubGlobal("fetch", fetchMock);
    setup({ players: [] });
    const user = userEvent.setup();
    const input = screen.getByRole("textbox", { name: /Add by player name/ });
    await user.type(input, "UnknownJava");
    await user.click(screen.getByRole("button", { name: "Add to whitelist" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("no Java Minecraft profile exists for that player name"));
    expect(input).toHaveValue("UnknownJava");
  });

  it("shows pending status and disables duplicate clicks until the request settles", async () => {
    let resolvePost!: (response: Response) => void;
    const postResponse = new Promise<Response>((resolve) => { resolvePost = resolve; });
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = await requestFrom(input, init);
      if (new URL(request.url, "http://localhost").pathname.endsWith("/whitelist") && request.method === "POST") return postResponse;
      throw new Error(`unexpected request ${request.method} ${request.url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    setup();
    const user = userEvent.setup();
    const allow = screen.getByRole("button", { name: "Allow" });
    await user.click(allow);
    expect(screen.getByRole("button", { name: "Adding…" })).toBeDisabled();
    resolvePost(Response.json(whitelistResponse([whitelistEntry(player.player_id, "Alex")]), { status: 201 }));
    await waitFor(() => expect(screen.getByText("Allowed")).toBeVisible());
  });
});
