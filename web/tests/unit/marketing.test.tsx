import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import LandingPage from "../../app/(marketing)/page";
import LoginPage from "../../app/(marketing)/login/page";
import { landing } from "../../lib/landing/store";

beforeEach(() => {
  // jsdom has no WebGL, so the landing world falls back to its flat picture.
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
});

afterEach(() => {
  cleanup();
  landing.reset();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("standalone marketing pages", () => {
  it("renders the landing page without contacting the backend", () => {
    const fetch = vi.fn(() => Promise.reject(new TypeError("fetch failed")));
    vi.stubGlobal("fetch", fetch);

    render(<LandingPage />);

    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Your world is waiting below.");
    expect(screen.getByRole("link", { name: "Sign in" })).toHaveAttribute("href", "/login");
    expect(screen.getByRole("link", { name: /Open dashboard/ })).toHaveAttribute("href", "/login");
    expect(fetch).not.toHaveBeenCalled();
  });

  it("advertises only the AI requests the in-game assistant really takes", () => {
    render(<LandingPage />);

    const spawner = screen.getByRole("region", { name: "Ask the spawner anything." });
    const commands = within(spawner).getAllByText(/^\/ai /).map((command) => command.textContent);
    expect(commands.length).toBeGreaterThan(0);
    for (const command of commands) expect(command).toMatch(/^\/ai (?:(?:ask|new) .+|status|cancel)$/);
  });

  it("runs the wake/sleep demo locally without API requests", async () => {
    const fetch = vi.fn(() => Promise.reject(new TypeError("fetch failed")));
    vi.stubGlobal("fetch", fetch);
    const user = userEvent.setup();
    render(<LandingPage />);

    const cave = screen.getByRole("region", { name: "It only glows when someone's home." });
    expect(within(cave).getByText(/^Asleep\./)).toBeVisible();
    await user.click(within(cave).getByRole("button", { name: "A player joins" }));
    expect(within(cave).getByText("Awake. Someone's home.")).toBeVisible();
    await user.click(within(cave).getByRole("button", { name: "Everyone leaves" }));
    expect(within(cave).getByText(/^Everyone left\. Lights out in/)).toBeVisible();
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
