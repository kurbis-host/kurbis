import { describe, expect, it } from "vitest";
import { settingsFormSchema, setupSchema } from "../../lib/schemas";

const valid = {
  subdomain: "friends-world",
  preset: "survival" as const,
  motd: "Welcome",
  difficulty: "Normal" as const,
  gamemode: "Survival" as const,
  view_distance: 12,
  simulation_distance: 8,
  hardcore: false,
  whitelist: true,
};

describe("server forms", () => {
  it("rejects unsafe addresses and out-of-range settings", () => {
    expect(setupSchema.safeParse({ ...valid, subdomain: "../bad" }).success).toBe(false);
    expect(setupSchema.safeParse({ ...valid, view_distance: 33 }).success).toBe(false);
    expect(setupSchema.safeParse({ ...valid, subdomain: "-edge" }).success).toBe(false);
  });

  it("accepts the complete curated settings payload", () => {
    const { preset: _preset, ...settings } = valid;
    expect(settingsFormSchema.safeParse(settings).success).toBe(true);
  });
});

describe("player mutation form", () => {
  it("normalizes Minecraft names and needs no UUID for an offline entry", async () => {
    const { playerMutationSchema } = await import("../../lib/schemas");
    expect(playerMutationSchema.parse({ name: "  OfflinePlayer  " })).toEqual({ name: "OfflinePlayer" });
  });

  it("rejects blank, oversized, and control-character names", async () => {
    const { playerMutationSchema } = await import("../../lib/schemas");
    expect(playerMutationSchema.safeParse({ name: "   " }).success).toBe(false);
    expect(playerMutationSchema.safeParse({ name: "x".repeat(65) }).success).toBe(false);
    expect(playerMutationSchema.safeParse({ name: "Bad\nName" }).success).toBe(false);
  });
});
