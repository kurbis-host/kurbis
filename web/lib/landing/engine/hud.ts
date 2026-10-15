/**
 * The interface: chat, toasts, tooltips and buttons in the game's own manner, drawn on
 * a second canvas over the film. It is laid out in interface pixels, each a whole
 * number of screen pixels, so the pixel type stays crisp whatever the screen. On a
 * phone its short side is about 234 of them; on a desktop each is about two CSS pixels
 * (a little more on a big screen), as the game's interface at its normal scale.
 *
 * Cards are drawn once, pixel by pixel, and then only placed and faded.
 */

import type { Face } from "./bundle";

export type Rgb = readonly [number, number, number];
type Rgba = readonly [number, number, number, number];

export const WHITE: Rgb = [1, 1, 1];
export const YELLOW: Rgb = [1, 1, 0.33];
export const GREY: Rgb = [0.66, 0.66, 0.66];
export const GREEN: Rgb = [0.33, 1, 0.33];
/** The yellow a button's label turns under the pointer, in the game. */
const LIT: Rgb = [1, 1, 0.63];
/** A button's greys (fill, lit edge, shaded edge): the game's stone, or the green of a way in. */
const TONES = {
  stone: [[0.42, 0.42, 0.42, 1], [0.66, 0.66, 0.66, 1], [0.3, 0.3, 0.3, 1]],
  green: [[0.2, 0.5, 0.14, 1], [0.38, 0.69, 0.27, 1], [0.11, 0.31, 0.07, 1]],
  /** A button that cannot be pressed yet, as the game greys one out: flat and dark. */
  off: [[0.17, 0.17, 0.17, 1], [0.17, 0.17, 0.17, 1], [0.17, 0.17, 0.17, 1]],
} as const satisfies Record<string, readonly [Rgba, Rgba, Rgba]>;
export type Tone = keyof typeof TONES;
/** The grey of a greyed-out button's label. */
const OFF: Rgb = [0.63, 0.63, 0.63];

/** The height of a line of chat. */
export const LINE = 10;
/** The debug screen's (F3): a line's height, its letters' light grey, and the see-through grey strip each lies on. */
export const DEBUG_LINE = 9;
const DEBUG_INK: Rgb = [0.878, 0.878, 0.878];
const DEBUG_STRIP: Rgba = [0.314, 0.314, 0.314, 0.565];

/** A stretch of a line in a style of its own, as the game's chat styles one: a colour, bold, underlined. */
export interface Span {
  text: string;
  color?: Rgb;
  bold?: boolean;
  underline?: boolean;
}
/** A line: plain words in one colour, or spans. */
export type Words = string | readonly Span[];
export const TOAST: readonly [number, number] = [160, 32];
/** How far a speech bubble's tail hangs under its box. */
export const BUBBLE_TAIL = 4;

/** A drawn piece of interface: its pixels, and its size in interface pixels. */
export interface Card {
  image: HTMLCanvasElement;
  width: number;
  height: number;
}

class Pixels {
  readonly data: Uint8ClampedArray;

  constructor(
    readonly width: number,
    readonly height: number,
    fill: Rgba = [0, 0, 0, 0],
  ) {
    this.data = new Uint8ClampedArray(width * height * 4);
    if (fill[3] > 0) this.rect(0, 0, width, height, fill);
  }

  /** Row 0 is the top row. */
  set(x: number, y: number, [r, g, b, a]: Rgba) {
    if (x < 0 || y < 0 || x >= this.width || y >= this.height) return;
    const i = (y * this.width + x) * 4;
    this.data[i] = r * 255;
    this.data[i + 1] = g * 255;
    this.data[i + 2] = b * 255;
    this.data[i + 3] = a * 255;
  }

  rect(x: number, y: number, width: number, height: number, color: Rgba) {
    for (let row = y; row < y + height; row += 1) for (let column = x; column < x + width; column += 1) this.set(column, row, color);
  }

