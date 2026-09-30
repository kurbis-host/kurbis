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
