/**
 * A line of type that is typed into: one mesh per letter, laid from the left, each
 * popping in as it arrives, the whole set smaller once it would outgrow its room. The
 * name fields (the sky's and the Nether's) are made of these. Some names do something
 * here, as they do in the game (./eggs.ts).
 */

import { Mesh, type BufferGeometry, type Material, type Object3D } from "three";
import type { Palette } from "../engine/bundle";
import type { Ctx } from "./context";
import { lineEggs } from "./eggs";

export interface TypedLine {
  /** The whole text as it now stands (it is set in capitals). */
  type(text: string, time: number): void;
  /** Lay the letters out for this frame; returns the cells the line takes (after fitting). */
  lay(time: number, shown: boolean): number;
  readonly text: string;
}

export interface LineStyle {
  palette: string | Palette;
  /** The size of a cell, in the parent's units. */
  cell: number;
  depth: number;
  /** Daylight on it: 1 above ground, 0 below. */
  day: number;
  /** Where the line begins, and its foot, in the parent's units (the line runs along x, standing in z). */
  left: number;
  line: number;
  /** Cells it has room for before it is set smaller. */
  room: number;
}

export function typedLine(ctx: Ctx, parent: Object3D, style: LineStyle): TypedLine {
  const { stage } = ctx;
  const face = stage.data.glyphs.block;
  const glyphs = new Map<string, BufferGeometry>();
  let look: Material | null = null;
  const letters: { mesh: Mesh; ch: string; born: number }[] = [];
  let text = "";
  const eggs = lineEggs(ctx, parent, style);

  const widthOf = (ch: string) => (face.glyphs[ch] ?? face.glyphs["?"])[0].length;
  const letter = (ch: string) => {
    const geometry = glyphs.get(ch);
    if (geometry && look) return new Mesh(geometry, look);
    const made = stage.makeType([ch], { palette: style.palette, cell: style.cell, depth: style.depth, day: style.day });
    look = made.material as Material;
    glyphs.set(ch, made.geometry);
    return made;
  };

  return {
    get text() {
      return text;
    },
    type(next, time) {
      const upper = next.toUpperCase();
      if (upper === text) return;
      // Keep the letters that still stand; the new ones pop.
      let same = 0;
      while (same < text.length && same < upper.length && text[same] === upper[same]) same += 1;
      for (const gone of letters.splice(same)) parent.remove(gone.mesh);
      [...upper.slice(same)].forEach((ch, i) => {
        const mesh = letter(ch);
        parent.add(mesh);
        letters.push({ mesh, ch, born: time + 0.02 * i });
      });
      text = upper;
      eggs.type(upper, time);
    },
    lay(time, shown) {
      let cells = 0;
      for (const { ch } of letters) cells += widthOf(ch) + 1;
      const fit = Math.min(style.room / Math.max(cells - 1, 1), 1);
      let x = 0;
      for (const { mesh, ch, born } of letters) {
        const w = widthOf(ch);
        mesh.position.set(style.left + (x + w / 2) * style.cell * fit, 0, style.line);
        mesh.scale.setScalar(fit * (1 + 0.2 * Math.sin(Math.PI * Math.min(Math.max(time - born, 0) / 0.12, 1))));
        mesh.visible = shown;
        x += w + 1;
      }
      eggs.lay(letters, time, Math.max(cells - 1, 0) * style.cell * fit, fit);
      return Math.max(cells - 1, 0) * fit;
    },
  };
}