  frame(x: number, y: number, width: number, height: number, color: Rgba) {
    this.rect(x, y, width, 1, color);
    this.rect(x, y + height - 1, width, 1, color);
    this.rect(x, y, 1, height, color);
    this.rect(x + width - 1, y, 1, height, color);
  }

  card(over: readonly [CanvasImageSource, number, number][] = []): Card {
    const image = document.createElement("canvas");
    image.width = this.width;
    image.height = this.height;
    const context = image.getContext("2d");
    if (context) {
      context.putImageData(new ImageData(this.data as Uint8ClampedArray<ArrayBuffer>, this.width, this.height), 0, 0);
      context.imageSmoothingEnabled = false;
      for (const [picture, x, y] of over) context.drawImage(picture, x, y);
    }
    return { image, width: this.width, height: this.height };
  }
}

/** What draws the cards: the small pixel face, and the game's panels. */
export class Draw {
  constructor(private readonly face: Face) {}

  /** How wide a line is, without its shadow. */
  measure(text: string): number {
    let width = 0;
    for (const ch of text) width += (this.face.glyphs[ch] ?? this.face.glyphs["?"])[0].length + 1;
    return Math.max(width - 1, 0);
  }

  /** How wide a line of spans is, as `measure` (a bold letter is a pixel wider, as the game draws it). */
  widthOf(words: Words): number {
    if (typeof words === "string") return this.measure(words);
    let width = 0;
    for (const span of words) width += this.measure(span.text) + (span.text ? 1 : 0) + (span.bold ? [...span.text].length : 0);
    return Math.max(width - 1, 0);
  }

  /** One line with its drop shadow, its top left at (x, y): it is a pixel wider and taller than the letters. */
  private write(into: Pixels, text: Words, x: number, y: number, color: Rgb, shadowed = true) {
    const spans: readonly Span[] = typeof text === "string" ? [{ text }] : text;
    for (const pass of shadowed ? [1, 0] : [0]) {
      let at = x;
      for (const span of spans) {
        const tone = span.color ?? color;
        const paint: Rgba = pass ? [tone[0] * 0.25, tone[1] * 0.25, tone[2] * 0.25, 1] : [tone[0], tone[1], tone[2], 1];
        const from = at;
        for (const ch of span.text) {
          const glyph = this.face.glyphs[ch] ?? this.face.glyphs["?"];
          // (Bold, as the game has it: the letter again a pixel to the right.)
          for (const bold of span.bold ? [0, 1] : [0])
            glyph.forEach((row, r) => {
              for (let c = 0; c < row.length; c += 1) if (row[c] === "#") into.set(at + c + bold + pass, y + r + pass, paint);
            });
          at += glyph[0].length + 1 + (span.bold ? 1 : 0);
        }
        // Underlined, as the game underlines a link: the row under the letters' tails, from a pixel before them.
        if (span.underline) into.rect(from - 1 + pass, y + 8 + pass, at - from + 1, 1, paint);
      }
    }
  }

  /** One line of chat: the text on a see-through dark bar. */
  chat(text: Words, color: Rgb = WHITE): Card {
    const pixels = new Pixels(this.widthOf(text) + 7, LINE, [0, 0, 0, 0.5]);
    this.write(pixels, text, 3, 1, color);
    return pixels.card();
  }

  /** One line with its shadow on nothing, as tall as a line of chat (its letters a pixel down from its top). */
  line(text: Words, color: Rgb = WHITE): Card {
    const pixels = new Pixels(this.widthOf(text) + 1, LINE);
    this.write(pixels, text, 0, 1, color);
    return pixels.card();
  }

  /** A box of one colour, see-through as `color` says: what the game lays under its chat field and its lists. */
  box(width: number, height: number, color: Rgba): Card {
    return new Pixels(Math.max(1, Math.round(width)), Math.max(1, Math.round(height)), color).card();
  }

