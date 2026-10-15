/**
 * The credits. After home's last view the frame goes dark, as the game's does when its
 * credits roll after the End, and a few faint stars drift on it; the page's own story
 * rolls up it, moved by the scroll (and back down, scrolled back). First the Kurbis mark
 * and name; each heading in the game's yellow, what it says in white, the last line in
 * grey; and the links in it can be followed. Where the roll comes to rest, "The End"
 * arrives as an advancement: "Or the beginning?". Scrolled on, the frame goes under the
 * film's cover, and the page comes round under it (./ring.ts): onto the sky, where the
 * pumpkin opens its eyes for a moment (./valley.ts). Scrolled up from the sky, the same
 * backwards: the cover comes over the sky, and lifts on the end of the credits, where
 * "The End" arrives again.
 *
 * All of it is the interface's, laid out in its pixels for the width of the frame.
 */

import { clamp01, lerp, seeded } from "../anim/easing";
import { GREY, LINE, TOAST, WHITE, YELLOW, fineCard, type Card, type Draw } from "../engine/hud";
import { COPY } from "../copy";
import type { Ctx } from "./context";

/** Screens of scroll the roll takes, from home's last view to where it rests; and from there, past "The End", to the cover. */
export const ROLL = 1.3;
export const AGAIN = 0.3;
/** And on the other side of the cover, before the sky, where it lifts (the lead-in: ./ring.ts). */
export const LEAD = 0.3;
/** How much of each, next to where the page comes round, the cover hides all of the frame. */
export const HOLD = 0.025;
/** How far into the roll the frame has gone dark. */
export const DUSK = 0.12;
/** The widest a line may be, and the room it keeps to the sides of a narrow frame. */
const COLUMN = 240;
const SIDES = 16;
/** How high over the frame's foot the last line comes to rest. */
const FLOOR = 26;
/**
 * The bar from the frame's top, with the gap under it, and the toasts' corner under that
 * (./interface.ts). The roll fades out over a few pixels on its way under the bar; and where
 * a toast would lie on it, under the toast's row. So it never runs over the bar, and a toast
 * never sits on letters.
 */
const BAR = 27;
const CORNER = 4;
const FADING = 12;
/** How far apart the lines of a paragraph are, the grey ones, and a row of links (as tall as a button). */
const PITCH = 12;
const QUIET = 11;
const ROW = 20;

/** What the roll says, from its top. */
export type Piece =
  | { kind: "mark" }
  | { kind: "heading" | "said" | "quiet"; text: string }
  | { kind: "links"; links: readonly (readonly [string, string])[] };

/** The roll, top to bottom. The page's own HTML says the same, in the same order (copy.test.ts holds the two together). */
export function roll(): Piece[] {
  const { credits, footer } = COPY;
  return [
    { kind: "mark" },
    ...credits.parts.flatMap(([heading, said]): Piece[] => [{ kind: "heading", text: heading.toUpperCase() }, ...said.map((text): Piece => ({ kind: "said", text }))]),
    { kind: "heading", text: credits.community.toUpperCase() },
    { kind: "links", links: credits.links },
    { kind: "links", links: footer.links },
    { kind: "quiet", text: footer.disclaimer },
  ];
}

/** The room before a piece of the roll, by what comes before it. */
function room(kind: Piece["kind"], last: Piece["kind"] | null): number {
  if (last === null) return 0;
  if (last === "mark") return 34;
  if (kind === "heading") return 30;
  if (last === "heading") return kind === "links" ? 2 : 6; // (a link's card has room over its letters already)
  return kind === "links" || last === "links" ? 4 : 10;
}

interface Laid {
  /** The frame's width it was laid out for, and how long the roll is. */
  width: number;
  height: number;
  /** Each card: how far down the roll its top is, and its left from the frame's middle. A link has two (as it rests, and under the pointer) and is pressed by its name. */
  cards: { card: Card | readonly Card[]; down: number; left: number; press?: string }[];
}

/** The roll laid out for a frame `width` across: broken into even lines as narrow as the frame needs. */
function lay(draw: Draw, width: number, mark: Card | null): Laid {
  const column = Math.min(COLUMN, width - 2 * SIDES);
  const cards: Laid["cards"] = [];
  const centred = (card: Card, down: number) => cards.push({ card, down, left: -Math.floor(card.width / 2) });
  let down = 0;
  let last: Piece["kind"] | null = null;
  let links = 0;
  for (const piece of roll()) {
    down += room(piece.kind, last);
    last = piece.kind;
    if (piece.kind === "mark") {
      // The mark, and the name beside it three times the interface's size, its letters level with the mark's middle.
      const name = draw.centred(["kurbis"]);
      const big = fineCard(name.image, name.width * 3, name.height * 3, 1);
      const across = (mark ? 32 + 7 : 0) + big.width;
      if (mark) cards.push({ card: mark, down, left: -Math.floor(across / 2) });
      cards.push({ card: big, down: down + 2, left: across - Math.floor(across / 2) - big.width });
      down += 32;
    } else if (piece.kind === "heading") {
      centred(draw.centred([piece.text], YELLOW), down);
      down += LINE;
    } else if (piece.kind === "links") {
      // On one line, a dot between each, if they fit; else one to a line. (The dot is a full stop, raised to the letters' middle.)
      const pairs = piece.links.map(([label]) => [draw.link(label), draw.link(label, true)] as const);
      const dot = draw.centred([" . "], GREY);
      const across = pairs.reduce((sum, [card]) => sum + card.width, 0) + (pairs.length - 1) * dot.width;
      if (across <= column) {
        let left = -Math.floor(across / 2);
        pairs.forEach((pair, k) => {
          if (k > 0) {
            cards.push({ card: dot, down: down + 2, left });
            left += dot.width;
          }
          cards.push({ card: pair, down, left, press: `credits_${links + k}` });
          left += pair[0].width;
        });
        down += ROW;
      } else
        pairs.forEach((pair, k) => {
          cards.push({ card: pair, down, left: -Math.floor(pair[0].width / 2), press: `credits_${links + k}` });
          down += ROW;
        });
      links += pairs.length;
    } else {
      const quiet = piece.kind === "quiet";
      for (const line of draw.balanced(piece.text, column)) {
        centred(draw.centred([line], quiet ? GREY : WHITE), down);
        down += quiet ? QUIET : PITCH;
      }
    }
  }
  return { width, height: down, cards };
}

