import { describe, expect, it } from "vitest";
import { formatCores, formatCount, formatShare, formatQuantity, formatUsd, formatUtcDay, parseUsdInput, usdInputValue } from "../../lib/format";

describe("micro-USD money", () => {
  it("shows dollars and cents, and keeps sub-cent amounts from reading as $0.00", () => {
    expect(formatUsd(0)).toBe("$0.00");
    expect(formatUsd(4_000_000)).toBe("$4.00");
    expect(formatUsd(100_000)).toBe("$0.10");
    expect(formatUsd(15_000)).toBe("$0.02");
    expect(formatUsd(1_234_567_890)).toBe("$1,234.57");
    expect(formatUsd(4_200)).toBe("$0.0042");
    expect(formatUsd(500)).toBe("$0.0005");
    expect(formatUsd(1)).toBe("$0.000001");
    expect(formatUsd(9_999)).toBe("$0.01");
  });

  it("parses typed dollar amounts exactly and rejects anything else", () => {
    expect(parseUsdInput("5")).toBe(5_000_000);
    expect(parseUsdInput(" 12.5 ")).toBe(12_500_000);
    expect(parseUsdInput("1.1")).toBe(1_100_000);
    expect(parseUsdInput("0.000001")).toBe(1);
    for (const text of ["", "-1", "$5", "1e3", "1.", ".5", "1.2345678", "1,000"]) {
      expect(parseUsdInput(text), text).toBeNull();
    }
  });

  it("writes budgets back into an input without losing precision", () => {
    expect(usdInputValue(0)).toBe("0.00");
    expect(usdInputValue(5_000_000)).toBe("5.00");
    expect(usdInputValue(12_500_000)).toBe("12.50");
    expect(usdInputValue(1_234_567)).toBe("1.234567");
    for (const micros of [0, 1, 990_000, 5_000_000, 1_234_567, 20_000_000]) {
      expect(parseUsdInput(usdInputValue(micros))).toBe(micros);
    }
  });
});

describe("counts and days", () => {
  it("groups counts and pluralizes units", () => {
    expect(formatCount(1_234_567)).toBe("1,234,567");
    expect(formatQuantity(0, "model call")).toBe("0 model calls");
    expect(formatQuantity(1, "tool call")).toBe("1 tool call");
    expect(formatQuantity(1_200, "tool call")).toBe("1,200 tool calls");
  });

  it("names the UTC day whatever the viewer's time zone", () => {
    expect(formatUtcDay("2026-10-01T00:00:00Z")).toBe("October 1");
    expect(formatUtcDay("2026-09-30T23:30:00-02:00")).toBe("October 1");
  });
});

describe("fleet figures", () => {
  it("shows thousandths of a core as cores, singular only for exactly one", () => {
    expect(formatCores(0)).toBe("0 cores");
    expect(formatCores(500)).toBe("0.5 cores");
    expect(formatCores(1000)).toBe("1 core");
    expect(formatCores(1049)).toBe("1 core");
    expect(formatCores(960)).toBe("1 core");
    expect(formatCores(1250)).toBe("1.3 cores");
    expect(formatCores(16_000)).toBe("16 cores");
    expect(formatCores(12_345_000)).toBe("12,345 cores");
  });

  it("shows a share as a whole percentage", () => {
    expect(formatShare(0)).toBe("0%");
    expect(formatShare(0.004)).toBe("0%");
    expect(formatShare(0.005)).toBe("1%");
    expect(formatShare(0.426)).toBe("43%");
    expect(formatShare(1)).toBe("100%");
    expect(formatShare(1.5)).toBe("150%");
  });
});