  /**
   * A line broken into lines no wider than `width`, as the game's chat wraps what is said:
   * at a space where it can (the space goes), and inside a word too long for a line of its
   * own. Each span keeps its style across the break.
   */
  breakLine(text: Words, width: number): Span[][] {
    const spans: readonly Span[] = typeof text === "string" ? [{ text }] : text;
    const chars = spans.flatMap((span) => [...span.text].map((ch) => ({ ch, span })));
    const advance = (ch: string, span: Span) => (this.face.glyphs[ch] ?? this.face.glyphs["?"])[0].length + 1 + (span.bold ? 1 : 0);
    const broken: (typeof chars)[] = [];
    let line: typeof chars = [];
    let x = 0;
    for (const char of chars) {
      const step = advance(char.ch, char.span);
      if (line.length && x + step - 1 > width) {
        // (At the space it is: the line breaks there. Else at the last space before it, or right here.)
        const space = char.ch === " " ? line.length : line.map((c) => c.ch).lastIndexOf(" ");
        if (space > 0) {
          broken.push(line.slice(0, space));
          line = line.slice(space + 1);
        } else {
          broken.push(line);
          line = [];
        }
        x = line.reduce((sum, c) => sum + advance(c.ch, c.span), 0);
        if (char.ch === " " && !line.length) continue; // (a line does not start with the space it broke at)
      }
      line.push(char);
      x += step;
    }
    broken.push(line);
    // Back into spans: the letters of each line that share a style, together.
    return broken.map((letters) => {
      const out: { of: Span; span: Span }[] = [];
      for (const { ch, span } of letters) {
        const last = out[out.length - 1];
        if (last?.of === span) last.span.text += ch;
        else out.push({ of: span, span: { ...span, text: ch } });
      }
      return out.length ? out.map(({ span }) => span) : [{ text: "" }];
    });
  }

  /**
   * A column of the debug screen (F3), as the game draws it: each line in light grey without
   * a shadow, on a see-through grey strip just wider than its letters, the strips one under
   * the next (an empty line leaves a gap). For the right-hand column, they line up on the right.
   */
  debug(lines: readonly Words[], right = false): Card {
    const strips = lines.map((line) => (this.widthOf(line) > 0 ? this.widthOf(line) + 3 : 0));
    const width = Math.max(1, ...strips);
    // (A pixel more at the foot, for an underline under the last line: it lies on the next line's strip.)
    const pixels = new Pixels(width, lines.length * DEBUG_LINE + 1);
    strips.forEach((strip, i) => strip && pixels.rect(right ? width - strip : 0, i * DEBUG_LINE, strip, DEBUG_LINE, DEBUG_STRIP));
    lines.forEach((line, i) => strips[i] && this.write(pixels, line, (right ? width - strips[i] : 0) + 1, i * DEBUG_LINE + 1, DEBUG_INK, false));
    return pixels.card();
  }

  /** Words broken into lines no wider than `width` (a word wider than that has a line of its own). */
  wrap(text: string, width: number): string[] {
    const lines: string[] = [];
    for (const word of text.split(" ")) {
      const last = lines[lines.length - 1];
      if (last !== undefined && this.measure(`${last} ${word}`) + 1 <= width) lines[lines.length - 1] = `${last} ${word}`;
      else lines.push(word);
    }
    return lines;
  }

  /** Words broken into as few lines as `wrap` makes of them in `width`, but as even as they will go. */
  balanced(text: string, width: number): string[] {
    const count = this.wrap(text, width).length;
    let [low, high] = [1, width];
    while (low < high) {
      const middle = Math.floor((low + high) / 2);
      if (this.wrap(text, middle).length <= count) high = middle;
      else low = middle + 1;
    }
    return this.wrap(text, low);
  }

  /** Lines with their shadow on nothing, each centred over the next: the action bar's message, as the game draws it. */
  centred(lines: readonly string[], color: Rgb = WHITE): Card {
    const width = Math.max(...lines.map((line) => this.measure(line))) + 1;
    const pixels = new Pixels(width, lines.length * LINE);
    lines.forEach((line, i) => this.write(pixels, line, Math.floor((width - 1 - this.measure(line)) / 2), 1 + i * LINE, color));
    return pixels.card();
  }

