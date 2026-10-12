import { useQuery } from "@tanstack/react-query";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AppProviders } from "../../components/providers";
import type { components } from "../../lib/api/generated";
import { queryKeys } from "../../lib/query";
import { aiOverview, server, session } from "./fixtures";

type Server = components["schemas"]["Server"];

const navigation = vi.hoisted(() => ({ pathname: "/manage/server-1" }));
vi.mock("next/navigation", () => ({ usePathname: () => navigation.pathname }));

class EventSourceStub {
  static instance: EventSourceStub | null = null;
  static url: string | null = null;
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  readonly close = vi.fn();
  private readonly listeners = new Map<string, EventListener>();

  addEventListener(type: string, listener: EventListenerOrEventListenerObject) {
    const callback = typeof listener === "function"
      ? listener
      : listener.handleEvent.bind(listener);
    this.listeners.set(type, callback);
  }

  emit(type: string, event: MessageEvent<string>) {
    this.listeners.get(type)?.(event);
  }

  constructor(url?: string) {
    EventSourceStub.instance = this;
    EventSourceStub.url = url ?? null;
  }
}

function EventStatus({ serverId = server.id }: { serverId?: string }) {
  const { data } = useQuery({
    queryKey: queryKeys.eventStatus(serverId),
    queryFn: async () => "unknown",
    staleTime: Infinity,
  });
  return <span>{String(data)}</span>;
}

function ServerState() {
  const { data } = useQuery<Server | null>({
    queryKey: queryKeys.server(server.id),
    queryFn: async () => server,
  });
  return <span>{data?.observed_state}</span>;
}

afterEach(() => {
  cleanup();
  navigation.pathname = `/manage/${server.id}`;
  vi.unstubAllGlobals();
  EventSourceStub.instance = null;
  EventSourceStub.url = null;
});

describe("event reconnect state", () => {
  it("keeps the default server's event stream connected on account routes", async () => {
    navigation.pathname = "/app/account";
    vi.stubGlobal("EventSource", EventSourceStub);
    render(
      <AppProviders session={session} servers={[server]}>
        <EventStatus />
      </AppProviders>,
    );

    await waitFor(() => expect(screen.getByText("connecting")).toBeVisible());
    expect(EventSourceStub.instance).not.toBeNull();
  });

  it("uses the server named by the shared /manage route even when it is not default", async () => {
    const other = { ...server, id: "server-2", subdomain: "other-world", is_default: false };
    navigation.pathname = `/manage/${other.id}/players`;
    vi.stubGlobal("EventSource", EventSourceStub);
    render(
      <AppProviders session={session} servers={[server, other]}>
        <EventStatus serverId={other.id} />
      </AppProviders>,
    );

    await waitFor(() => expect(screen.getByText("connecting")).toBeVisible());
    expect(EventSourceStub.url).toBe(`/api/v1/servers/${other.id}/events`);
  });

  it("moves from connecting to live and reconnecting as EventSource changes state", async () => {
    vi.stubGlobal("EventSource", EventSourceStub);
    render(
      <AppProviders session={session} servers={[server]}>
        <EventStatus />
      </AppProviders>,
    );
    await waitFor(() => expect(screen.getByText("connecting")).toBeVisible());
    expect(EventSourceStub.instance).not.toBeNull();

    act(() => EventSourceStub.instance?.onopen?.(new Event("open")));
    await waitFor(() => expect(screen.getByText("live")).toBeVisible());
    act(() => EventSourceStub.instance?.onerror?.(new Event("error")));
    await waitFor(() => expect(screen.getByText("reconnecting")).toBeVisible());
  });

  it("subscribes to operator events and updates the managed roster cache", async () => {
    vi.stubGlobal("EventSource", EventSourceStub);
    function OperatorCount() {
      const { data } = useQuery({
        queryKey: queryKeys.operators(server.id),
        queryFn: async () => ({ revision: 1, applied_revision: 1, synchronized: true, entries: [] }),
        staleTime: Infinity,
      });
      return <span>{data?.entries.length ?? 0} operators</span>;
    }
    render(
      <AppProviders session={session} servers={[server]}>
        <OperatorCount />
      </AppProviders>,
    );
    await waitFor(() => expect(screen.getByText("0 operators")).toBeVisible());

    const roster = {
      revision: 2,
      applied_revision: 2,
      synchronized: true,
      entries: [{
        player_id: "00000000-0000-0000-0000-000000000004",
        name: "Operator",
        created_at: "2026-10-03T12:00:00Z",
        updated_at: "2026-10-03T12:00:00Z",
        is_owner: false,
      }],
    };
    act(() => EventSourceStub.instance?.emit(
      "operators",
      new MessageEvent("operators", {
        data: JSON.stringify({ id: 22, type: "operators", data: roster }),
        lastEventId: "22",
      }),
    ));
    await waitFor(() => expect(screen.getByText("1 operators")).toBeVisible());
  });

  it("subscribes to AI events and replaces the overview cache", async () => {
    vi.stubGlobal("EventSource", EventSourceStub);
    function AiSpend() {
      const { data } = useQuery({
        queryKey: queryKeys.ai(server.id),
        queryFn: async () => aiOverview,
        staleTime: Infinity,
      });
      return <span>{data?.usage.server.spent_micros} spent</span>;
    }
    render(
      <AppProviders session={session} servers={[server]}>
        <AiSpend />
      </AppProviders>,
    );
    await waitFor(() => expect(screen.getByText("1250000 spent")).toBeVisible());

    const updated = { ...aiOverview, usage: { ...aiOverview.usage, server: { ...aiOverview.usage.server, spent_micros: 1_300_000 } } };
    act(() => EventSourceStub.instance?.emit(
      "ai",
      new MessageEvent("ai", {
        data: JSON.stringify({ id: 23, type: "ai", data: updated }),
        lastEventId: "23",
      }),
    ));
    await waitFor(() => expect(screen.getByText("1300000 spent")).toBeVisible());
  });

  it("applies a named server event's complete envelope to the shared cache", async () => {
    vi.stubGlobal("EventSource", EventSourceStub);
    render(
      <AppProviders session={session} servers={[server]}>
        <ServerState />
      </AppProviders>,
    );
    await waitFor(() => expect(screen.getByText("sleeping")).toBeVisible());

    const updatedServer = {
      ...server,
      desired_state: "running" as const,
      observed_state: "online" as const,
      player_count: 1,
    };
    act(() => EventSourceStub.instance?.emit(
      "server",
      new MessageEvent("server", {
        data: JSON.stringify({ id: 21, type: "server", data: updatedServer }),
        lastEventId: "21",
      }),
    ));

    await waitFor(() => expect(screen.getByText("online")).toBeVisible());
  });
});
