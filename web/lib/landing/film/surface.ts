/**
 * The meadow where the pumpkin lands: the address, whose name retypes itself (or is the
 * visitor's own, once they have picked a free one), and the two friends on different
 * editions at the same pond, each with a nametag that says which.
 */

import { Vector3, type Mesh, type Object3D } from "three";
import { POP, smooth, wrap } from "../anim/easing";
import type { Card } from "../engine/hud";
import { grow } from "../anim/timeline";
import { COPY } from "../copy";
import type { Ctx } from "./context";

/** The scroll position at which the address stands up. */
const SHOWN = 1.05;
/** The loop of the name: seconds a name stands, a letter takes to type and to wipe, and the gap between. */
const HELD = 2.4;
const TYPING = 0.11;
const WIPING = 0.05;
const PAUSE = 0.25;
const HOST = ".KURBIS.HOST";
/** Cells a visitor's name has before it is set smaller: on the host's line (wide), or over it (tall). */
const ROOM = { wide: 46, tall: 71 }; // (the art laid the wide line out for the longest of its own names)

export interface Surface {
  /** The visitor's own name for the address (it stands for good), or none (the names take turns again). */
  name(name: string | null, time: number): void;
}

export function setSurface(ctx: Ctx): Surface {
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
  // The friends, and where a nametag hangs: over the head of a player two blocks tall.
  const friends = (["Steve", "Alex"] as const).map((tag, i) => ({ root: ctx.node(tag), name: tag, edition: COPY.surface.tags[i], card: null as Card | null, cardFor: "" }));
  const overHead = new Vector3();
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
  // The visitor's name, set as one piece with its rim, popping in when it changes.
  let own: { text: string; mesh: Mesh; born: number } | null = null;
  const room = ctx.pick(ROOM.wide, ROOM.tall);

  tl.during(0.6, 2.0, ({ s, time, dt }) => {
    // The type belongs to this stop: it goes as the ground breaks.
    const leaving = 1 - smooth((s - C.STOMP) / 0.07);
    grow(block, leaving);
    grow(root, POP(stood.t / 0.28));
    grow(caption, POP(captioned.t / 0.28));
    grow(editions, POP(edition.t / 0.28));
    const named = POP(tagged.t / 0.28) * leaving;
    if (ctx.ui && named > 0) {
      // Over each head; where two would meet (heads in a line, in a narrow frame), the farther one stands above the nearer.
      const placed: [number, number, number, number][] = [];
      for (const friend of friends) {
        // (Drawn again if the friend's name changes: the visitor may take their place.)
        // The visitor's skin is a Java account's: in a friend's place they are on Java, and the other friend on Bedrock.
        const edition = ctx.wearer ? COPY.surface.tags[friend.name === ctx.wearer ? 0 : 1] : friend.edition;
        if (friend.cardFor !== `${ctx.players[friend.name]} ${edition}`) {
          friend.cardFor = `${ctx.players[friend.name]} ${edition}`;
          friend.card = ctx.ui.draw.nametag(ctx.players[friend.name], edition);
        }
        friend.root.updateWorldMatrix(true, false);
        overHead.setFromMatrixPosition(friend.root.matrixWorld).add(UP).project(stage.camera);
        if (overHead.z >= 1) continue;
        const x = (overHead.x * 0.5 + 0.5) * ctx.ui.hud.width;
        let y = (overHead.y * 0.5 + 0.5) * ctx.ui.hud.height;
        const card = friend.card!; // (drawn just above)
        const { width: w, height: h } = card;
        for (const [px, py, pw, ph] of placed) if (Math.abs(x - px) < (w + pw) / 2 + 2 && Math.abs(y - py) < (h + ph) / 2 + 1) y = py + ph + 2;
        placed.push([x, y, w, h]);
        ctx.ui.tips.push({ card, at: [x, y], anchor: [0.5, 0], alpha: named, of: friend.root.name });
      }
    }
    if (!block.visible || !root.visible) return;

    if (own) {
      // The visitor's own name: it stands, re-centred on the host as the cycle's names are.
      for (const row of rows) for (const { letter } of row) letter.visible = false;
      const cells = lineWidth(own.text);
      const fit = Math.min(room / Math.max(cells, 1), 1);
      width = width === null ? cells * fit : width + (cells * fit - width) * (1 - Math.exp(-22 * dt));
      const space = Math.min(width / 3, 1);
      const left = tall ? -width / 2 : -(width + space + hostWidth) / 2;
      own.mesh.visible = true;
      own.mesh.position.set((left + width / 2) * cell, 0, over);
      own.mesh.scale.setScalar(fit * (1 + 0.18 * Math.sin(Math.PI * Math.min(Math.max(time - own.born, 0) / 0.18, 1))));
      host.position.x = tall ? 0 : (left + width + space + hostWidth / 2) * cell;
      return;
    }

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

  return {
    name(name, time) {
      const text = name?.toUpperCase() ?? null;
      if (text === (own?.text ?? null)) return;
      if (own) {
        root.remove(own.mesh);
        for (const mesh of [own.mesh, ...(own.mesh.children as Mesh[])]) mesh.geometry.dispose();
        own = null;
      }
      if (!text) return;
      // As the art sets the address's letters: cream, four cells deep, with the dark rim behind.
      const mesh = stage.makeType([text], { palette: "cream", cell, depth: 4, day: 1 });
      mesh.add(stage.makeRim([text], { cell, width: 0.2, day: 1 }));
      root.add(mesh);
      own = { text, mesh, born: time };
    },
  };
}

/** From a player's feet to just over their head. */
const UP = new Vector3(0, 0, 2.3);
