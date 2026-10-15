/**
 * What a friend says when clicked: each place's own lines, shuffled once, said in turn and
 * round again, going on where they left off; what has happened there, at the next clicks after.
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { PerspectiveCamera } from "three";
import { describe, expect, it } from "vitest";
import { COPY } from "../../../lib/landing/copy";
import { seeded } from "../../../lib/landing/anim/easing";
import type { Face } from "../../../lib/landing/engine/bundle";
import { Draw } from "../../../lib/landing/engine/hud";
import { Speech, Turns, halves, type After } from "../../../lib/landing/film/speech";

const { talk } = COPY;
const FRIENDS = ["Steve", "Alex"] as const;
type Friend = (typeof FRIENDS)[number];
/** The places with friends, the homestead by its features; and what happens at each that they say something of after. */
const STOPS = {
  pond: { lines: talk.pond, events: [talk.pond.boot] },
  cave: { lines: talk.cave, events: [talk.cave.woke] },
  gate: { lines: talk.gate, events: [talk.gate.left] },
  market: { lines: talk.market, events: [] },
  night: { lines: talk.night, events: [talk.night.blown, talk.night.restored] },
};
type Stop = keyof typeof STOPS;
const EVERY = (Object.keys(STOPS) as Stop[]).flatMap((stop) => FRIENDS.map((friend) => [stop, friend] as const));
/** A place's lines for a friend, and what is said first there, as the film has them (the pond's fishers say the boot too). */
const linesOf = (stop: Stop, friend: Friend): readonly string[] => (stop === "pond" ? [...talk.pond[friend], talk.fishing] : STOPS[stop].lines[friend]);
const firstOf = (stop: Stop, friend: Friend) => (stop === "cave" && friend === "Steve" ? talk.cave.mines : null);
const turns = (stop: Stop, friend: Friend, seed: number) => new Turns(linesOf(stop, friend), firstOf(stop, friend), seeded(seed));
const say = (turns: Turns, count: number, after: readonly After[] = []) => Array.from({ length: count }, () => turns.take(after));
/** Something that happens, at moments the test sets (null: it has not, or no longer holds). */
const event = (lines: readonly string[]) => {
  const one = { at: null as number | null, lines, since: () => one.at };
  return one;
};