  /** The tooltip's dark, purple-edged panel, empty, for what is placed on it. */
  panel(width: number, height: number): Card {
    const pixels = new Pixels(width, height, [0.063, 0, 0.063, 0.94]);
    pixels.frame(1, 1, width - 2, height - 2, [0.16, 0, 0.5, 1]);
    for (const [x, y] of [[0, 0], [width - 1, 0], [0, height - 1], [width - 1, height - 1]]) pixels.set(x, y, [0, 0, 0, 0]);
    return pixels.card();
  }

  /**
   * A tooltip: a title and grey lines under it on the game's dark, purple-edged panel.
   * Without a title (the thing has its name beside it already), the lines alone, in white.
   */
  tooltip(title: string | null, lines: readonly string[] = []): Card {
    return this.panelled(title, lines);
  }

  /**
   * A tooltip with a link on a row of its own at its foot: the tooltip, with room kept for
   * the link; and the link to lay on it, as it rests and under the pointer (`link`), with
   * its top left there, counted from the tooltip's top left.
   */
  tooltipWithLink(title: string, lines: readonly string[], link: string): { card: Card; link: { card: readonly [Card, Card]; x: number; y: number } } {
    // (Its letters a little lower than a line's would be, so the link's card, as tall as a button, keeps off the line over it.)
    const card = this.panelled(title, lines, this.measure(link) + 1, LINE + 6);
    return { card, link: { card: [this.link(link), this.link(link, true)], x: 4, y: card.height - 19 } };
  }

  /** A tooltip's panel and words, `room` wide and `foot` tall kept free under the last line. */
  private panelled(title: string | null, lines: readonly string[], room = 0, foot = 0): Card {
    const rows: [string, Rgb][] = [...(title ? [[title, WHITE] as [string, Rgb]] : []), ...lines.map((line) => [line, title ? GREY : WHITE] as [string, Rgb])];
    const width = Math.max(room, ...rows.map(([text]) => this.measure(text) + 1)) + 8;
    const height = rows.length * LINE + 6 + (lines.length && title ? 2 : 0) + foot;
    const pixels = new Pixels(width, height, [0.063, 0, 0.063, 0.94]);
    pixels.frame(1, 1, width - 2, height - 2, [0.16, 0, 0.5, 1]);
    for (const [x, y] of [[0, 0], [width - 1, 0], [0, height - 1], [width - 1, height - 1]]) pixels.set(x, y, [0, 0, 0, 0]);
    let y = 5;
    rows.forEach(([text, color], i) => {
      this.write(pixels, text, 4, y, color);
      y += LINE + (i === 0 && title ? 2 : 0);
    });
    return pixels.card();
  }

  /**
   * A toast, 160 by 32: an icon, a yellow title and a white line (or one in `color`), on the
   * game's card if we have it. The icon is 16 pixels square; one drawn finer than that (a model, which the
   * game draws at the screen's own pixels: the shield) lies in the same square, and the
   * card is drawn that much finer to hold it.
   */
  toast(icon: CanvasImageSource | null, title: string, body: string, back: CanvasImageSource | null, color: Rgb = WHITE): Card {
    const pixels = new Pixels(TOAST[0], TOAST[1], back ? [0, 0, 0, 0] : [0.13, 0.13, 0.13, 1]);
    if (!back) pixels.frame(0, 0, TOAST[0], TOAST[1], [0.33, 0.33, 0.33, 1]);
    const words = new Pixels(TOAST[0], TOAST[1]);
    this.write(words, title, 30, 7, YELLOW);
    this.write(words, body, 30, 18, color);
    const fine = icon && "width" in icon && typeof icon.width === "number" ? Math.max(Math.round(icon.width / 16), 1) : 1;
    const card = pixels.card([...(back ? [[back, 0, 0] as [CanvasImageSource, number, number]] : []), ...(icon && fine === 1 ? [[icon, 8, 8] as [CanvasImageSource, number, number]] : []), [words.card().image, 0, 0]]);
    if (!icon || fine === 1) return card;
    const finer = fineCard(card.image, TOAST[0], TOAST[1], fine);
    finer.image.getContext("2d")?.drawImage(icon, 8 * fine, 8 * fine);
    return finer;
  }

