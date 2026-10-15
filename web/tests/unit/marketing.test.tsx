import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
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
  localStorage.clear();
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
      "Stops when you leave. Starts when you join.",
      "Friends in. Everyone else out.",
      "Add plugins. Or write your own.",
      "Creeper? Aw man. Turn back time.",
      "Pick a name.",
      "See you in there.",
      // (The credits the film rolls at its end, as the page's footer.)
      "How it started",
      "Built on Pumpkin",
      "The community",
    ]);
    expect(screen.getByText("No card. No ads.")).toBeInTheDocument();
    expect(screen.getByText("2 GB RAM. 5 GB world. One server each.")).toBeInTheDocument();
    // The film's bar, as links: first in the page, as the bar is first in the frame.
    const bar = screen.getByRole("navigation", { name: "Kurbis" });
    expect(within(bar).getAllByRole("link").map((link) => [link.textContent, link.getAttribute("href")])).toEqual([
      ["Sign in", "/login"],
      ["Get your server", "#name"],
    ]);
    expect(screen.getByRole("link", { name: "Impressum" })).toHaveAttribute("href", "/impressum");
    expect(screen.getByRole("link", { name: "Privacy" })).toHaveAttribute("href", "/privacy");
    expect(screen.getByRole("link", { name: "Pumpkin's Discord" })).toHaveAttribute("href", "https://discord.gg/wT8XjrjKkf");
    expect(screen.getByRole("link", { name: "Pumpkin on GitHub" })).toHaveAttribute("target", "_blank");
    expect(screen.getByRole("link", { name: "Kurbis on GitHub" })).toHaveAttribute("href", "https://github.com/kurbis-host");
    // The name can be picked at the start, and again at the end.
    expect(screen.getAllByRole("textbox", { name: /Pick a name for your server/ })).toHaveLength(2);
    // Where there is no WebGL (as here), nothing of the film is fetched either.
    await waitFor(() => expect(screen.getByRole("main")).toHaveAttribute("data-film", "failed"));
    expect(fetch).not.toHaveBeenCalled();
  });

  it("runs the sleep/wake demo locally without API requests", async () => {
    const fetch = vi.fn(() => Promise.reject(new TypeError("fetch failed")));
    vi.stubGlobal("fetch", fetch);
    const user = userEvent.setup();
    render(<LandingPage />);

    const state = screen.getAllByRole("status").find((status) => status.textContent === "The server is running.");
    expect(state).toBeDefined();
    await user.click(screen.getByRole("button", { name: "Pull it: everyone leaves, and the server stops." }));
    expect(state).toHaveTextContent("Nobody on. Server off. No phantoms.");
    expect(state).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Pull it: someone joins, and the server starts." }));
    expect(state).toHaveTextContent("The server is running.");
    expect(fetch).not.toHaveBeenCalled();
  });

  it("checks a typed name with the control plane, holds it in a claim card, and goes on to sign in", async () => {
    // The only request the page makes: whether the name is free.
    const fetch = vi.fn((url: string) => {
      const name = url.split("/").pop() ?? "";
      return Promise.resolve(Response.json({ name, free: name !== "steve" }));
    });
    vi.stubGlobal("fetch", fetch);
    const user = userEvent.setup();
    render(<LandingPage />);

    const [field, last] = screen.getAllByRole("textbox", { name: /Pick a name for your server/ });
    await user.type(field, "Steve");
    expect(last).toHaveValue("steve"); // one name, in both fields
    expect(field).toHaveValue("steve");
    expect(await screen.findByText("steve.kurbis.host is taken. Try another name.", {}, { timeout: 3000 })).toBeInTheDocument();
    await user.keyboard("{Enter}");
    expect(push).not.toHaveBeenCalled();
    expect(localStorage.getItem("kurbis.name")).toBeNull();

    await user.clear(field);
    await user.type(field, "pumpkin-patch");
    expect(await screen.findByText("pumpkin-patch.kurbis.host is free. Press Enter to claim it.", {}, { timeout: 3000 })).toBeInTheDocument();
    await user.keyboard("{Enter}");
    // Enter holds the name in the claim card: copy it into the game, or sign in.
    expect(push).not.toHaveBeenCalled();
    const card = screen.getByRole("region", { name: "pumpkin-patch.kurbis.host" });
    expect(card).toHaveTextContent("It's free. Join it in Minecraft and it's yours the moment you arrive.");
    // Kept for the tab, so setup can offer it after signing in.
    expect(localStorage.getItem("kurbis.name")).toBe("pumpkin-patch");
    await user.click(within(card).getByRole("button", { name: "Copy address" }));
    expect(await navigator.clipboard.readText()).toBe("pumpkin-patch.kurbis.host");
    expect(within(card).getByRole("button", { name: "Copied!" })).toBeInTheDocument();
    expect(within(card).getByRole("status")).toHaveTextContent("Copied. In Minecraft: Multiplayer > Add Server, paste, and join.");
    await user.click(within(card).getByRole("button", { name: "Or sign in" }));
    expect(push).toHaveBeenCalledWith("/login");
    expect(fetch.mock.calls.map(([url]) => url)).toEqual(["/api/v1/names/steve", "/api/v1/names/pumpkin-patch"]);
  });

  it("closes the claim card, from its button or with Escape", async () => {
    vi.stubGlobal("fetch", vi.fn((url: string) => Promise.resolve(Response.json({ name: url.split("/").pop(), free: true }))));
    const user = userEvent.setup();
    render(<LandingPage />);
    const [field] = screen.getAllByRole("textbox", { name: /Pick a name for your server/ });
    await user.type(field, "pumpkin-patch");
    await screen.findByText("pumpkin-patch.kurbis.host is free. Press Enter to claim it.", {}, { timeout: 3000 });
    await user.keyboard("{Enter}");
    await user.click(screen.getByRole("button", { name: "Close" }));
    expect(screen.queryByRole("region", { name: "pumpkin-patch.kurbis.host" })).not.toBeInTheDocument();
    await user.click(field);
    await user.keyboard("{Enter}");
    expect(screen.getByRole("region", { name: "pumpkin-patch.kurbis.host" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("region", { name: "pumpkin-patch.kurbis.host" })).not.toBeInTheDocument();
  });

  it("takes a free name when the field is left, without Enter, and keeps a closed card closed", async () => {
    vi.stubGlobal("fetch", vi.fn((url: string) => Promise.resolve(Response.json({ name: url.split("/").pop(), free: true }))));
    const user = userEvent.setup();
    render(<LandingPage />);
    const [field] = screen.getAllByRole("textbox", { name: /Pick a name for your server/ });
    // Left before the check is back: the card comes once it says the name is free.
    await user.type(field, "pumpkin-patch");
    await user.tab();
    const card = await screen.findByRole("region", { name: "pumpkin-patch.kurbis.host" }, { timeout: 3000 });
    expect(card).toBeInTheDocument();
    expect(localStorage.getItem("kurbis.name")).toBe("pumpkin-patch");
    await user.click(within(card).getByRole("button", { name: "Close" }));
    await user.click(field);
    await user.tab();
    expect(screen.queryByRole("region", { name: "pumpkin-patch.kurbis.host" })).not.toBeInTheDocument();
    // Enter still opens it.
    await user.click(field);
    await user.keyboard("{Enter}");
    expect(screen.getByRole("region", { name: "pumpkin-patch.kurbis.host" })).toBeInTheDocument();
  });

  it("has the name from an earlier visit in its fields again", async () => {
    localStorage.setItem("kurbis.name", "pumpkin-patch");
    vi.stubGlobal("fetch", vi.fn((url: string) => Promise.resolve(Response.json({ name: url.split("/").pop(), free: true }))));
    render(<LandingPage />);
    const fields = screen.getAllByRole("textbox", { name: /Pick a name for your server/ });
    await waitFor(() => expect(fields[0]).toHaveValue("pumpkin-patch"));
    expect(fields[1]).toHaveValue("pumpkin-patch");
  });

  it("keeps the open card on the name as it stands: checking, free, taken", async () => {
    const taken = new Set(["steve"]);
    vi.stubGlobal("fetch", vi.fn((url: string) => {
      const name = url.split("/").pop() ?? "";
      return Promise.resolve(Response.json({ name, free: !taken.has(name) }));
    }));
    const user = userEvent.setup();
    render(<LandingPage />);
    const [field] = screen.getAllByRole("textbox", { name: /Pick a name for your server/ });
    await user.type(field, "pumpkin");
    await screen.findByText("pumpkin.kurbis.host is free. Press Enter to claim it.", {}, { timeout: 3000 });
    await user.keyboard("{Enter}");
    // Typed on with the card open: it follows at once, and says it is checking.
    await user.clear(field);
    await user.type(field, "steve");
    const card = screen.getByRole("region", { name: "steve.kurbis.host" });
    expect(card).toHaveTextContent("Checking...");
    expect(within(card).queryByRole("button", { name: "Copy address" })).not.toBeInTheDocument();
    // Taken: it says so, and offers nothing to copy or sign in with.
    await waitFor(() => expect(card).toHaveTextContent("Taken. Try another name."), { timeout: 3000 });
    expect(within(card).queryByRole("button", { name: "Or sign in" })).not.toBeInTheDocument();
    // Free again: the buttons are back, and that is the name kept.
    await user.clear(field);
    await user.type(field, "pumpkin-two");
    const again = await screen.findByRole("region", { name: "pumpkin-two.kurbis.host" });
    await waitFor(() => expect(within(again).getByRole("button", { name: "Copy address" })).toBeInTheDocument(), { timeout: 3000 });
    expect(localStorage.getItem("kurbis.name")).toBe("pumpkin-two");
  });

  it("welcomes a visitor back to the server they made, with a way to its panel", async () => {
    localStorage.setItem("kurbis.name", "pumpkin-patch");
    // The name they claimed is taken now: by them.
    vi.stubGlobal("fetch", vi.fn((url: string) => Promise.resolve(Response.json({ name: url.split("/").pop(), free: false }))));
    const user = userEvent.setup();
    render(<LandingPage />);
    const card = await screen.findByRole("region", { name: "pumpkin-patch.kurbis.host" }, { timeout: 3000 });
    expect(card).toHaveTextContent("Welcome back. Your world is waiting.");
    expect(screen.getByText("pumpkin-patch.kurbis.host is yours. Welcome back.")).toBeInTheDocument();
    expect(within(screen.getByRole("navigation", { name: "Kurbis" })).getByRole("link", { name: "Your server" })).toHaveAttribute("href", "/app");
    await user.click(within(card).getByRole("button", { name: "Open panel" }));
    expect(push).toHaveBeenCalledWith("/app");
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
