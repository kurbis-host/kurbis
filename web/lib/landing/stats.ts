/**
 * The whole service at a glance, for the maps on the wall at the end of the film and the
 * list in the page's own HTML: servers made, servers and players online, the CPU and
 * memory they use, their ticks per second, and how those went lately.
 */

import type { components } from "../api/generated/schemas";
import { COPY } from "./copy";

export type FleetStats = components["schemas"]["FleetStats"];
export type FleetSample = components["schemas"]["FleetSample"];

/** Seconds between asks while the maps are in sight; the answer may be that old already. */
export const EVERY = 10;

/** The service now. No session is needed. */
export async function fleetStats(signal?: AbortSignal): Promise<FleetStats> {
  // Plain fetch, not the panel's API client: the landing page carries as little as it can.
  const response = await fetch("/api/v1/stats", { signal, headers: { accept: "application/json" } });
  if (!response.ok) throw new Error(`landing: the stats answered ${response.status}`);
  return (await response.json()) as FleetStats;
}

export type Stat = "servers" | "online" | "players" | "cpu" | "ram" | "tps";
/** In the order the maps hang, left to right and top to bottom. */
export const STATS: readonly Stat[] = ["servers", "online", "players", "cpu", "ram", "tps"];

/** What a map's copy holds (./copy.ts): its title and line, its tooltip, and its sentence for the HTML. */
export interface Said {
  title: string;
  under: string;
  /** The line under a share's figure, once its whole is known: "3.4 of 16 cores". */
  of?: string;
  tip: string;
  span: string;
  said: string;
}

export const saidOf = (stat: Stat): Said => COPY.ending.maps[stat];

const GIB = 1024 ** 3;
const count = new Intl.NumberFormat("en-US");

/** An amount as a line says it: "3.4", or "21" once it is large. */
export function amount(value: number): string {
  return value < 9.95 ? value.toFixed(1) : String(Math.round(value));
}

/** The copy with its {...} filled in. */
export function filled(template: string, values: Record<string, string | number>): string {
  return template.replace(/\{(\w+)\}/g, (all, key: string) => (key in values ? String(values[key]) : all));
}

/** The figures the copy's {...} name, as the maps' tooltips and the page's HTML say them. */
export function mapValues(stats: FleetStats): Record<string, string | number> {
  const used = (part: number | null | undefined, unit: number) => amount((part ?? 0) / unit);
  return {
    servers: count.format(stats.servers_created),
    online: count.format(stats.servers_online),
    players: count.format(stats.players_online),
    cores: used(stats.cpu_used_millis, 1000),
    allCores: stats.cpu_total_millis ? Math.round(stats.cpu_total_millis / 1000) : "?",
    gb: used(stats.memory_used_bytes, GIB),
    allGb: stats.memory_total_bytes ? Math.round(stats.memory_total_bytes / GIB) : "?",
    tps: stats.tps !== null && stats.tps !== undefined ? stats.tps.toFixed(1) : "-",
  };
}

/** A map's sentence for the page's own HTML (null while the figures are not in). */
export function sentenceOf(stat: Stat, stats: FleetStats | null): string | null {
  if (!stats) return null;
  if (stat === "tps" && (stats.tps === null || stats.tps === undefined) && stats.servers_online === 0) return COPY.ending.asleep.said;
  return filled(saidOf(stat).said, mapValues(stats));
}