  /**
   * A small button: an icon and a label on the game's grey (or green). Under the pointer,
   * as in the game, it brightens and its edge turns white. Given a `width` (the game's
   * screens have theirs 200 across), the label sits in its middle. Greyed out ("off"), its
   * label is grey.
   */
  button(icon: CanvasImageSource | null, label: string, tone: Tone = "stone", hover = false, width?: number): Card {
    const room = width ? Math.floor((width - this.measure(label)) / 2) : icon ? 24 : 8;
    const across = width ?? this.measure(label) + room + 8; // (as much room after the letters as before them)
    const [fill, lit, shade] = TONES[tone].map(([r, g, b, a]): Rgba => (hover ? [Math.min(r * 1.25, 1), Math.min(g * 1.25, 1), Math.min(b * 1.25, 1), a] : [r, g, b, a]));
    const pixels = new Pixels(across, 20, fill);
    pixels.frame(0, 0, across, 20, hover ? [1, 1, 1, 1] : [0, 0, 0, 1]);
    pixels.rect(1, 1, across - 2, 1, lit); // lit from the top left
    pixels.rect(1, 1, 1, 18, lit);
    pixels.rect(1, 18, across - 2, 1, shade);
    pixels.rect(across - 2, 1, 1, 18, shade);
    this.write(pixels, label, room, 6, tone === "off" ? OFF : WHITE);
    return pixels.card(icon ? [[icon, 4, 2]] : []);
  }

  /**
   * A square button with a cross on it, to close what it sits on: the cross drawn
   * rather than typed, so it sits in the middle (a letter x sits low in its line).
   */
  close(tone: Tone = "stone", hover = false): Card {
    const size = 16;
    const [fill, lit, shade] = TONES[tone].map(([r, g, b, a]): Rgba => (hover ? [Math.min(r * 1.25, 1), Math.min(g * 1.25, 1), Math.min(b * 1.25, 1), a] : [r, g, b, a]));
    const pixels = new Pixels(size, size, fill);
    pixels.frame(0, 0, size, size, hover ? [1, 1, 1, 1] : [0, 0, 0, 1]);
    pixels.rect(1, 1, size - 2, 1, lit);
    pixels.rect(1, 1, 1, size - 2, lit);
    pixels.rect(1, size - 2, size - 2, 1, shade);
    pixels.rect(size - 2, 1, 1, size - 2, shade);
    // Five pixels each way, its shadow a pixel down and right: the two together centred.
    const arm = 5;
    const from = Math.floor((size - arm - 1) / 2);
    for (const [d, paint] of [[1, [0.25, 0.25, 0.25, 1]], [0, [1, 1, 1, 1]]] as const)
      for (let i = 0; i < arm; i += 1) {
        pixels.set(from + i + d, from + i + d, paint);
        pixels.set(from + arm - 1 - i + d, from + i + d, paint);
      }
    return pixels.card([]);
  }

  /** A player's nametag: the name in white and a word after it in grey, on the game's see-through bar. */
  nametag(name: string, note: string): Card {
    const gap = note ? this.measure(" ") + 1 : 0;
    const pixels = new Pixels(this.measure(name) + gap + this.measure(note) + 6, LINE, [0, 0, 0, 0.45]); // (darker than the game's: it may hang in the sun)
    this.write(pixels, name, 3, 1, WHITE);
    this.write(pixels, note, 3 + this.measure(name) + gap, 1, GREY);
    return pixels.card();
  }

