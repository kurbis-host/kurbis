/**
 * The meadow where the pumpkin lands: the address, whose name retypes itself, and the
 * two friends on different editions at the same pond.
 */

import type { Object3D } from "three";
import { POP, smooth, wrap } from "../anim/easing";
import { grow } from "../anim/timeline";
import type { Ctx } from "./context";

/** The scroll position at which the address stands up. */
const SHOWN = 1.05;
/** The loop of the name: seconds a name stands, a letter takes to type and to wipe, and the gap between. */
const HELD = 2.4;
const TYPING = 0.11;
const WIPING = 0.05;
const PAUSE = 0.25;
const HOST = ".KURBIS.HOST";

export function setSurface(ctx: Ctx) {
  const { tl, C, stage, tall } = ctx;
  const face = stage.data.glyphs.block;
  const widthOf = (ch: string) => face.glyphs[ch]?.[0]?.length ?? 0;
  const lineWidth = (text: string) => [...text].reduce((sum, ch) => sum + widthOf(ch) + 1, -1);
  const cell = ctx.pick(0.115, 0.11);
  const over = tall ? (face.rows + 3) * cell : 0; // in the tall frame the name stands over the host
  const hostWidth = lineWidth(HOST);

  const block = ctx.node("Landing_Type");
  const root = ctx.node("Landing_Address");
  const host = ctx.node("Landing_Host");
  const caption = ctx.node("Landing_Caption");
  const editions = ctx.node("Landing_Editions");
  const tags = [ctx.node("Tag_Steve"), ctx.node("Tag_Alex")];
  const rows = C.NAMES.map((name) => {
    let x = 0;
    return [...name].map((ch, i) => {
      const letter = { letter: ctx.node(`Landing_${name}_${i}`), offset: x, width: widthOf(ch) };
      x += letter.width + 1;
      return letter;
    });
  });

  // What stands there at each moment of the loop, [seconds, which name, how many letters],
  // and when each letter is typed.
  const steps: [number, number, number][] = [[0, 0, C.NAMES[0].length]];
  const typed = new Map<Object3D, number>();
  let t = 0;
  let k = 0;
  for (const following of [1, 2, 0]) {
    t += HELD;
    for (let n = C.NAMES[k].length - 1; n >= 0; n -= 1) {
      steps.push([t, k, n]);
      t += WIPING;
    }
    t += PAUSE;
    for (let n = 1; n <= C.NAMES[following].length; n += 1) {
      steps.push([t, following, n]);
      typed.set(rows[following][n - 1].letter, t);
      t += TYPING;
    }
    k = following;
  }
  const period = t;


  const stood = tl.trigger(SHOWN, 0.28);
  const captioned = tl.trigger(SHOWN + 0.07, 0.28);
  const edition = tl.trigger(SHOWN + 0.1, 0.28);
  const tagged = tl.trigger(SHOWN + 0.05, 0.28);
  let width: number | null = null;

  tl.during(0.6, 2.0, ({ s, dt }) => {
    // The type belongs to this stop: it goes as the ground breaks.
    const leaving = 1 - smooth((s - C.STOMP) / 0.07);
    grow(block, leaving);
    grow(root, POP(stood.t / 0.28));
    grow(caption, POP(captioned.t / 0.28));
    grow(editions, POP(edition.t / 0.28));
    for (const tag of tags) grow(tag, POP(tagged.t / 0.28) * leaving);
    if (!block.visible || !root.visible) return;

    // The address. Its line re-centres as the name grows and shrinks.
    const u = stood.t > 0 ? stood.t % period : 0;
    let which = 0;
    let count = C.NAMES[0].length;
    for (const [moment, name, letters] of steps) {
      if (moment > u + 1e-9) break;
      which = name;
      count = letters;
    }
    const want = count ? rows[which][count - 1].offset + rows[which][count - 1].width : 0;
    width = width === null ? want : width + (want - width) * (1 - Math.exp(-22 * dt));
    const space = Math.min(width / 3, 1);
    const left = tall ? -width / 2 : -(width + space + hostWidth) / 2;
    rows.forEach((row, j) => {
      row.forEach(({ letter, offset, width: w }, i) => {
        const on = j === which && i < count;
        letter.visible = on;
        if (!on) return;
        const age = wrap(u - (typed.get(letter) ?? 0), period);
        letter.scale.setScalar(1 + 0.18 * Math.sin(Math.PI * Math.min(age / 0.12, 1)));
        letter.position.set((left + offset + w / 2) * cell, 0, over);
      });
    });
    host.position.x = tall ? 0 : (left + width + space + hostWidth / 2) * cell;
  });
}