describe("what the friends say", () => {
  it.each(EVERY)("at the %s, %s says each line once before any again, and then again in the same order", (stop, friend) => {
    const lines = linesOf(stop, friend);
    const said = say(turns(stop, friend, 1), 2 * lines.length);
    const round = said.slice(0, lines.length);
    expect(new Set(round)).toEqual(new Set(lines));
    expect(said.slice(lines.length)).toEqual(round);
  });

  it("is shuffled: another visit has it in another order", () => {
    for (const [stop, friend] of EVERY) {
      const orders = new Set([1, 2, 3, 4, 5, 6].map((seed) => say(turns(stop, friend, seed), linesOf(stop, friend).length).join("|")));
      expect(orders.size).toBeGreaterThan(1);
    }
  });

  it("starts in the cave, for Steve, with the mines, which then come round with the rest", () => {
    const lines = talk.cave.Steve;
    for (let seed = 1; seed <= 20; seed += 1) {
      const said = say(turns("cave", "Steve", seed), 3 * lines.length);
      expect(said[0]).toBe(talk.cave.mines);
      expect(new Set(said.slice(0, lines.length))).toEqual(new Set(lines));
      expect(said.filter((line) => line === talk.cave.mines)).toHaveLength(3);
      expect(said.every((line, i) => i === 0 || line !== said[i - 1])).toBe(true); // (never the same twice running)
    }
  });

  it("goes on where it left off at each place, whatever was said elsewhere in between", () => {
    const speech = new Speech(new PerspectiveCamera(), new Map());
    const at = (lines: readonly string[]) => {
      const click = speech.speaker({ rig: "Steve", seen: [0, 1], headline: [], lines });
      return () => {
        click(0);
        const bubbles = speech["bubbles"];
        return bubbles[bubbles.length - 1].text;
      };
    };
    const [pond, cave] = [at(talk.pond.Steve), at(talk.cave.Steve)];
    const there = [pond(), pond()];
    const elsewhere = [cave(), cave(), cave()];
    const back = Array.from({ length: talk.pond.Steve.length - 2 }, pond);
    expect(new Set([...there, ...back])).toEqual(new Set(talk.pond.Steve));
    expect(new Set(elsewhere).size).toBe(3);
    for (const line of elsewhere) expect(talk.cave.Steve).toContain(line);
  });

  it("is different at each stop, five to seven lines a friend", () => {
    for (const friend of FRIENDS) {
      const seen = new Map<string, Stop>();
      for (const stop of Object.keys(STOPS) as Stop[]) {
        const { lines, events } = STOPS[stop];
        expect(lines[friend].length).toBeGreaterThanOrEqual(5);
        expect(lines[friend].length).toBeLessThanOrEqual(7);
        expect(new Set(lines[friend]).size).toBe(lines[friend].length);
        for (const line of [...lines[friend], ...events.flatMap((one) => one[friend])]) {
          expect(seen.get(line), `${friend}: "${line}" at the ${stop}`).toBeUndefined();
          seen.set(line, stop);
        }
      }
    }
  });

  it.each(EVERY)("at the %s, %s says what has happened only after it has", (stop, friend) => {
    const after = STOPS[stop].events.map((one) => event(one[friend]));
    const later = after.flatMap(({ lines }) => lines);
    for (const line of later) expect(linesOf(stop, friend)).not.toContain(line);
    const t = turns(stop, friend, 7);
    const before = say(t, 3 * linesOf(stop, friend).length, after);
    for (const line of later) expect(before).not.toContain(line);
    // It happens: what is said of it comes next, in order, and then the place's own lines again.
    after.forEach((one, k) => {
      one.at = 10 + k;
      expect(say(t, one.lines.length, after)).toEqual(one.lines);
      expect(one.lines).not.toContain(t.take(after));
    });
  });

  it("says what has happened once each time it happens, not again while it still holds", () => {
    const left = event(talk.gate.left.Alex);
    const t = turns("gate", "Alex", 3);
    left.at = 5;
    expect(say(t, 2, [left])).toEqual(talk.gate.left.Alex);
    const meanwhile = say(t, 20, [left]);
    for (const line of talk.gate.left.Alex) expect(meanwhile).not.toContain(line);
    left.at = 9; // (let in again, and gone again)
    expect(say(t, 2, [left])).toEqual(talk.gate.left.Alex);
    left.at = null; // (the gate has started over)
    expect(talk.gate.Alex).toContain(t.take([left]));
  });

  it("says of the house what holds now: blown up, or back", () => {
    const blown = event(talk.night.blown.Steve);
    const back = event(talk.night.restored.Steve);
    // (As the film has it: the blast is said of only until the house is back.)
    const after: After[] = [{ since: () => (back.at === null ? blown.at : null), lines: blown.lines }, back];
    const t = turns("night", "Steve", 2);
    blown.at = 4;
    back.at = 6; // (scrolled on past both before a click)
    expect(say(t, back.lines.length, after)).toEqual(back.lines);
    expect(blown.lines).not.toContain(t.take(after));
  });

  it("does not say again at once what was just said because something happened (a boot came up)", () => {
    for (let seed = 1; seed <= 60; seed += 1) {
      const t = turns("pond", "Steve", seed);
      t.said(talk.fishing);
      expect(t.take()).not.toBe(talk.fishing);
      t.said(talk.jockey);
      expect(t.take()).not.toBe(talk.jockey);
    }
  });

  it("is spread over the deck, whichever comes first", () => {
    const lines = linesOf("pond", "Steve");
    const firsts = new Map<string, number>();
    for (let seed = 1; seed <= 600; seed += 1) {
      const [line] = say(turns("pond", "Steve", seed), 1);
      firsts.set(line, (firsts.get(line) ?? 0) + 1);
    }
    expect(firsts.size).toBe(lines.length);
    for (const count of firsts.values()) expect(count).toBeGreaterThan(60);
  });

  it("is chat for Alex: lower case, no full stop", () => {
    for (const stop of Object.keys(STOPS) as Stop[]) {
      for (const line of [...STOPS[stop].lines.Alex, ...STOPS[stop].events.flatMap((one) => one.Alex)]) {
        expect(line).toBe(line.toLowerCase());
        expect(line.endsWith(".")).toBe(false);
      }
    }
  });
});

describe("a long line in a bubble", () => {
  const measure = (line: string) => line.length;

  it("breaks in two where the halves come out most even", () => {
    expect(halves("i swear i saw herobrine", measure)).toEqual(["i swear i", "saw herobrine"]);
  });

  it("stays whole without a space to break at", () => {
    expect(halves("sleep", measure)).toEqual(["sleep"]);
  });

  it("is never more than two lines, each well inside a phone's frame (234 interface pixels across)", () => {
    const face = (JSON.parse(readFileSync(join(__dirname, "../../../public/landing/scene.json"), "utf8")) as { glyphs: { pixel: Face } }).glyphs.pixel;
    const draw = new Draw(face);
    const all = (Object.keys(STOPS) as Stop[]).flatMap((stop) => FRIENDS.flatMap((friend) => [...linesOf(stop, friend), ...STOPS[stop].events.flatMap((one) => one[friend])]));
    for (const line of [...all, talk.jockey]) {
      for (const ch of line) expect(face.glyphs[ch], `"${ch}" in "${line}"`).toBeDefined();
      expect(Math.max(...halves(line, (part) => draw.measure(part)).map((part) => draw.measure(part))), line).toBeLessThanOrEqual(140);
    }
  });
});
