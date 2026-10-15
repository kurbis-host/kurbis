/** The splash's words (lib/landing/film/splash.ts): capitals as the block face has them, lower case in smaller ones. */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { BufferGeometry } from "three";
import { describe, expect, it } from "vitest";
import type { Face, Palette } from "../../../lib/landing/engine/bundle";
import { textMask, typeGeometry } from "../../../lib/landing/engine/type";
import { SMALL, splashGeometry } from "../../../lib/landing/film/splash";

const face = (JSON.parse(readFileSync(join(__dirname, "../../../public/landing/scene.json"), "utf8")) as { glyphs: { block: Face } }).glyphs.block;
const PALETTE: Palette = { front: [1, 1, 0], top: [1, 1, 0], side: [1, 1, 0], bottom: [1, 1, 0] };

/** Every corner of the front faces (they look along -Y), rounded, as a sorted list. */
function fronts(geometry: BufferGeometry): string[] {
  const position = geometry.getAttribute("position");
  const normal = geometry.getAttribute("normal");
  const out: string[] = [];
  for (let i = 0; i < position.count; i += 1) if (normal.getY(i) < -0.5) out.push([position.getX(i), position.getY(i), position.getZ(i)].map((v) => v.toFixed(5)).join(","));
  return out.sort();
}

/** The letters' boxes along the line (x from, x to, height), left to right, from the front faces. */
function letters(geometry: BufferGeometry): [number, number, number][] {
  const position = geometry.getAttribute("position");
  const normal = geometry.getAttribute("normal");
  const quads: [number, number, number, number][] = [];
  for (let i = 0; i < position.count; i += 4) {
    if (normal.getY(i) > -0.5) continue;
    const xs = [0, 1, 2, 3].map((k) => position.getX(i + k));
    const zs = [0, 1, 2, 3].map((k) => position.getZ(i + k));
    quads.push([Math.min(...xs), Math.max(...xs), Math.min(...zs), Math.max(...zs)]);
  }
  quads.sort((a, b) => a[0] - b[0]);
  const boxes: [number, number, number][] = [];
  for (const [x0, x1, , z1] of quads) {
    const last = boxes[boxes.length - 1];
    if (last && x0 <= last[1] + 1e-6) {
      last[1] = Math.max(last[1], x1);
      last[2] = Math.max(last[2], z1);
    } else boxes.push([x0, x1, z1]);
  }
  return boxes;
}

describe("the splash's words", () => {
  it("in capitals, are set as the stage sets any type", () => {
    const words = "NOW WITH 100% MORE PUMPKIN!";
    expect(fronts(splashGeometry(face, words, PALETTE, 0.03, 3))).toEqual(fronts(typeGeometry(textMask(face, [words], { leading: 3 }), PALETTE, 0.03, 3)));
  });

  it("in lower case, are smaller capitals on the same baseline, evenly spaced", () => {
    const cell = 0.1;
    const boxes = letters(splashGeometry(face, "OOoo", PALETTE, cell, 3));
    expect(boxes).toHaveLength(4);
    const tall = face.rows * cell;
    expect(boxes.map(([, , top]) => +top.toFixed(5))).toEqual([tall, tall, tall * SMALL, tall * SMALL].map((v) => +v.toFixed(5)));
    for (let i = 1; i < boxes.length; i += 1) expect(boxes[i][0] - boxes[i - 1][1]).toBeCloseTo(cell, 6);
  });
});
