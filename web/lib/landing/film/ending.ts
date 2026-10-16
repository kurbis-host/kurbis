/**
 * Home: through the second frame and back in the valley, made whole. Over the meadow
 * stands the visitor's address (theirs, once they have picked a name); a wall of maps
 * says how the whole service is doing right now (./maps.ts); and the lectern's book holds
 * the questions people ask, a page at a time. (The credits roll after it: ./credits.ts.)
 *
 * The book opens by itself while the camera rests by the lectern, and turns its pages
 * on real time; its arrows (or a click on the lectern) turn them by hand.
 */

import { Vector3, type Mesh } from "three";
import { POP, clamp01 } from "../anim/easing";
import { grow } from "../anim/timeline";
import type { Card } from "../engine/hud";
import { COPY } from "../copy";
import { vec, type Ctx } from "./context";
import { DUSK } from "./credits";

/** Seconds a page of the book stays open before the next. */
const PAGE = 5.5;
/** The book's width, in interface pixels. */
const BOOK = 156;
/** Screens of scroll past the film's end over which the book goes, before the credits' first line reaches it. */
const LEAVE = 0.06;

export function setEnding(ctx: Ctx) {
  const { tl, C, stage, tall, points } = ctx;
  const at = points.ending;
  const home = C.HOME;
  const lectern = vec(at.lectern);

  // -- the address over the meadow, and the line under it -----------------------------------
  const anchor = vec(at.address);
  const cell = at.cell;
  const host = COPY.surface.host.toUpperCase();
  const line = stage.makeType([COPY.ending.title.toUpperCase()], { palette: "cream", cell: cell * 0.45, depth: 3, day: 1 });
  line.add(stage.makeRim([COPY.ending.title.toUpperCase()], { cell: cell * 0.45, width: 0.3, day: 1 })); // (it stands on the bright sky)
  line.position.copy(anchor).add(new Vector3(0, 0, -cell * 4.4));
  stage.scene.add(line);
  let shownFor: string | null | undefined; // (undefined: nothing built yet)
  let address: Mesh[] = [];
  const name = (who: string | null) => {
    if (who === shownFor) return;
    shownFor = who;
    for (const mesh of address) {
      stage.scene.remove(mesh);
      for (const part of [mesh, ...(mesh.children as Mesh[])]) part.geometry.dispose();
    }
    // The visitor's name in cream, the host in gold, as the meadow's address was; a placeholder until they pick one.
    const parts: [string, string][] = [[(who ?? "yourname").toUpperCase(), "cream"], [host, "gold"]];
    const face = stage.data.glyphs.block;
    const width = (text: string) => [...text].reduce((sum, ch) => sum + (face.glyphs[ch]?.[0]?.length ?? 0) + 1, -1);
    const cells = width(parts[0][0]) + width(host) + 1;
    const fit = Math.min((tall ? 70 : 150) / cells, 1); // (a long name is set smaller, to fit)
    let x = -(cells * cell * fit) / 2;
    address = parts.map(([text, palette]) => {
      const mesh = stage.makeType([text], { palette, cell: cell * fit, depth: 4, day: 1 });
      mesh.add(stage.makeRim([text], { cell: cell * fit, width: 0.2, day: 1 }));
      const w = width(text) * cell * fit;
      mesh.position.set(anchor.x + x + w / 2, anchor.y, anchor.z);
      x += w + cell * fit;
      stage.scene.add(mesh);
      return mesh;
    });
  };

  // -- the book of questions --------------------------------------------------------------------
  const pages = COPY.ending.faq;
  let page = 0;
  let turned = 0; // when the page last turned
  let open = 0; // how far the book is open, 0..1
  let held = false; // a hand on it: it stays on its page
  const cards = new Map<number, Card>();
  const pageCard = (k: number) => {
    let card = cards.get(k);
    if (!card && ctx.ui) {
      const [question, answer] = pages[k];
      const [word, of] = COPY.ending.page;
      card = ctx.ui.draw.page(question, [answer], `${word} ${k + 1} ${of} ${pages.length}`, BOOK);
      cards.set(k, card);
    }
    return card ?? null;
  };
  const turn = (to: number, time: number) => {
    page = (to + pages.length) % pages.length;
    turned = time;
  };
  ctx.touch.button("book-prev", (time) => turn(page - 1, time));
  ctx.touch.button("book-next", (time) => turn(page + 1, time));
  let clicked = -Infinity;
  ctx.touch.add({
    name: "lectern",
    at: lectern,
    half: new Vector3(0.55, 0.55, 0.6),
    seen: [home, C.END + 0.01],
    tip: () => ctx.ui?.draw.tooltip(COPY.ending.book, [`${pages.length} questions`]) ?? null,
    enter: () => void (held = true),
    leave: () => void (held = false),
    click: (time) => {
      clicked = time;
      turn(page + 1, time);
    },
  });

  let arrows: Card[][] | null = null;

  tl.during(home - 0.05, C.END + DUSK + 0.02, ({ s, time, dt }) => {
    const there = s >= home;
    name(ctx.visitor);
    const shown = there ? POP(clamp01((s - home - 0.04) / 0.12)) : 0;
    for (const mesh of [...address, line]) grow(mesh, shown);

    const ui = ctx.ui;
    if (!ui) return;
    // The book: open while the camera rests by the lectern, or for a while after a click on it.
    // As the credits come in it goes with the scroll, so the roll never rises under it; it is
    // still wanted until their night is full, so the claim card stays where it was (./credits.ts).
    const resting = s > 6.5;
    const wanted = there && s < C.END + DUSK && (resting || time - clicked < 6);
    open = wanted ? Math.min(open + dt / 0.25, 1) : Math.max(open - dt / 0.2, 0);
    if (!wanted) turned = time;
    if (wanted && !held && time - turned > PAGE) turn(page + 1, time);
    const alpha = open * (1 - clamp01((s - C.END) / LEAVE));
    if (!tall) ui.aside.left = Math.max(ui.aside.left, open);
    const card = alpha > 0 ? pageCard(page) : null;
    if (card) {
      // Along the bottom, over the grass: at the right in the wide frame (the claim card makes
      // way for it, to the left), in the middle of the tall one (above the claim card, if it is
      // open). Its buttons stand on the chat's field while that is open.
      const x = tall ? (ui.hud.width - card.width) / 2 : ui.hud.width - 12 - card.width;
      const y = (tall ? Math.max(28, ui.cardTop + 26) : 28) + ui.foot;
      ui.extras.push({ card, x, y, anchor: [0, 0], alpha });
      arrows ??= ["<", ">"].map((label) => [false, true].map((hover) => ui.draw.button(null, label, "stone", hover)));
      (["book-prev", "book-next"] as const).forEach((press, k) => {
        const button = arrows![k][ctx.touch.pressing === press ? 1 : 0];
        ui.extras.push({ card: button, x: k ? x + card.width - button.width : x, y: y - button.height - 2, anchor: [0, 0], alpha, press: alpha > 0.9 ? press : undefined });
      });
    }
  });

}
