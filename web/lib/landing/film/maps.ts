/**
 * Home: the wall of maps beside the meadow's address (./ending.ts). Six maps in item
 * frames say how the whole service is doing right now, one thing each: servers made,
 * servers awake, players on, CPU, memory, ticks per second. Each is a big figure over a
 * graph of how it went lately, drawn as the game draws a map: 128 pixels square, in the
 * map colours, each at one of their four shades, the ground lit where it rises.
 *
 * The figures are asked for every few seconds while the wall is in sight (../stats.ts),
 * and a red dot blinks in each map's corner while they are fresh. Until the first answer
 * (or if none comes) the maps are blank paper with their titles: nothing is made up.
 */

import { CanvasTexture, Mesh, NearestFilter, SRGBColorSpace, Vector3 } from "three";
import type { Face } from "../engine/bundle";
import { modelMaterial } from "../engine/models";
import { COPY } from "../copy";
import { EVERY, STATS, amount, filled, fleetStats, mapValues, saidOf, type FleetSample, type FleetStats, type Stat } from "../stats";
import { vec, type Ctx } from "./context";

/** A map's pixels across and down, as the game's. */
export const MAP = 128;
/** Columns of a graph of the last two hours, a minute each; and bars of the last thirty days. */
export const MINUTES = 120;
export const DAYS = 30;
/** Seconds after which an answer is no longer "now": the dot stops blinking. */
const FRESH = 30;

// -- the map colours -------------------------------------------------------------------
type Rgb = readonly [number, number, number];
/** The game's map colours used here (MapColor), at their brightest. */
const BASE = {
  sand: [247, 233, 163],
  grass: [127, 178, 56],
  fire: [255, 0, 0],
  orange: [216, 127, 51],
  lightBlue: [102, 153, 216],
  yellow: [229, 229, 51],
  brown: [102, 76, 51],
  red: [153, 51, 51],
  purple: [127, 63, 178],
  lapis: [74, 128, 255],
} as const satisfies Record<string, Rgb>;
type Base = keyof typeof BASE;
/** The four shades every map colour comes in, as the game multiplies them (of 255): low, normal, high, lowest. */
const SHADES = { low: 180, normal: 220, high: 255, lowest: 135 } as const;
type Shade = keyof typeof SHADES;

export function tone(base: Base, shade: Shade): Rgb {
  const k = SHADES[shade];
  const [r, g, b] = BASE[base];
  return [Math.floor((r * k) / 255), Math.floor((g * k) / 255), Math.floor((b * k) / 255)];
}

const PAPER = tone("sand", "normal");
const EDGE = tone("sand", "low");
const INK = tone("brown", "lowest");
const SOFT = tone("brown", "high");
/** Each map's own colour: its title and its graph. */
const HUE: Record<Stat, Base> = { servers: "purple", online: "grass", players: "lightBlue", cpu: "orange", ram: "lapis", tps: "grass" };

// -- what a map says ---------------------------------------------------------------------
/** A graph: one value per column (null where nothing is known) and the value at the top. */
export interface Graph {
  columns: (number | null)[];
  top: number;
  /** Bars a few pixels wide with gaps (days), or one pixel a column (minutes). */
  bars: boolean;
}

export interface MapFace {
  stat: Stat;
  title: string;
  /** The big figure; null until it is known. */
  figure: string | null;
  under: string;
  graph: Graph | null;
}

type Usage = Pick<FleetSample, "servers_online" | "players_online" | "cpu_used_millis" | "cpu_total_millis" | "memory_used_bytes" | "memory_total_bytes" | "tps">;

const GIB = 1024 ** 3;
const count = new Intl.NumberFormat("en-US");

/** A share as the map says it: "34%", or "3.2%" while it is small. */
export function percent(share: number): string {
  const value = share * 100;
  return `${value < 9.95 ? value.toFixed(1) : Math.round(value)}%`;
}


/** The smallest of a few round numbers at or above `value`, for the top of a graph. */
export function roof(value: number, least: number): number {
  const at = Math.max(value, least);
  const magnitude = 10 ** Math.floor(Math.log10(at));
  for (const step of [1, 2, 2.5, 5, 10]) if (step * magnitude >= at) return step * magnitude;
  return 10 * magnitude;
}

