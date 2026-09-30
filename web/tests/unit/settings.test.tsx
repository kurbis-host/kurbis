import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { describe, expect, it, vi } from "vitest";
import { Settings } from "../../components/settings";
import { queryKeys } from "../../lib/query";
import { server } from "./fixtures";

vi.mock("next/navigation", () => ({ useRouter: () => ({ replace: vi.fn() }) }));

function wrapper(client: QueryClient) {
  return function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  };
}

describe("settings draft isolation", () => {
  it("does not overwrite a dirty draft when live server state changes", async () => {
    const client = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity } } });
    client.setQueryData(queryKeys.server, server);
    render(<Settings initialServer={server} />, { wrapper: wrapper(client) });
    const user = userEvent.setup();
    const motd = screen.getByRole("textbox", { name: "MOTD" });
    await user.clear(motd);
    await user.type(motd, "Draft survives SSE");
    await act(async () => {
      client.setQueryData(queryKeys.server, { ...server, observed_state: "queued", desired_state: "running", updated_at: "2026-09-30T18:30:00Z" });
    });
    await waitFor(() => expect(screen.getByText(/configuration is locked while the world is queued/i)).toBeVisible());
    expect(motd).toHaveValue("Draft survives SSE");
    expect(screen.getByText("Unsaved draft")).toBeVisible();
    expect(screen.getByRole("button", { name: /save configuration/i })).toBeDisabled();
  });
});
