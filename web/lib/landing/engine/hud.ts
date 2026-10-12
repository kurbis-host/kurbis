/**
 * The interface: chat, toasts, tooltips and buttons in the game's own manner, drawn on
 * a second canvas over the film. It is laid out in interface pixels, each a whole
 * number of screen pixels, so the pixel type stays crisp whatever the screen. The
 * frame's long side is about 480 of them, as in the game at a large interface scale.
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
/** The height of a line of chat. */
export const LINE = 10;
export const TOAST: readonly [number, number] = [160, 32];

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

  /** One line with its drop shadow, its top left at (x, y): it is a pixel wider and taller than the letters. */
  private write(into: Pixels, text: string, x: number, y: number, color: Rgb) {
    const shadow: Rgba = [color[0] * 0.25, color[1] * 0.25, color[2] * 0.25, 1];
    const ink: Rgba = [color[0], color[1], color[2], 1];
    for (const [dx, dy, paint] of [[1, 1, shadow], [0, 0, ink]] as const) {
      let at = x;
      for (const ch of text) {
        const glyph = this.face.glyphs[ch] ?? this.face.glyphs["?"];
        glyph.forEach((row, r) => {
          for (let c = 0; c < row.length; c += 1) if (row[c] === "#") into.set(at + c + dx, y + r + dy, paint);
        });
        at += glyph[0].length + 1;
      }
    }
  }

  /** One line of chat: the text on a see-through dark bar. */
  chat(text: string, color: Rgb = WHITE): Card {
    const pixels = new Pixels(this.measure(text) + 7, LINE, [0, 0, 0, 0.5]);
    this.write(pixels, text, 3, 1, color);
    return pixels.card();
  }

  /**
   * A tooltip: a title and grey lines under it on the game's dark, purple-edged panel.
   * Without a title (the thing has its name beside it already), the lines alone, in white.
   */
  tooltip(title: string | null, lines: readonly string[] = []): Card {
    const rows: [string, Rgb][] = [...(title ? [[title, WHITE] as [string, Rgb]] : []), ...lines.map((line) => [line, title ? GREY : WHITE] as [string, Rgb])];
    const width = Math.max(...rows.map(([text]) => this.measure(text) + 1)) + 8;
    const height = rows.length * LINE + 6 + (lines.length && title ? 2 : 0);
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

  /** A toast, 160 by 32: an icon, a yellow title and a white line, on the game's card if we have it. */
  toast(icon: CanvasImageSource | null, title: string, body: string, back: CanvasImageSource | null): Card {
    const pixels = new Pixels(TOAST[0], TOAST[1], back ? [0, 0, 0, 0] : [0.13, 0.13, 0.13, 1]);
    if (!back) pixels.frame(0, 0, TOAST[0], TOAST[1], [0.33, 0.33, 0.33, 1]);
    const words = new Pixels(TOAST[0], TOAST[1]);
    this.write(words, title, 30, 7, YELLOW);
    this.write(words, body, 30, 18, WHITE);
    const card = pixels.card([...(back ? [[back, 0, 0] as [CanvasImageSource, number, number]] : []), ...(icon ? [[icon, 8, 8] as [CanvasImageSource, number, number]] : []), [words.card().image, 0, 0]]);
    return card;
  }

  /** A small button: an icon and a label on the game's grey. */
  button(icon: CanvasImageSource | null, label: string): Card {
    const room = icon ? 24 : 9;
    const width = this.measure(label) + 1 + room + 6;
    const pixels = new Pixels(width, 20, [0.42, 0.42, 0.42, 1]);
    pixels.frame(0, 0, width, 20, [0, 0, 0, 1]);
    pixels.rect(1, 1, width - 2, 1, [0.66, 0.66, 0.66, 1]); // lit from the top left
    pixels.rect(1, 1, 1, 18, [0.66, 0.66, 0.66, 1]);
    pixels.rect(1, 18, width - 2, 1, [0.3, 0.3, 0.3, 1]);
    pixels.rect(width - 2, 1, 1, 18, [0.3, 0.3, 0.3, 1]);
    this.write(pixels, label, room, 6, WHITE);
    return pixels.card(icon ? [[icon, 4, 2]] : []);
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

  constructor(readonly canvas: HTMLCanvasElement) {
    this.context = canvas.getContext("2d");
  }

  resize(width: number, height: number, ratio: number) {
    const w = Math.round(width * ratio);
    const h = Math.round(height * ratio);
    this.scale = Math.max(1, Math.round(Math.max(w, h) / 480));
    this.canvas.width = w;
    this.canvas.height = h;
    this.width = w / this.scale;
    this.height = h / this.scale;
    this.drawn = true; // whatever was there is gone
  }

  begin() {
    this.pressable.length = 0;
    if (!this.context || !this.drawn) return;
    this.context.clearRect(0, 0, this.canvas.width, this.canvas.height);
    this.drawn = false;
  }

  /**
   * Draw a card with its `anchor` (0..1 across and up) at (x, y), counted from the
   * frame's bottom left as the game counts. `inside` keeps that many pixels of margin
   * to every edge. Returns where its bottom left came to lie.
   */
  place(card: Card, x: number, y: number, options: { anchor?: readonly [number, number]; alpha?: number; inside?: number; size?: number; press?: string } = {}): [number, number] {
    const { anchor = [0, 0], alpha = 1, inside, size = 1, press } = options;
    let left = x - anchor[0] * card.width;
    let bottom = y - anchor[1] * card.height;
    if (inside !== undefined) {
      left = Math.min(Math.max(left, inside), this.width - inside - card.width);
      bottom = Math.min(Math.max(bottom, inside), this.height - inside - card.height);
    }
    if (!this.context || alpha <= 0.004) return [left, bottom];
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

/** A picture for the interface, as it comes (pixel art: never smoothed). */
export async function loadIcon(path: string): Promise<ImageBitmap | null> {
  try {
    const response = await fetch(path);
    if (!response.ok) return null;
    return await createImageBitmap(await response.blob());
  } catch {
    return null;
  }
}