/** What a sample says of one thing: a count, a share, the ticks (null where it is not known). */
export function valueOf(stat: Exclude<Stat, "servers">, sample: Usage): number | null {
  // With nothing running, nothing is used: the service is asleep, not unknown.
  const share = (used: number | null | undefined, total: number | null | undefined) => {
    if (!total) return null;
    if (used === null || used === undefined) return sample.servers_online === 0 ? 0 : null;
    return used / total;
  };
  switch (stat) {
    case "online":
      return sample.servers_online;
    case "players":
      return sample.players_online;
    case "cpu":
      return share(sample.cpu_used_millis, sample.cpu_total_millis);
    case "ram":
      return share(sample.memory_used_bytes, sample.memory_total_bytes);
    case "tps":
      return sample.tps ?? null;
  }
}

/** The last two hours a column a minute, the newest at the right: the sample of each minute, and now in the last. */
export function minutes(stat: Exclude<Stat, "servers">, stats: FleetStats): (number | null)[] {
  const now = Date.parse(stats.measured_at);
  const columns: (number | null)[] = new Array(MINUTES).fill(null);
  for (const sample of stats.history) {
    const ago = Math.floor((now - Date.parse(sample.minute)) / 60_000);
    if (ago >= 0 && ago < MINUTES) columns[MINUTES - 1 - ago] = valueOf(stat, sample);
  }
  columns[MINUTES - 1] = valueOf(stat, stats);
  return columns;
}

const highest = (columns: readonly (number | null)[]) => Math.max(0, ...columns.map((v) => v ?? 0));

/** What a map says, from the service's figures (or only its title, before there are any). */
export function faceOf(stat: Stat, stats: FleetStats | null): MapFace {
  const said = saidOf(stat);
  if (!stats) return { stat, title: said.title, figure: null, under: said.under, graph: null };
  if (stat === "servers") {
    const days = stats.servers_by_day.slice(-DAYS);
    days.splice(days.length - 1, 1, stats.servers_created); // (today so far is the count now)
    return { stat, title: said.title, figure: count.format(stats.servers_created), under: said.under, graph: { columns: days, top: roof(highest(days), 4), bars: true } };
  }
  const value = valueOf(stat, stats);
  const columns = minutes(stat, stats);
  switch (stat) {
    case "online":
    case "players":
      return { stat, title: said.title, figure: count.format(value ?? 0), under: said.under, graph: { columns, top: roof(highest(columns), 4), bars: false } };
    case "cpu":
    case "ram": {
      const [used, total, unit] = stat === "cpu" ? [stats.cpu_used_millis, stats.cpu_total_millis, 1000] : [stats.memory_used_bytes, stats.memory_total_bytes, GIB];
      const under = value !== null && total && said.of ? filled(said.of, { used: amount((used ?? 0) / unit), total: Math.round(total / unit) }) : said.under;
      return { stat, title: said.title, figure: value === null ? null : percent(value), under, graph: { columns, top: roof(highest(columns), 0.1), bars: false } };
    }
    case "tps": {
      // Nothing ticks while every server sleeps: the map says so rather than show no figure.
      const asleep = value === null && stats.servers_online === 0;
      return {
        stat,
        title: said.title,
        figure: value !== null ? value.toFixed(1) : asleep ? COPY.ending.asleep.figure : null,
        under: asleep ? COPY.ending.asleep.under : said.under,
        graph: { columns, top: 20, bars: false },
      };
    }
  }
}

/** The ticks' colour, as the debug screen's tick chart has it: green when the game keeps up. */
export function tickColor(tps: number): Rgb {
  return tps >= 18 ? tone("grass", "normal") : tps >= 15 ? tone("yellow", "low") : tone("red", "high");
}

// -- drawing a map -------------------------------------------------------------------------
/** A map's pixels, top row first, four bytes each. */
export class Paper {
  readonly data = new Uint8ClampedArray(MAP * MAP * 4);

  set(x: number, y: number, [r, g, b]: Rgb) {
    if (x < 0 || y < 0 || x >= MAP || y >= MAP) return;
    const i = (y * MAP + x) * 4;
    this.data[i] = r;
    this.data[i + 1] = g;
    this.data[i + 2] = b;
    this.data[i + 3] = 255;
  }

