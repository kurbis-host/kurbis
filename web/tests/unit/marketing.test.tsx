import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import LandingPage from "../../app/(marketing)/page";
import LoginPage from "../../app/(marketing)/login/page";
import { session } from "./fixtures";

vi.mock("next/headers", () => ({
  cookies: async () => ({ getAll: () => [] }),
}));
vi.mock("next/navigation", () => ({
  redirect: vi.fn(() => {
    throw new Error("Public pages must not redirect based on a session");
  }),
}));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
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

    render(await LandingPage());

    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Your world waits. Your server doesn’t.");
    expect(screen.getByRole("link", { name: "Create your world" })).toHaveAttribute("href", "/login");
    expect(screen.getByRole("link", { name: "How it works" })).toHaveAttribute("href", "#how-it-works");
    expect(screen.getByRole("link", { name: "Free tier" })).toHaveAttribute("href", "#limits");
    expect(screen.getByRole("link", { name: "FAQ" })).toHaveAttribute("href", "#faq");
    expect(fetch).not.toHaveBeenCalled();
  });

  it("runs the wake/sleep demo locally without API requests", async () => {
    const fetch = vi.fn(() => Promise.reject(new TypeError("fetch failed")));
    vi.stubGlobal("fetch", fetch);
    const user = userEvent.setup();
    render(await LandingPage());

    expect(screen.getByText("World sleeping")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Wake the world" }));
    expect(screen.getByText("World online")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Let the world sleep" }));
    expect(screen.getByText("World sleeping")).toBeVisible();
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