  /**
   * A speech bubble: the nametag's see-through box with a line or two in white, and a
   * small tail pointing to whoever says it: under the box (down to them), or out of one
   * side (to a head beside it). `tip` is where along that edge its point is: the column,
   * or the row from the top. Under the box it leans toward an end (-1 left, 1 right) when
   * the speaker is there.
   */
  bubble(lines: readonly string[], tail: "under" | "left" | "right", tip: number, lean: -1 | 0 | 1 = 0): Card {
    const width = Math.max(...lines.map((line) => this.measure(line))) + 6;
    const height = lines.length * LINE;
    const fill: Rgba = [0, 0, 0, 0.45]; // (as the nametag's)
    const under = tail === "under";
    const pixels = new Pixels(width + (under ? 0 : BUBBLE_TAIL), height + (under ? BUBBLE_TAIL : 0));
    const x = tail === "left" ? BUBBLE_TAIL : 0;
    pixels.rect(x, 0, width, height, fill);
    lines.forEach((line, i) => this.write(pixels, line, x + 3, 1 + i * LINE, WHITE));
    for (let k = 0; k < BUBBLE_TAIL; k += 1) {
      const spread = BUBBLE_TAIL - 1 - k; // narrowing to the point, `k` out from the box
      if (under) {
        const from = lean === -1 ? tip : tip - spread;
        const to = lean === 1 ? tip : tip + spread;
        pixels.rect(from, height + k, to - from + 1, 1, fill);
      } else pixels.rect(tail === "left" ? BUBBLE_TAIL - 1 - k : width + k, tip - spread, 1, 2 * spread + 1, fill);
    }
    return pixels.card();
  }

  /**
   * A page of a book, as the game shows one: lines of ink without shadows on paper, the
   * first (the question) in the book's red-brown, its page number at the foot.
   */
  page(title: string, lines: readonly string[], foot: string, width: number): Card {
    const inner = width - 16;
    const body = lines.flatMap((line) => this.wrap(line, inner));
    const heading = this.wrap(title, inner);
    const height = 12 + heading.length * LINE + 4 + body.length * LINE + 6 + LINE + 6;
    const pixels = new Pixels(width, height, [0.95, 0.9, 0.77, 1]);
    pixels.frame(0, 0, width, height, [0.36, 0.23, 0.12, 1]);
    pixels.frame(2, 2, width - 4, height - 4, [0.82, 0.72, 0.55, 1]);
    let y = 10;
    for (const line of heading) {
      this.write(pixels, line, 8, y, [0.45, 0.12, 0.06], false);
      y += LINE;
    }
    y += 4;
    for (const line of body) {
      this.write(pixels, line, 8, y, [0.12, 0.09, 0.06], false);
      y += LINE;
    }
    this.write(pixels, foot, width - 8 - this.measure(foot), height - LINE - 4, [0.45, 0.36, 0.26], false);
    return pixels.card();
  }

  /** Words alone, with their shadow, as tall as a button so they line up beside one. */
  words(text: string, color: Rgb = WHITE): Card {
    const pixels = new Pixels(this.measure(text) + 1, 20);
    this.write(pixels, text, 0, 6, color);
    return pixels.card();
  }

  /**
   * A link: words with a line under them, as the game marks what can be clicked in its
   * chat. Under the pointer they turn the yellow of a button's label. As tall as a button,
   * for a finger to find.
   */
  link(text: string, hover = false): Card {
    const color = hover ? LIT : WHITE;
    const width = this.measure(text);
    const pixels = new Pixels(width + 1, 20);
    this.write(pixels, text, 0, 5, color);
    // (Under the letters that hang below the line, with its shadow.)
    pixels.rect(1, 15, width, 1, [color[0] * 0.25, color[1] * 0.25, color[2] * 0.25, 1]);
    pixels.rect(0, 14, width, 1, [color[0], color[1], color[2], 1]);
    return pixels.card();
  }

