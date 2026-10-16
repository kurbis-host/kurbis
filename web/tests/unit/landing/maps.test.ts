/**
 * The wall of maps at the end of the film (lib/landing/film/maps.ts): what each map says
 * from the service's figures, and how it is drawn in the map colours.
 */

import { describe, expect, it } from "vitest";
import { COPY } from "../../../lib/landing/copy";
import { LAYOUT, MAP, MINUTES, faceOf, measure, minutes, paint, percent, roof, tickColor, tipOf, tone } from "../../../lib/landing/film/maps";
import type { FleetStats } from "../../../lib/landing/stats";
import type { Face } from "../../../lib/landing/engine/bundle";

const GIB = 1024 ** 3;
const NOW = "2026-10-16T12:00:30Z";
const minute = (ago: number) => new Date(Date.parse("2026-10-16T12:00:00Z") - ago * 60_000).toISOString();

function stats(overrides: Partial<FleetStats> = {}): FleetStats {
  return {
    measured_at: NOW,
    servers_created: 1284,
    servers_online: 9,
    players_online: 23,
    cpu_used_millis: 3400,
    cpu_total_millis: 16000,
    memory_used_bytes: 21 * GIB,
    memory_total_bytes: 64 * GIB,
    tps: 19.6,
    history: [],
    servers_by_day: [100, 120, 1200],
    ...overrides,
  };
}

/** A face whose every glyph is a block three wide, so widths are easy to count. */
const glyphs: Face = { rows: 8, glyphs: new Proxy({}, { get: () => ["###", "###", "###", "###", "###", "###", "###", "..."] }) as Record<string, string[]> };

describe("a map's figure and line", () => {
  it("counts servers, and those awake, and players", () => {
    expect(faceOf("servers", stats())).toMatchObject({ title: "Servers", figure: "1,284", under: COPY.ending.maps.servers.under });
    expect(faceOf("online", stats()).figure).toBe("9");
    expect(faceOf("players", stats()).figure).toBe("23");
  });

  it("says CPU and memory as shares of the live workers, and the amounts under them", () => {
    expect(faceOf("cpu", stats())).toMatchObject({ figure: "21%", under: "3.4 of 16 cores" });
    expect(faceOf("ram", stats())).toMatchObject({ figure: "33%", under: "21 of 64 GB" });
    expect(percent(0.034)).toBe("3.4%");
    expect(percent(1)).toBe("100%");
  });

  it("knows no figure it was not given, but an idle service uses nothing and its ticks sleep", () => {
    expect(faceOf("cpu", stats({ cpu_used_millis: null }))).toMatchObject({ figure: null, under: COPY.ending.maps.cpu.under });
    expect(faceOf("cpu", stats({ cpu_total_millis: null })).figure).toBeNull();
    const idle = stats({ servers_online: 0, players_online: 0, cpu_used_millis: null, memory_used_bytes: null, tps: null });
    expect(faceOf("cpu", idle).figure).toBe("0.0%");
    expect(faceOf("tps", idle)).toMatchObject({ figure: COPY.ending.asleep.figure, under: COPY.ending.asleep.under });
    expect(faceOf("tps", stats({ tps: null })).figure).toBeNull();
  });

  it("has only its title before there are figures", () => {
    expect(faceOf("tps", null)).toEqual({ stat: "tps", title: "TPS", figure: null, under: COPY.ending.maps.tps.under, graph: null });
  });
});

describe("a map's graph", () => {
  it("puts each minute's sample in its column, now in the last, and leaves the minutes nobody sampled empty", () => {
    const history = [
      { minute: minute(200), servers_online: 50, players_online: 0, tps: 20 },
      { minute: minute(119), servers_online: 4, players_online: 0, tps: 20 },
      { minute: minute(2), servers_online: 7, players_online: 0, tps: 18.5 },
    ].map((sample) => ({ cpu_used_millis: null, cpu_total_millis: null, memory_used_bytes: null, memory_total_bytes: null, ...sample }));
    const columns = minutes("online", stats({ history }));
    expect(columns).toHaveLength(MINUTES);
    expect(columns[0]).toBe(4);
    expect(columns[MINUTES - 3]).toBe(7);
    expect(columns[MINUTES - 2]).toBeNull();
    expect(columns[MINUTES - 1]).toBe(9);
    expect(columns.filter((value) => value !== null)).toHaveLength(3);
  });

  it("is a bar a day for servers, today's being the count now, and rounds its top up", () => {
    const graph = faceOf("servers", stats())!.graph!;
    expect(graph).toMatchObject({ columns: [100, 120, 1284], bars: true, top: 2000 });
    expect(roof(37, 4)).toBe(50);
    expect(roof(0, 4)).toBe(5);
    expect(roof(0.34, 0.1)).toBe(0.5);
    expect(faceOf("tps", stats())!.graph!.top).toBe(20);
  });

  it("colours ticks as the debug screen does", () => {
    expect(tickColor(19.9)).toEqual(tone("grass", "normal"));
    expect(tickColor(16)).toEqual(tone("yellow", "low"));
    expect(tickColor(9)).toEqual(tone("red", "high"));
  });
});

describe("drawing a map", () => {
  it("is the game's map colours at their shades", () => {
    // MapColor.SAND at NORMAL (220 of 255), as the game shades it.
    expect(tone("sand", "normal")).toEqual([213, 201, 140]);
    expect(tone("sand", "high")).toEqual([247, 233, 163]);
  });

  it("sets a figure as large as fits", () => {
    const paper = paint(faceOf("servers", stats({ servers_created: 1234567 })), glyphs);
    // Too wide at four times the face's size: it is set smaller, from the same left edge.
    expect(measure(glyphs, "1,234,567", 4)).toBeGreaterThan(MAP - 2 * LAYOUT.margin);
    expect(paper.at(LAYOUT.margin, LAYOUT.figure + 4)).not.toEqual(tone("sand", "normal"));
  });

  it("is blank paper with its title until the figures come", () => {
    const paper = paint(faceOf("cpu", null), glyphs);
    const [, bottom] = LAYOUT.graph;
    // (Nothing under the title: no figure, no line, no graph.)
    for (let y = LAYOUT.figure; y < bottom; y += 1) for (let x = 8; x < MAP - 1; x += 8) expect(paper.at(x, y)).toEqual(tone("sand", "normal"));
    expect(paper.at(LAYOUT.margin, LAYOUT.title)).toEqual(tone("orange", "lowest"));
  });

  it("raises a column as high as its value, lit along its top", () => {
    const paper = paint(faceOf("online", stats({ servers_online: 5 })), glyphs); // its top is 5: the last column is full height
    const [top, bottom] = LAYOUT.graph;
    const x = LAYOUT.margin - 1 + MINUTES - 1;
    expect(paper.at(x, top)).toEqual(tone("grass", "high"));
    expect(paper.at(x, bottom - 1)).toEqual(tone("grass", "normal"));
    expect(paper.at(x - 1, bottom - 1)).toEqual(tone("sand", "normal")); // (a minute nobody sampled)
  });
});

describe("a map's tooltip", () => {
  it("gives the exact figures, their stretch, and how old they are", () => {
    expect(tipOf("ram", stats(), 4.2)).toEqual(["21 of 64 GB in use", "Last 2 hours", "Updated 4 s ago"]);
    expect(tipOf("servers", null, 0)).toEqual([COPY.ending.waiting]);
  });
});
