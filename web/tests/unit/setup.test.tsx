/** The setup page offers the name a visitor picked on the landing page before signing in. */

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Setup } from "../../components/setup";
import { keepChosenName } from "../../lib/chosen-name";
import { server } from "./fixtures";

const replace = vi.fn();
vi.mock("next/navigation", () => ({ useRouter: () => ({ replace }) }));

function wrapper({ children }: { children: ReactNode }) {
  return <QueryClientProvider client={new QueryClient()}>{children}</QueryClientProvider>;
}

afterEach(() => {
  cleanup();
  localStorage.clear();
  vi.unstubAllGlobals();
  replace.mockReset();
});

describe("world setup", () => {
  it("shows the real domain and starts empty when nothing was picked", () => {
    render(<Setup domain="kurbis.host" />, { wrapper });
    expect(screen.getByPlaceholderText("friends-world")).toHaveValue("");
    expect(screen.getByText(".kurbis.host")).toBeVisible();
  });

  it("offers the landing page's name, and keeps it once the world is made", async () => {
    keepChosenName("pumpkin-patch");
    const fetch = vi.fn(() => Promise.resolve(Response.json({ ...server, subdomain: "pumpkin-patch" }, { status: 201 })));
    vi.stubGlobal("fetch", fetch);
    render(<Setup domain="kurbis.host" />, { wrapper });
    const field = screen.getByPlaceholderText("friends-world");
    await waitFor(() => expect(field).toHaveValue("pumpkin-patch"));

    await userEvent.setup().click(screen.getByRole("button", { name: /Create sleeping world/ }));
    await waitFor(() => expect(replace).toHaveBeenCalledWith(`/manage/${server.id}`));
    const [request] = fetch.mock.calls[0] as unknown as [Request];
    expect(await request.json()).toMatchObject({ subdomain: "pumpkin-patch" });
    expect(localStorage.getItem("kurbis.name")).toBe("pumpkin-patch"); // (kept for good)
  });

  it("ignores a kept name that is not a valid address", async () => {
    localStorage.setItem("kurbis.name", "-bad-");
    render(<Setup domain="kurbis.host" />, { wrapper });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(screen.getByPlaceholderText("friends-world")).toHaveValue("");
  });
});