  /** A square of one colour, `size` pixels across: a star. */
  square(size: number, color: Rgb = WHITE): Card {
    return new Pixels(size, size, [color[0], color[1], color[2], 1]).card();
  }
}

/** The canvas the interface is drawn on, afresh each frame something of it shows. */
export class Hud {
  /** Size in interface pixels, and how many screen pixels one of them is. */
  width = 480;
  height = 270;
  scale = 1;
  private readonly context: CanvasRenderingContext2D | null;
  private drawn = false;
  /** What can be pressed, as drawn this frame: [name, x, y, width, height] from the top left. */
  private readonly pressable: [string, number, number, number, number][] = [];
  /** Everything drawn so far this frame, as [left, bottom, right, top] (for what must keep off it: film/speech.ts). */
  readonly placed: [number, number, number, number][] = [];
  /**
   * While set, what is placed or shaded is not drawn, and cannot be pressed: the visitor has
   * put the interface away (F1). Where it would go is still worked out. Washes are drawn.
   */
  hidden = false;

  constructor(readonly canvas: HTMLCanvasElement) {
    this.context = canvas.getContext("2d");
  }

  resize(width: number, height: number, ratio: number) {
    const w = Math.round(width * ratio);
    const h = Math.round(height * ratio);
    // CSS pixels to one interface pixel: by the short side on a phone (it must fill the
    // width); by the long side on a desktop, where a phone's share would make it huge.
    const narrow = Math.min(width, height) < 600;
    const unit = narrow ? Math.min(width, height) / 234 : Math.min(Math.max(Math.max(width, height) / 800, 1.25), 3);
    this.scale = Math.max(1, Math.round(unit * ratio));
    this.canvas.width = w;
    this.canvas.height = h;
    this.width = w / this.scale;
    this.height = h / this.scale;
    this.drawn = true; // whatever was there is gone
  }

  begin() {
    this.pressable.length = 0;
    this.placed.length = 0;
    if (!this.context || !this.drawn) return;
    this.context.clearRect(0, 0, this.canvas.width, this.canvas.height);
    this.drawn = false;
  }

  /** Where `place` would put a card's bottom left, without drawing it. */
  where(card: Card, x: number, y: number, { anchor = [0, 0], inside }: { anchor?: readonly [number, number]; inside?: number } = {}): [number, number] {
    let left = x - anchor[0] * card.width;
    let bottom = y - anchor[1] * card.height;
    if (inside !== undefined) {
      left = Math.min(Math.max(left, inside), this.width - inside - card.width);
      bottom = Math.min(Math.max(bottom, inside), this.height - inside - card.height);
    }
    return [left, bottom];
  }

  /**
   * Draw a card with its `anchor` (0..1 across and up) at (x, y), counted from the
   * frame's bottom left as the game counts. `inside` keeps that many pixels of margin
   * to every edge. Returns where its bottom left came to lie.
   */
  place(card: Card, x: number, y: number, options: { anchor?: readonly [number, number]; alpha?: number; inside?: number; size?: number; press?: string } = {}): [number, number] {
    const { alpha = 1, size = 1, press } = options;
    const [left, bottom] = this.where(card, x, y, options);
    if (!this.context || alpha <= 0.004 || this.hidden) return [left, bottom];
    this.placed.push([left, bottom, left + card.width, bottom + card.height]);
    const { scale } = this;
    const w = card.width * size;
    const h = card.height * size;
    const px = Math.round((left + (card.width - w) / 2) * scale);
    const py = Math.round((this.height - bottom - card.height + (card.height - h) / 2) * scale);
    this.context.globalAlpha = Math.min(alpha, 1);
    this.context.imageSmoothingEnabled = false;
    this.context.drawImage(card.image, px, py, Math.round(w * scale), Math.round(h * scale));
    this.drawn = true;
    if (press && alpha > 0.5) this.pressable.push([press, left, this.height - bottom - card.height, card.width, card.height]);
    return [left, bottom];
  }

