/**
 * What a free name gets in either name field: a splash, as on the game's title screen.
 * Yellow words with a dark shadow, tilted up to the right over the corner of the address
 * (as the game's is over its logo's), throbbing twice a second. Pointed at, it says what is
 * under the hood. (On Halloween the hero's title has one of its own: ./halloween.ts.)
 */

import { Box3, Matrix4, Mesh, Vector2, Vector3, type BufferGeometry, type Object3D } from "three";
import { mergeGeometries } from "three/addons/utils/BufferGeometryUtils.js";
import type { Face, Palette } from "../engine/bundle";
import type { Card, Draw } from "../engine/hud";
import type { Stage } from "../engine/stage";
import { lineMask, scrimMesh, typeGeometry } from "../engine/type";
import { COPY } from "../copy";
import type { Ctx } from "./context";
import type { Tip } from "./interface";

const RAD = Math.PI / 180;
/** How tall a lower-case letter is, as a share of a capital: the block face has capitals only, so lower case is set in smaller ones. */
export const SMALL = 0.7;
/** The game's pure yellow, its sides a darker gold. */
const YELLOW: Palette = { front: [1.0, 1.0, 0.12], top: [1.0, 0.9, 0.1], side: [0.78, 0.62, 0.04], bottom: [0.5, 0.4, 0.02] };
/** And the game's shadow under it: the yellow at a quarter. */
const SHADOW: Palette = { front: [0.25, 0.25, 0.0], top: [0.25, 0.25, 0.0], side: [0.2, 0.2, 0.0], bottom: [0.14, 0.14, 0.0] };

/**
 * The splash at `cell`, saying `words` as they are written (lower case in smaller capitals:
 * see `splashGeometry`), tilted, its origin at its middle (it throbs about it). `scrim` is
 * how dark a soft card behind it is, where it stands on something bright; 0 for none.
 */
export function makeSplash(stage: Stage, cell: number, { words, day = 0, scrim = 0 }: { words: string; day?: number; scrim?: number }): Mesh {
  const face = stage.data.glyphs.block;
  const splash = new Mesh(splashGeometry(face, words, YELLOW, cell, 3), stage.typeLook("Type", day));
  splash.geometry.computeBoundingBox();
  const size = splash.geometry.boundingBox!.getSize(new Vector3());
  splash.geometry.translate(0, 0, -size.z / 2);
  // The shadow: a pixel of each letter down and to the right of it, just behind its face.
  const shadow = new Mesh(splashGeometry(face, words, SHADOW, cell, 2, 1), stage.typeLook("Type", day));
  shadow.geometry.translate(0, 0, -size.z / 2);
  shadow.position.set(0, 0.5 * cell, 0);
  splash.add(shadow);
  if (scrim > 0) {
    const card = scrimMesh(size.x * 1.04, size.z * 1.5, size.z * 0.9, scrim);
    card.position.y = 3 * cell;
    card.renderOrder = 5;
    splash.add(card);
  }
  splash.rotation.y = -20 * RAD; // (up to the right)
  splash.visible = false;
  return splash;
}

/**
 * Words in the block face on one baseline, as one geometry: capitals at `cell`, and lower
 * case as capitals SMALL as tall (the face has no other letters), each a cell from the
 * next, and all of them `depth` cells (of `cell`) deep. Its origin is at the bottom centre
 * of the front, as the stage's type has it. `drop` moves each letter that many of its own
 * cells down and to the right: for the shadow under it.
 */
export function splashGeometry(face: Face, words: string, palette: Palette, cell: number, depth: number, drop = 0): BufferGeometry {
  const letters: BufferGeometry[] = [];
  let x = 0;
  for (const character of words) {
    const size = character === character.toUpperCase() ? cell : cell * SMALL;
    const mask = lineMask(face, character);
    const width = mask[0].length * size;
    if (mask.some((row) => row.includes(true))) letters.push(typeGeometry(mask, palette, size, (depth * cell) / size).translate(x + width / 2 + drop * size, 0, -drop * size));
    x += width + cell;
  }
  const line = mergeGeometries(letters);
  for (const letter of letters) letter.dispose();
  return line.translate(-(x - cell) / 2, 0, 0);
}

/** How large the splash is at a time: the game's 1.8 less a tenth of a bounce, twice a second, as a share of 1.8. */
export function throb(time: number): number {
  return 1 - (Math.abs(Math.sin(time * 2 * Math.PI)) * 0.1) / 1.8;
}

/** The link on the splash's tooltip, by the name it is pressed by (the two fields' splashes share it). */
const LINK = "splash-link";
/** Interface pixels between the splash, or the field's line, and the tooltip. */
const APART = 3;

