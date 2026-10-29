import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import catalogJson from "../../../src/contracts/world_catalog.json";
import { Settings } from "../../components/settings";
import { CurrentWorld, describeWorld, flatProblem, newWorldRequest } from "../../components/world";
import type { components } from "../../lib/api/generated";
import { browserApi } from "../../lib/api/operations";
import { queryKeys } from "../../lib/query";
import { server } from "./fixtures";

vi.mock("next/navigation", () => ({ useRouter: () => ({ replace: vi.fn() }) }));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const catalog = catalogJson as components["schemas"]["WorldCatalog"];
const theVoid = catalog.flat_presets.find((preset) => preset.id === "the_void")!.settings;

function renderWith(node: ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, retry: false } } });
  client.setQueryData(queryKeys.server(server.id), server);
  client.setQueryData(queryKeys.worldCatalog, catalog);
  render(<QueryClientProvider client={client}>{node}</QueryClientProvider>);
}

async function openReset(name: RegExp) {
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name }));
  return { user, dialog: screen.getByRole("dialog") };
}

describe("world descriptions", () => {
  it("names world types, presets and biomes as Minecraft does", () => {
    expect(describeWorld({ world_type: "normal", structures: true, seed: 1 }, catalog)).toBe("Default");
    expect(describeWorld({ world_type: "flat", flat: theVoid, structures: false, seed: 1 }, catalog)).toBe("Superflat, The Void");
    expect(
      describeWorld({ world_type: "flat", flat: { ...theVoid, lakes: true }, structures: false, seed: 1 }, catalog),
    ).toBe("Superflat, custom layers");
    expect(
      describeWorld({ world_type: "single_biome_surface", biome: "minecraft:cherry_grove", structures: true, seed: 1 }, catalog),
    ).toBe("Single Biome, Cherry Grove");
  });

  it("shows the seed with a copy button, or says it is not known yet", async () => {
    const user = userEvent.setup();
    renderWith(<CurrentWorld world={{ world_type: "amplified", structures: false, seed: -42 }} />);
    expect(screen.getByText("AMPLIFIED")).toBeVisible();
    expect(screen.getByText("Off")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Copy seed" }));
    expect(await navigator.clipboard.readText()).toBe("-42");
    expect(screen.getByText("Copied")).toBeVisible();
  });

  it("explains a world whose seed was never recorded", () => {
    renderWith(<CurrentWorld world={{ world_type: "normal", structures: true, seed: null }} />);
    expect(screen.getByText("Shown once the world next runs")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Copy seed" })).toBeNull();
  });
});

describe("new world requests", () => {
  it("send only the choices that belong to the world type and a trimmed seed", () => {
    expect(newWorldRequest({ world_type: "normal", structures: true, flat: theVoid, biome: "minecraft:desert", seed: "  " })).toEqual({
      world_type: "normal",
      structures: true,
    });
    expect(newWorldRequest({ world_type: "flat", structures: false, flat: theVoid, biome: "minecraft:desert", seed: " glacier " })).toEqual({
      world_type: "flat",
      structures: false,
      flat: theVoid,
      seed: "glacier",
    });
  });

  it("refuse superflat layers Pumpkin could not generate", () => {
    expect(flatProblem(theVoid, catalog)).toBeNull();
    expect(flatProblem({ ...theVoid, layers: [] }, catalog)).toMatch(/at least one layer/);
    expect(flatProblem({ ...theVoid, layers: [{ block: "minecraft:nonsense", height: 1 }] }, catalog)).toMatch(/not a block/);
    expect(flatProblem({ ...theVoid, layers: [{ block: "minecraft:stone", height: 0 }] }, catalog)).toMatch(/one block high/);
    expect(flatProblem({ ...theVoid, layers: [{ block: "minecraft:stone", height: 385 }] }, catalog)).toMatch(/room for 384/);
  });
});

describe("reset dialog", () => {
  it("resets to a single biome world with a typed seed", async () => {
    const command = vi.spyOn(browserApi, "command").mockResolvedValue({ observed_state: "resetting" });
    renderWith(<Settings initialServer={server} />);
    expect(screen.getByText("-1120102460")).toBeVisible();
    const { user, dialog } = await openReset(/^world reset$/i);
    await user.selectOptions(within(dialog).getByRole("combobox", { name: "World type" }), "single_biome_surface");
    await user.selectOptions(within(dialog).getByRole("combobox", { name: "Biome" }), "minecraft:cherry_grove");
    await user.click(within(dialog).getByRole("switch", { name: "Generate structures" }));
    await user.type(within(dialog).getByRole("textbox", { name: "Seed" }), "kurbis");
    await user.click(within(dialog).getByRole("button", { name: "Reset world" }));
    expect(command).toHaveBeenCalledWith(server.id, {
      command: "reset",
      mode: "world",
      world: { world_type: "single_biome_surface", biome: "minecraft:cherry_grove", structures: false, seed: "kurbis" },
    });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("builds superflat layers from a preset and edits them", async () => {
    const command = vi.spyOn(browserApi, "command").mockResolvedValue({ observed_state: "resetting" });
    renderWith(<Settings initialServer={server} />);
    const { user, dialog } = await openReset(/full server reset/i);
    await user.selectOptions(within(dialog).getByRole("combobox", { name: "World type" }), "flat");
    expect(within(dialog).getByRole("combobox", { name: "Preset" })).toHaveValue("classic_flat");
    await user.selectOptions(within(dialog).getByRole("combobox", { name: "Preset" }), "the_void");
    expect(within(dialog).getByText("1 of 384 blocks high")).toBeVisible();

    await user.click(within(dialog).getByRole("button", { name: "Add layer on top" }));
    expect(within(dialog).getByRole("combobox", { name: "Preset" })).toHaveValue("custom");
    const top = within(dialog).getByRole("combobox", { name: "Layer 2 block" });
    await user.clear(top);
    await user.type(top, "minecraft:nonsense");
    expect(within(dialog).getByRole("alert")).toHaveTextContent("minecraft:nonsense is not a block");
    expect(within(dialog).getByRole("button", { name: "Reset full server" })).toBeDisabled();
    await user.clear(top);
    await user.type(top, "minecraft:glass");
    const height = within(dialog).getByRole("spinbutton", { name: "Layer 2 height" });
    await user.clear(height);
    await user.type(height, "3");
    await user.click(within(dialog).getByRole("checkbox", { name: "Villages" }));
    await user.click(within(dialog).getByRole("checkbox", { name: "Ancient cities" }));
    await user.click(within(dialog).getByRole("button", { name: "Reset full server" }));

    expect(command).toHaveBeenCalledWith(server.id, {
      command: "reset",
      mode: "full",
      world: {
        world_type: "flat",
        structures: true,
        flat: {
          ...theVoid,
          layers: [...theVoid.layers, { block: "minecraft:glass", height: 3 }],
          structure_overrides: ["minecraft:ancient_cities", "minecraft:villages"],
        },
      },
    });
  });

  it("keeps the dialog open with the error when the reset is refused", async () => {
    const command = vi.spyOn(browserApi, "command").mockRejectedValue(new Error("unknown biome minecraft:x"));
    renderWith(<Settings initialServer={server} />);
    const { user, dialog } = await openReset(/^world reset$/i);
    await user.click(within(dialog).getByRole("button", { name: "Reset world" }));
    expect(await screen.findByText("unknown biome minecraft:x")).toBeInTheDocument();
    expect(screen.getByRole("dialog")).toBeVisible();
  });

  it("starts every reset from a default world with a random seed", async () => {
    const command = vi.spyOn(browserApi, "command").mockResolvedValue({ observed_state: "resetting" });
    renderWith(<Settings initialServer={server} />);
    const { user, dialog } = await openReset(/^world reset$/i);
    await user.click(within(dialog).getByRole("button", { name: "Reset world" }));
    expect(command).toHaveBeenCalledWith(server.id, { command: "reset", mode: "world", world: { world_type: "normal", structures: true } });
  });
});