  /** Darken the top `depth` interface pixels of the frame, most at the edge, so what sits there reads on any sky. */
  shade(depth: number, alpha: number) {
    if (!this.context || this.hidden) return;
    const height = depth * this.scale;
    const gradient = this.context.createLinearGradient(0, 0, 0, height);
    gradient.addColorStop(0, `rgba(0, 0, 0, ${alpha})`);
    gradient.addColorStop(1, "rgba(0, 0, 0, 0)");
    this.context.globalAlpha = 1;
    this.context.fillStyle = gradient;
    this.context.fillRect(0, 0, this.canvas.width, height);
    this.drawn = true;
  }

  /**
   * Lay colours over the whole frame, `alpha` of the way: `stops` say where each is (0 at the
   * top, 1 at the bottom), and how see-through each is of its own, if it is (the death screen's red).
   */
  wash(stops: readonly (readonly [number, Rgb, number?])[], alpha: number) {
    if (!this.context || alpha <= 0.004) return;
    const gradient = this.context.createLinearGradient(0, 0, 0, this.canvas.height);
    for (const [at, [r, g, b], a = 1] of stops) gradient.addColorStop(at, `rgba(${Math.round(r * 255)}, ${Math.round(g * 255)}, ${Math.round(b * 255)}, ${a})`);
    this.context.globalAlpha = Math.min(alpha, 1);
    this.context.fillStyle = gradient;
    this.context.fillRect(0, 0, this.canvas.width, this.canvas.height);
    this.drawn = true;
  }

  /**
   * Darken a patch of the frame softly, its edges fading over about `feather` interface
   * pixels, so letters laid on it read on anything (a glowing pumpkin, a torch). Counted
   * from the bottom left, as `place`.
   */
  soften(left: number, bottom: number, width: number, height: number, alpha: number, feather = 4) {
    if (!this.context || alpha <= 0.004 || this.hidden) return;
    const { context, scale } = this;
    // The patch is drawn off the canvas, and only its blurred shadow falls on it.
    const away = this.canvas.width + feather * scale * 4;
    context.save();
    context.globalAlpha = 1;
    context.shadowColor = `rgba(0, 0, 0, ${Math.min(alpha, 1)})`;
    context.shadowBlur = feather * scale;
    context.shadowOffsetX = away;
    context.fillStyle = "#000";
    context.fillRect(Math.round(left * scale) - away, Math.round((this.height - bottom - height) * scale), Math.round(width * scale), Math.round(height * scale));
    context.restore();
    this.drawn = true;
  }

  /** Nothing placed so far can be pressed: a screen of the game's has come over it (the death screen). */
  cover() {
    this.pressable.length = 0;
  }

  /** What, if anything, is under a point of the page (in CSS pixels from the canvas's top left). */
  under(x: number, y: number, ratio: number): string | null {
    const ix = (x * ratio) / this.scale;
    const iy = (y * ratio) / this.scale;
    for (let i = this.pressable.length - 1; i >= 0; i -= 1) {
      const [name, left, top, width, height] = this.pressable[i];
      if (ix >= left && ix < left + width && iy >= top && iy < top + height) return name;
    }
    return null;
  }
}

/**
 * A picture drawn finer than the interface's pixels: `width` by `height` of them, from a
 * picture with `density` times as many (the brand's mark, which is 32 pixels to our 16).
 */
export function fineCard(picture: CanvasImageSource, width: number, height: number, density: number): Card {
  const image = document.createElement("canvas");
  image.width = width * density;
  image.height = height * density;
  const context = image.getContext("2d");
  if (context) {
    context.imageSmoothingEnabled = false;
    context.drawImage(picture, 0, 0, image.width, image.height);
  }
  return { image, width, height };
}

/** A picture for the interface, as it comes (pixel art: never smoothed). The interface does without one that will not decode. */
export async function loadIcon(picture: Blob): Promise<ImageBitmap | null> {
  try {
    return await createImageBitmap(picture);
  } catch {
    return null;
  }
}
