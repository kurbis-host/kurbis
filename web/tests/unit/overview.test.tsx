import * as Tooltip from "@radix-ui/react-tooltip";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Overview } from "../../components/overview";
import { queryKeys } from "../../lib/query";
import { server } from "./fixtures";

function wrapper(client: QueryClient) {
  return function Wrapper({ children }: { children: ReactNode }) {
    return (
      <QueryClientProvider client={client}>
        <Tooltip.Provider>{children}</Tooltip.Provider>
      </QueryClientProvider>
    );
  };
}

afterEach(() => vi.unstubAllGlobals());

describe("overview lifecycle mutations", () => {
  it("shows optimistic lifecycle feedback and rolls back on REST failure", async () => {
    let rejectRequest: ((reason?: unknown) => void) | undefined;
    vi.stubGlobal(
      "fetch",
      vi.fn(
        () =>
          new Promise<Response>((_resolve, reject) => {
            rejectRequest = reject;
          }),
      ),
    );
    const client = new QueryClient({
      defaultOptions: { queries: { staleTime: Infinity, retry: false }, mutations: { retry: false } },
    });
    client.setQueryData(queryKeys.server(server.id), server);
    render(
      <Overview initialServer={server} initialActivity={[]} renderedAt={Date.now()} />,
      { wrapper: wrapper(client) },
    );

    await userEvent.click(screen.getByRole("button", { name: "Start" }));
    await waitFor(() => expect(screen.getByRole("heading", { name: "Queued" })).toBeVisible());
    expect(client.getQueryData<typeof server>(queryKeys.server(server.id))).toMatchObject({
      desired_state: "running",
      observed_state: "queued",
    });

    rejectRequest?.(new Error("control plane unavailable"));
    await waitFor(() => expect(screen.getByRole("heading", { name: "Sleeping" })).toBeVisible());
    expect(screen.getByText("control plane unavailable")).toBeVisible();
    expect(client.getQueryData(queryKeys.server(server.id))).toMatchObject({
      desired_state: "stopped",
      observed_state: "sleeping",
    });
  });
});