interface Star {
  x: number;
  y: number;
  size: 1 | 2;
  light: number;
  /** Seconds a twinkle takes, where in it the star starts, and how many interface pixels a second it drifts. */
  period: number;
  phase: number;
  drift: number;
}

/** A few faint stars for a frame this big, always the same ones. */
function sky(width: number, height: number): Star[] {
  const random = seeded(11);
  return Array.from({ length: Math.round((width * height) / 7000) }, () => {
    const big = random() < 0.16;
    return {
      x: random() * width,
      y: random() * height,
      size: big ? 2 : 1,
      light: big ? lerp(0.4, 0.6, random()) : lerp(0.18, 0.5, random()),
      period: lerp(3, 7, random()),
      phase: random(),
      drift: lerp(0.6, 1.6, random()),
    };
  });
}

export interface Credits {
  /** How dark the frame is at a scroll position: 1 is the credits' night, with nothing of the world showing. */
  dark(s: number): number;
}

export function setCredits(ctx: Ctx): Credits {
  const { tl, C } = ctx;
  const rolled = C.END + ROLL;
  const dark = (s: number) => clamp01((s - C.END) / DUSK);

  // The links in it: Pumpkin's places, and the page's own last three (Kurbis on GitHub, and the legal pages). Those on other sites open in a tab of their own.
  [...COPY.credits.links, ...COPY.footer.links].forEach(([, href], k) => ctx.touch.button(`credits_${k}`, () => (href.startsWith("/") ? ctx.go(href) : ctx.open(href))));

  let laid: Laid | null = null;
  let stars: { for: string; list: Star[]; cards: [Card, Card] } | null = null;
  let before = 0;
  // Come round to it from the sky, scrolled up past it (./ring.ts): until the cover begins to lift.
  let back = false;
  tl.during(C.END - 0.01, Infinity, ({ s, time, jumped }) => {
    const ui = ctx.ui;
    const was = before;
    before = s;
    if (jumped) back = ctx.again === time && s >= rolled;
    if (!ui) return;
    const { hud, draw } = ui;
    const night = dark(s);
    ui.night = Math.max(ui.night, night);
    ui.aside.hidden = Math.max(ui.aside.hidden, night); // (the claim card is put away: the credits have the frame)
    if (night <= 0) return;

    if (laid?.width !== hud.width) laid = lay(draw, hud.width, ui.mark ? fineCard(ui.mark.image, 32, 32, 1) : null);
    // How far the roll's top has risen over the frame's foot: from there at home's last view, to where its last line rests.
    const rise = clamp01((s - C.END) / ROLL) * (laid.height + FLOOR);

    // The stars drift on in real time, and rise a little with the roll, far behind it.
    const size = `${hud.width}x${hud.height}`;
    if (stars?.for !== size) stars = { for: size, list: sky(hud.width, hud.height), cards: [draw.square(1), draw.square(2)] };
    for (const star of stars.list) {
      const x = (((star.x - star.drift * time) % hud.width) + hud.width) % hud.width;
      const y = (star.y + 0.08 * rise) % hud.height;
      const twinkle = 0.8 + 0.2 * Math.sin(2 * Math.PI * (time / star.period + star.phase));
      ui.under.push({ card: stars.cards[star.size - 1], x: Math.round(x), y: Math.round(y), anchor: [0, 0], alpha: night * star.light * twinkle });
    }

    // The roll: what is under the frame's foot is not there yet, and it fades out on its way under
    // the bar (or under the toasts' row, where a toast would lie on it).
    const middle = hud.width / 2;
    const corner = hud.width - CORNER - TOAST[0];
    for (const { card, down, left, press } of laid.cards) {
      const top = rise - down;
      const shown = Array.isArray(card) ? card[ctx.touch.pressing === press ? 1 : 0] : (card as Card);
      const ceiling = hud.height - BAR - (middle + left + shown.width > corner ? TOAST[1] : 0);
      const alpha = night * clamp01((ceiling - top) / FADING);
      if (top <= 0 || alpha <= 0) continue;
      ui.under.push({ card: shown, x: middle + left, y: top - shown.height, anchor: [0, 0], alpha, press: alpha > 0.9 ? press : undefined });
    }

    // Come to rest: the game's advancement for the End, and what it says under its name; or
    // come round to from the sky, as the cover begins to lift off it.
    if (!jumped && ((was < rolled && s >= rolled) || (back && ui.cover < 1))) {
      back = false;
      const { end, beginning } = COPY.toasts.events;
      ui.toast(end, "end", time, { from: C.END + DUSK, until: Infinity, again: true, note: beginning });
    }
  });

  return { dark };
}
