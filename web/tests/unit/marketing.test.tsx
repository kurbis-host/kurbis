import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import LandingPage from "../../app/(marketing)/page";
import LoginPage from "../../app/(marketing)/login/page";
import { session } from "./fixtures";

const push = vi.fn();
vi.mock("next/headers", () => ({
  cookies: async () => ({ getAll: () => [] }),
}));
vi.mock("next/navigation", () => ({
  redirect: vi.fn(() => {
    throw new Error("Public pages must not redirect based on a session");
  }),
  useRouter: () => ({ push }),
}));

beforeEach(() => {
  // A browser without WebGL (jsdom has no canvas at all): the film cannot play, and the page must not need it.
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  push.mockReset();
});

describe("standalone marketing pages", () => {
  it.each([
    ["offline", () => Promise.reject(new TypeError("fetch failed"))],
    ["unresponsive", () => new Promise<Response>(() => {})],
    ["unavailable", () => Promise.resolve(new Response("unavailable", { status: 503 }))],
    ["signed out", () => Promise.resolve(new Response("unauthorized", { status: 401 }))],
    ["signed in", () => Promise.resolve(Response.json(session))],
  ] as const)("renders the landing page without consulting the backend (%s)", async (_state, respond) => {
    const fetch = vi.fn(respond);
    vi.stubGlobal("fetch", fetch);

    render(<LandingPage />);

    // Everything the film says is on the page as text, in reading order, whether or not the film plays.
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Your world is waiting below");
    expect(screen.getAllByRole("heading", { level: 2 }).map((heading) => heading.textContent)).toEqual([
      "You choose the name.",
      "Cost: nothing.",
      "Sleeps when you leave. Wakes when you join.",
      "Your server. Your rules.",
      "Ask the spawner anything.",
      "Pick a name.",
    ]);
    expect(screen.getByText("No card. No ads.")).toBeInTheDocument();
    expect(screen.getByText("2 GB RAM. 5 GB world. One server each.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Jump to portal" })).toHaveAttribute("href", "#name");
    expect(screen.getByRole("link", { name: "Sign in" })).toHaveAttribute("href", "/login");
    expect(screen.getByRole("textbox", { name: /Pick a name for your server/ })).toBeInTheDocument();
    // Where there is no WebGL (as here), nothing of the film is fetched either.
    await waitFor(() => expect(screen.getByRole("main")).toHaveAttribute("data-film", "failed"));
    expect(fetch).not.toHaveBeenCalled();
  });

  it("runs the sleep/wake demo locally without API requests", async () => {
    const fetch = vi.fn(() => Promise.reject(new TypeError("fetch failed")));
    vi.stubGlobal("fetch", fetch);
    const user = userEvent.setup();
    render(<LandingPage />);

    const state = screen.getAllByRole("status")[0];
    expect(state).toHaveTextContent("The server is awake.");
    await user.click(screen.getByRole("button", { name: "Pull it: everyone leaves, and the server sleeps." }));
    expect(state).toHaveTextContent("Nobody home. It sleeps. Every block saved.");
    expect(state).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Pull it: someone joins, and the server wakes." }));
    expect(state).toHaveTextContent("The server is awake.");
    expect(fetch).not.toHaveBeenCalled();
  });

  it("checks a typed name and goes on to sign in, without API requests", async () => {
    const fetch = vi.fn(() => Promise.reject(new TypeError("fetch failed")));
    vi.stubGlobal("fetch", fetch);
    const user = userEvent.setup();
    render(<LandingPage />);

    const field = screen.getByRole("textbox", { name: /Pick a name for your server/ });
    await user.type(field, "Steve");
    expect(field).toHaveValue("steve");
    expect(await screen.findByText("steve.kurbis.host is taken. Try another name.", {}, { timeout: 3000 })).toBeInTheDocument();
    await user.keyboard("{Enter}");
    expect(push).not.toHaveBeenCalled();

    await user.clear(field);
    await user.type(field, "pumpkin-patch");
    expect(await screen.findByText("pumpkin-patch.kurbis.host is free. Press Enter to step through.", {}, { timeout: 3000 })).toBeInTheDocument();
    await user.keyboard("{Enter}");
    expect(push).toHaveBeenCalledWith("/login");
    expect(fetch).not.toHaveBeenCalled();
  });

  it("renders sign-in options without contacting the providers or control plane", () => {
    const fetch = vi.fn(() => Promise.reject(new TypeError("fetch failed")));
    vi.stubGlobal("fetch", fetch);
    render(<LoginPage />);

    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Sign in to Kurbis");
    expect(screen.getByRole("link", { name: "Continue with Google" })).toHaveAttribute("href", "/auth/google/login");
    expect(screen.getByRole("link", { name: "Continue with Discord" })).toHaveAttribute("href", "/auth/discord/login");
    expect(screen.getByRole("link", { name: "Back to Kurbis" })).toHaveAttribute("href", "/");
    expect(fetch).not.toHaveBeenCalled();
  });
});
