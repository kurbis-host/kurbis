/**
 * The page says each thing once (lib/landing/copy.ts): as HTML for readers that do not
 * see the film, and as type in the film's world. The type is laid out by the art
 * pipeline, which exports what it set; this holds the two together in both frame shapes.
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { COPY } from "../../../lib/landing/copy";

interface Reference {
  type: Record<string, string[]>;
  constants: { NAMES: string[]; ORES: string[]; ORE_LINES: string[][]; SIGNS: string[][]; ASKED: [string, string][] };
}

const loud = (words: readonly string[] | string) => (typeof words === "string" ? words : words.join(" ")).toUpperCase();

describe.each(["wide", "tall"])("the copy in the %s film", (aspect) => {
  const ref = JSON.parse(readFileSync(join(__dirname, "fixtures", `reference.${aspect}.json`), "utf8")) as Reference;
  /** Everything a block of type says, its lines in order (a block is one thing, or one thing per line). */
  const set = (block: string) => {
    if (ref.type[block]) return ref.type[block].join(" ");
    const lines = Object.keys(ref.type).filter((name) => new RegExp(`^${block}_\\d+$`).test(name)).sort((a, b) => Number(a.split("_").pop()) - Number(b.split("_").pop()));
    return lines.map((name) => ref.type[name].join(" ")).join(" ");
  };

  it("says in the world what the page says", () => {
    expect(set("Hero_Title")).toBe(loud(COPY.sky.title));
    expect(set("Hero_Caption")).toBe(loud(COPY.sky.caption));
    expect(set("Hero_Cue")).toContain(loud(COPY.sky.cue));
    expect(set("Landing_Caption")).toBe(loud(COPY.surface.caption));
    expect(set("Landing_Host")).toBe(loud(COPY.surface.host));
    expect(set("Landing_Editions")).toBe(loud(COPY.surface.editions));
    expect([set("Tag_Steve"), set("Tag_Alex")]).toEqual(COPY.surface.tags.map(loud));
    COPY.dig.facts.forEach((fact, i) => expect(set(`Band_${i}`)).toBe(loud(fact)));
    expect(set("Cave_Awake")).toBe(loud(COPY.cave.awake));
    expect(set("Cave_Asleep")).toBe(loud(COPY.cave.asleep));
    expect(set("Dungeon_Headline")).toBe(loud(COPY.dungeon.headline));
    COPY.dungeon.ores.forEach(([name], k) => expect(set(`Dungeon_Ore_${k}`)).toBe(loud(name)));
    expect(set("Dungeon_Backups")).toBe(loud(COPY.dungeon.backups[0]));
    expect(set("Spawner_Headline")).toBe(loud(COPY.spawner.headline));
    expect(set("Spawner_Small")).toBe(loud(COPY.spawner.small));
    expect(set("Nether_Headline")).toBe(loud(COPY.nether.headline));
    expect(set("Nether_Caption")).toBe(loud(COPY.nether.caption));
    expect(set("Nether_Host")).toBe(loud(COPY.surface.host));
  });

  it("says in the interface what the page says", () => {
    const { NAMES, ORES, ORE_LINES, ASKED } = ref.constants;
    expect(NAMES).toEqual(COPY.surface.names.map(loud));
    expect(ORES).toEqual(COPY.dungeon.ores.map(([name]) => loud(name)));
    expect(ORE_LINES.map((lines) => lines.join(" "))).toEqual(COPY.dungeon.ores.map(([, says]) => says));
    expect(ASKED).toEqual(COPY.spawner.asks.map(([ask, answer]) => [ask, answer]));
  });
});