  rect(x: number, y: number, w: number, h: number, color: Rgb) {
    for (let j = y; j < y + h; j += 1) for (let i = x; i < x + w; i += 1) this.set(i, j, color);
  }

  at(x: number, y: number): Rgb {
    const i = (y * MAP + x) * 4;
    return [this.data[i], this.data[i + 1], this.data[i + 2]];
  }
}

/** How wide a line of the pixel face is at a scale. */
export function measure(face: Face, text: string, scale = 1): number {
  let width = 0;
  for (const ch of text) width += (face.glyphs[ch] ?? face.glyphs["?"])[0].length + 1;
  return Math.max(width - 1, 0) * scale;
}

function write(paper: Paper, face: Face, text: string, x: number, y: number, scale: number, color: Rgb) {
  let at = x;
  for (const ch of text) {
    const glyph = face.glyphs[ch] ?? face.glyphs["?"];
    glyph.forEach((row, r) => {
      for (let c = 0; c < row.length; c += 1) if (row[c] === "#") paper.rect(at + c * scale, y + r * scale, scale, scale, color);
    });
    at += (glyph[0].length + 1) * scale;
  }
}

/** Where a map's parts lie, in its pixels. */
export const LAYOUT = { margin: 5, title: 5, figure: 22, under: 54, graph: [66, 122] as const, dot: [117, 6, 5] as const };

/** The live dot's corner of a map: red while the figures are fresh and it is lit, paper otherwise. */
export function dot(paper: Paper, on: boolean) {
  const [x, y, size] = LAYOUT.dot;
  paper.rect(x, y, size, size, on ? tone("fire", "high") : PAPER);
  if (on) paper.rect(x, y + size - 1, size, 1, tone("fire", "low")); // (shaded underneath, as the map shades a rise)
}

/** A map, drawn. */
export function paint(face: MapFace, glyphs: Face, paper = new Paper()): Paper {
  const { margin } = LAYOUT;
  paper.rect(0, 0, MAP, MAP, PAPER);
  // An edge a shade darker, so maps side by side read as maps and not as one sheet.
  paper.rect(0, 0, MAP, 1, EDGE);
  paper.rect(0, MAP - 1, MAP, 1, EDGE);
  paper.rect(0, 0, 1, MAP, EDGE);
  paper.rect(MAP - 1, 0, 1, MAP, EDGE);

  const hue = HUE[face.stat];
  write(paper, glyphs, face.title.toUpperCase(), margin, LAYOUT.title, 2, tone(hue, "lowest"));
  // The line under the figure says what it counts: without a figure, there is nothing to say.
  if (face.figure !== null) {
    const room = MAP - 2 * margin;
    const scale = [4, 3, 2].find((k) => measure(glyphs, face.figure!, k) <= room) ?? 1;
    write(paper, glyphs, face.figure, margin, LAYOUT.figure + (4 - scale) * 4, scale, INK);
    write(paper, glyphs, face.under, margin, LAYOUT.under, 1, SOFT);
  }

  const [top, bottom] = LAYOUT.graph;
  // The ground under the graph, as a map's flat land: the baseline a shade darker.
  paper.rect(margin - 1, bottom, MAP - 2 * (margin - 1), 1, EDGE);
  const graph = face.graph;
  if (!graph) return paper;
  const height = bottom - top;
  const span = MAP - 2 * (margin - 1);
  const width = graph.bars ? Math.floor(span / graph.columns.length) : 1;
  const left = margin - 1 + Math.floor((span - width * graph.columns.length) / 2);
  let before = 0;
  graph.columns.forEach((value, i) => {
    if (value === null) return;
    const rise = Math.max(value > 0 ? 1 : 0, Math.round((Math.min(value, graph.top) / graph.top) * height));
    const x = left + i * width;
    const w = graph.bars ? width - 1 : 1;
    const fillTone = face.stat === "tps" ? tickColor(value) : tone(hue, "normal");
    paper.rect(x, bottom - rise, w, rise, fillTone);
    // Lit where the ground rises from the column before, darker where it falls: the game's map shading.
    const lip: Rgb = face.stat === "tps" ? fillTone : tone(hue, rise >= before ? "high" : "low");
    if (rise > 0) paper.rect(x, bottom - rise, w, 1, lip);
    before = rise;
  });
  return paper;
}

