import { describe, expect, it } from "vitest";
import { aiSettingsFormSchema, settingsFormSchema } from "../../lib/schemas";

const valid = {
  subdomain: "friends-world",
  motd: "Welcome",
  difficulty: "Normal" as const,
  gamemode: "Survival" as const,
  view_distance: 12,
  simulation_distance: 8,
  hardcore: false,
  whitelist: true,
  whitelist_by_members: true,
  tab_stats: false,
};

describe("server forms", () => {
  it("rejects unsafe addresses and out-of-range settings", () => {
    expect(settingsFormSchema.safeParse({ ...valid, subdomain: "../bad" }).success).toBe(false);
    expect(settingsFormSchema.safeParse({ ...valid, view_distance: 33 }).success).toBe(false);
    expect(settingsFormSchema.safeParse({ ...valid, subdomain: "-edge" }).success).toBe(false);
  });

  it("accepts the complete curated settings payload", () => {
    expect(settingsFormSchema.safeParse(valid).success).toBe(true);
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

describe("AI settings form", () => {
  const schema = aiSettingsFormSchema(20_000_000);
  const values = { enabled: true, model: "", effort: "" as const, monthly_budget: "" };

  it("accepts defaults and dollar budgets up to the account budget", () => {
    for (const monthly_budget of ["", "0", "5", "12.50", " 20 ", "19.999999"]) {
      expect(schema.safeParse({ ...values, monthly_budget }).success, monthly_budget).toBe(true);
    }
    expect(schema.parse({ ...values, monthly_budget: " 12.50 " }).monthly_budget).toBe("12.50");
  });

  it("rejects malformed budgets and budgets above the account budget", () => {
    for (const monthly_budget of ["abc", "-1", "$5", "1.2345678"]) {
      const result = schema.safeParse({ ...values, monthly_budget });
      expect(result.success, monthly_budget).toBe(false);
      expect(result.error?.issues[0]?.message).toBe("Enter an amount in US dollars, like 5 or 12.50.");
    }
    const above = schema.safeParse({ ...values, monthly_budget: "20.01" });
    expect(above.error?.issues.map((issue) => issue.message)).toEqual([
      "A server can't spend more than the account budget of $20.00.",
    ]);
  });
});
