import { useQuery } from "@tanstack/react-query";
import { act, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AppProviders } from "../../components/providers";
import { queryKeys } from "../../lib/query";
import { server, session } from "./fixtures";

class EventSourceStub {
  static instance: EventSourceStub | null = null;
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent<string>) => void) | null = null;
  readonly close = vi.fn();
  readonly addEventListener = vi.fn();

  constructor() {
    EventSourceStub.instance = this;
  }
}

function EventStatus() {
  const { data } = useQuery({
    queryKey: queryKeys.eventStatus,
    queryFn: async () => "unknown",
    staleTime: Infinity,
  });
  return <span>{String(data)}</span>;
}

afterEach(() => {
  vi.unstubAllGlobals();
  EventSourceStub.instance = null;
});

describe("event reconnect state", () => {
  it("moves from connecting to live and reconnecting as EventSource changes state", async () => {
    vi.stubGlobal("EventSource", EventSourceStub);
    render(
      <AppProviders session={session} server={server}>
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
});
