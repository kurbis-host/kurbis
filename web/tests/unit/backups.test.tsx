import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { describe, expect, it } from "vitest";
import { Backups } from "../../components/backups";
import { queryKeys } from "../../lib/query";
import { server, snapshot } from "./fixtures";

function wrapper(client: QueryClient) {
  return function Wrapper({ children }: { children: ReactNode }) { return <QueryClientProvider client={client}>{children}</QueryClientProvider>; };
}

describe("backup restrictions", () => {
  it("allows restore while sleeping and manual snapshots only while online", () => {
    const client = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity } } });
    client.setQueryData(queryKeys.server(server.id), server);
    client.setQueryData(queryKeys.snapshots(server.id), [snapshot]);
    render(<Backups initialServer={server} initialSnapshots={[snapshot]} />, { wrapper: wrapper(client) });
    expect(screen.getByRole("button", { name: "Restore" })).toBeEnabled();
    expect(screen.getByRole("button", { name: /create manual backup/i })).toBeDisabled();
  });
});