// -- the wall --------------------------------------------------------------------------------
/** What the game's tooltip says over a map: the exact figures, over what stretch, and how many seconds ago they came. */
export function tipOf(stat: Stat, stats: FleetStats | null, ago: number): string[] {
  if (!stats) return [COPY.ending.waiting];
  const said = saidOf(stat);
  return [filled(said.tip, mapValues(stats)), said.span, filled(COPY.ending.updated, { seconds: Math.max(0, Math.round(ago)) })];
}

interface Hung {
  stat: Stat;
  canvas: HTMLCanvasElement;
  context: CanvasRenderingContext2D;
  texture: CanvasTexture;
  paper: Paper;
  face: MapFace;
}

export function setMaps(ctx: Ctx, onStats: (stats: FleetStats) => void) {
  const { stage, tl, C, points } = ctx;
  const home = C.HOME;
  const glyphs = stage.data.glyphs.pixel;
  const hung: Hung[] = [];
  STATS.forEach((stat, k) => {
    const mesh = ctx.find(`Ending_Map_${k}_Map`) as Mesh | null;
    if (!mesh || typeof document === "undefined") return;
    const canvas = document.createElement("canvas");
    canvas.width = canvas.height = MAP;
    const context = canvas.getContext("2d");
    if (!context) return;
    const texture = new CanvasTexture(canvas);
    texture.colorSpace = SRGBColorSpace;
    texture.magFilter = texture.minFilter = NearestFilter;
    texture.generateMipmaps = false;
    mesh.material = modelMaterial(stage.globals, { kind: "tex", image: "map" }, { ...stage.pictures, named: new Map([["map", texture]]) });
    const face = faceOf(stat, null);
    hung.push({ stat, canvas, context, texture, paper: paint(face, glyphs), face });
  });
  if (!hung.length) return;

  let stats: FleetStats | null = null;
  let got = -Infinity; // when the last answer came (performance.now, ms)
  const show = (lit: boolean) => {
    for (const map of hung) {
      dot(map.paper, lit);
      map.context.putImageData(new ImageData(map.paper.data, MAP, MAP), 0, 0);
      map.texture.needsUpdate = true;
    }
  };
  const redraw = () => {
    for (const map of hung) {
      map.face = faceOf(map.stat, stats);
      paint(map.face, glyphs, map.paper);
    }
  };

  // Ask while the wall is in sight and the page is looked at; one question at a time.
  let asking: AbortController | null = null;
  let asked = -Infinity;
  let warned = false;
  const ask = (time: number) => {
    if (asking || time - asked < EVERY) return;
    asked = time;
    const question = (asking = new AbortController());
    fleetStats(question.signal).then(
      (answer) => {
        stats = answer;
        got = performance.now();
        redraw();
        onStats(answer);
      },
      (error: unknown) => {
        // (Once: the maps stay blank, or keep the last figures, and ask again in a while.)
        if (!question.signal.aborted && !warned) console.warn("landing: the stats could not be fetched", error);
        warned ||= !question.signal.aborted;
      },
    ).finally(() => {
      if (asking === question) asking = null;
    });
  };
  const stop = () => {
    asking?.abort();
    asking = null;
    asked = -Infinity;
  };

  // The pointer on a map: the game's tooltip, with the figures as they are.
  points.ending.maps.forEach((at, k) => {
    const map = hung[k];
    if (!map) return;
    ctx.touch.add({
      name: `map-${map.stat}`,
      at: vec(at),
      half: new Vector3(0.5, 0.08, 0.5),
      seen: [home, C.END + 0.01],
      tip: () => ctx.ui?.draw.tooltip(map.face.title, tipOf(map.stat, stats, (performance.now() - got) / 1000)) ?? null,
    });
  });

  let shown = "";
  tl.during(home - 0.05, C.END + 0.05, ({ s, time }) => {
    // (Also called once as the scroll leaves: then, and while the page is not looked at, nobody asks.)
    if (s < home - 0.05 || s > C.END + 0.05 || document.hidden) {
      stop();
      return;
    }
    ask(time);
    const fresh = performance.now() - got < FRESH * 1000;
    const lit = fresh && Math.floor(time) % 2 === 0;
    const key = `${stats?.measured_at ?? ""}:${lit}`;
    if (key === shown) return;
    shown = key;
    show(lit);
  });
  show(false);
}
