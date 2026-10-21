export function formatBytes(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
  return `${(bytes / 1024 ** 3).toFixed(2)} GiB`;
}

export function formatDateTime(value: string) {
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(new Date(value));
}

export function formatRelative(value: string, now = Date.now()) {
  const delta = new Date(value).getTime() - now;
  const abs = Math.abs(delta);
  const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  if (abs < 60_000) return formatter.format(Math.round(delta / 1000), "second");
  if (abs < 3_600_000) return formatter.format(Math.round(delta / 60_000), "minute");
  if (abs < 86_400_000) return formatter.format(Math.round(delta / 3_600_000), "hour");
  return formatter.format(Math.round(delta / 86_400_000), "day");
}

const utcDay = new Intl.DateTimeFormat("en-US", { month: "long", day: "numeric", timeZone: "UTC" });

/** A calendar day as the control plane counts it, in UTC. */
export function formatUtcDay(value: string) {
  return utcDay.format(new Date(value));
}

const count = new Intl.NumberFormat("en-US");

export function formatCount(value: number) {
  return count.format(value);
}

/** "1 tool call", "3 tool calls". */
export function formatQuantity(value: number, unit: string) {
  return `${formatCount(value)} ${value === 1 ? unit : `${unit}s`}`;
}

const MICROS_PER_USD = 1_000_000;
const MICROS_PER_CENT = 10_000;
const usd = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" });
const subCentUsd = new Intl.NumberFormat("en-US", {
  style: "currency",
  currency: "USD",
  minimumSignificantDigits: 1,
  maximumSignificantDigits: 2,
});

/**
 * Money is integer micro-USD (1 USD = 1,000,000). Shows dollars and cents;
 * amounts below one cent keep two significant digits instead of rounding to $0.00.
 */
export function formatUsd(micros: number) {
  const subCent = micros !== 0 && Math.abs(micros) < MICROS_PER_CENT;
  return (subCent ? subCentUsd : usd).format(micros / MICROS_PER_USD);
}

/** Micro-USD as the plain decimal a dollar input shows: "12.50", or every significant micro-dollar digit. */
export function usdInputValue(micros: number) {
  const cents = String(micros % MICROS_PER_USD).padStart(6, "0").replace(/0+$/, "").padEnd(2, "0");
  return `${Math.trunc(micros / MICROS_PER_USD)}.${cents}`;
}

/** A typed dollar amount ("5", "12.50", at most micro-dollar precision) in micro-USD; null when it is not one. */
export function parseUsdInput(text: string) {
  const match = /^(\d+)(?:\.(\d{1,6}))?$/.exec(text.trim());
  if (!match) return null;
  return Number(match[1]) * MICROS_PER_USD + Number((match[2] ?? "").padEnd(6, "0"));
}

const decimal = new Intl.NumberFormat("en-US", { maximumFractionDigits: 1 });

/** Thousandths of a CPU core as cores: "0.5 cores", "1 core". */
export function formatCores(millis: number) {
  const cores = decimal.format(millis / 1000);
  return `${cores} ${cores === "1" ? "core" : "cores"}`;
}

/** A share between 0 and 1 as a whole percentage. */
export function formatShare(share: number) {
  return `${Math.round(share * 100)}%`;
}