/**
 * The splash can be pointed at (or tapped): a tooltip says what Kurbis runs on, with a link
 * to it, and stays while the pointer goes onto it (./touch.ts). The pointer finds the
 * splash as it is drawn, tilted and throbbing: its box in the world would take in the sky
 * round it. The tooltip hangs `side` it, clear of its letters and inside the frame; over
 * it, it also keeps over `keep` (interface pixels up from the frame's foot: where the
 * field's line is).
 */
export function touchSplash(ctx: Ctx, splash: Mesh, { name, seen, side, keep }: { name: string; seen: readonly [number, number]; side: "over" | "under"; keep?: () => number }) {
  const { camera } = ctx.stage;
  ctx.touch.button(LINK, () => ctx.open(COPY.hood.link[1]));
  // Its letters' face, and a little round it, for their shadow (and a finger): low end, high end, high end's top, low end's top.
  const { min, max } = splash.geometry.boundingBox!;
  const pad = (max.z - min.z) / 4;
  const face = [[min.x - pad, min.z - pad], [max.x + pad, min.z - pad], [max.x + pad, max.z + pad], [min.x - pad, max.z + pad]].map(([x, z]) => new Vector3(x, min.y, z));
  const back = face.map((corner) => corner.clone().setY(max.y));
  const point = new Vector3();
  const box = new Box3();
  const middle = new Vector3();
  const half = new Vector3();
  const now = face.map(() => new Vector2());
  const resting = face.map(() => new Vector2());
  const rest = new Matrix4();
  const steady = new Vector3(1, 1, 1);
  /**
   * Where its corners are on screen: as it is drawn, from the frame's top left as shares of it;
   * or as it rests, in interface pixels from the bottom left (the tooltip holds still while it throbs).
   */
  const onScreen = (corners: Vector2[], still: boolean) => {
    splash.updateWorldMatrix(true, false);
    if (still) rest.multiplyMatrices(splash.parent!.matrixWorld, rest.compose(splash.position, splash.quaternion, steady));
    const ui = ctx.ui;
    face.forEach((corner, i) => {
      point.copy(corner).applyMatrix4(still ? rest : splash.matrixWorld).project(camera);
      if (still && ui) corners[i].set((point.x * 0.5 + 0.5) * ui.hud.width, (point.y * 0.5 + 0.5) * ui.hud.height);
      else corners[i].set(point.x * 0.5 + 0.5, 0.5 - point.y * 0.5);
    });
    return corners;
  };
  let tip: { card: Card; link: NonNullable<Tip["link"]> } | null = null;
  const made = (draw: Draw) => {
    const { card, link } = draw.tooltipWithLink(COPY.hood.title, COPY.hood.lines, COPY.hood.link[0]);
    return { card, link: { ...link, press: LINK } };
  };
  ctx.touch.add({
    name,
    at: () => {
      // (Its box in the world, as it is now: how near it is, and what the hitboxes show.)
      splash.updateWorldMatrix(true, false);
      box.makeEmpty();
      for (const corner of [...face, ...back]) box.expandByPoint(point.copy(corner).applyMatrix4(splash.matrixWorld));
      box.getSize(half).multiplyScalar(0.5);
      return box.getCenter(middle);
    },
    half,
    seen,
    here: () => shown(splash),
    // On it if on the same side of each of its four edges.
    over: (x, y) => {
      const corners = onScreen(now, false);
      let turn = 0;
      for (let i = 0; i < 4; i += 1) {
        const [a, b] = [corners[i], corners[(i + 1) % 4]];
        const cross = Math.sign((b.x - a.x) * (y - a.y) - (b.y - a.y) * (x - a.x));
        if (cross !== 0 && cross === -turn) return false;
        turn ||= cross;
      }
      return true;
    },
    tip: () => (tip ??= ctx.ui ? made(ctx.ui.draw) : null)?.card ?? null,
    link: () => tip?.link,
    // Over it, from its high end; or under it, from its middle: as far along as the frame allows, and only as far
    // up (or down) as its letters need, where the card is over them (the splash rises to the right).
    hang: (card) => {
      const [low, high, highTop, lowTop] = onScreen(resting, true);
      const width = ctx.ui?.hud.width ?? 0;
      const along = (from: Vector2, to: Vector2, x: number) => from.y + (to.y - from.y) * Math.min(Math.max((x - from.x) / (to.x - from.x || 1), 0), 1);
      const left = Math.min(Math.max(side === "over" ? high.x : (low.x + high.x) / 2, 4), width - 4 - card.width);
      if (side === "over") return { at: [left, Math.max(along(lowTop, highTop, left + card.width), keep?.() ?? -Infinity) + APART], anchor: [0, 0] };
      return { at: [left, along(low, high, left) - APART], anchor: [0, 1] };
    },
  });
}

/** Whether a thing is drawn: it, and everything it hangs from. */
function shown(thing: Object3D): boolean {
  for (let at: Object3D | null = thing; at; at = at.parent) if (!at.visible) return false;
  return true;
}
