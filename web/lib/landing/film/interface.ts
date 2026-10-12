/**
 * The interface over the film: chat that belongs to a place, a toast for each discovery,
 * and the shortcut to the end. What is said and shown is scripted by the scroll and by
 * what the visitor does; how it is drawn is the engine's (engine/hud.ts).
 */

import { clamp01, easeIn, easeOut } from "../anim/easing";
import type { Frame, Trigger } from "../anim/timeline";
import { BASE } from "../engine/bundle";
import { Draw, GREY, Hud, LINE, TOAST, WHITE, YELLOW, loadIcon, type Card, type Rgb } from "../engine/hud";
import type { Ctx } from "./context";

/** How many lines of chat show at once. */
const CHAT_LINES = 4;
/** Seconds a toast takes to slide in, and to slide out. */
const TRAVEL = 0.3;

interface Line {
  card: Card;
  /** The moment it arrived, or null while it has not. */
  begun: number | null;
  stay: number;
  /** The stretch of scroll it belongs to: outside it, it goes. */
  from: number;
  until: number;
  /** Scripted by the scroll: it arrives `delay` seconds after the trigger. */
  script?: { trigger: Trigger; delay: number };
  /** How much of it is there, eased. */
  shown: number;
  /** Whether it is in its place and time now. */
  here?: boolean;
}

interface Toast {
  card: Card;
  start: number;
  stay: number;
  until: number;
  /** The moment it was sent in, and the moment it was sent off. */
  shownAt: number | null;
  leftAt: number | null;
}

export interface Tip {
  card: Card;
  /** Where it points, in interface pixels from the bottom left, and which point of the card lies there. */
  at: [number, number];
  anchor: readonly [number, number];
  alpha: number;
}

export interface Interface {
  hud: Hud;
  draw: Draw;
  /** A line of chat, now. It goes when the scroll leaves from..until. */
  say(text: string, color: Rgb, time: number, options: { stay?: number; from?: number; until?: number }): void;
  /** Tooltips to draw this frame (whoever owns one adds it each frame it shows). */
  tips: Tip[];
  /** Every line of chat, said and to be said (for looking into it). */
  lines: readonly { begun: number | null; stay: number; from: number; until: number; shown: number }[];
  /** A card to draw this frame, pressable under `press`. */
  extras: { card: Card; x: number; y: number; anchor: readonly [number, number]; alpha: number; size?: number; press?: string }[];
  /** How much of the shortcut button shows (the film fades it once you are there). */
  update(frame: Frame): void;
}

export async function setInterface(ctx: Ctx, canvas: HTMLCanvasElement): Promise<Interface> {
  const { tl, C, stage, tall } = ctx;
  const hud = new Hud(canvas);
  const draw = new Draw(stage.data.glyphs.pixel);
  const icon = async (key: string) => (stage.data.hud?.[key] ? loadIcon(`${BASE}/${stage.data.hud[key]}`) : null);
  const [back, pearl, ...icons] = await Promise.all([icon("toast"), icon("pearl"), icon("grass"), icon("dirt"), icon("cave"), icon("deeper")]);

  // -- chat ---------------------------------------------------------------------------
  const lines: Line[] = [];
  const scripted = (trigger: Trigger, delay: number, text: string, color: Rgb, stay: number, from: number, until: number) => {
    lines.push({ card: draw.chat(text, color), begun: null, stay, from, until, script: { trigger, delay }, shown: 0 });
  };
  // A friend asks for the address. It starts as the pumpkin lands and runs in real time.
  const landed = tl.trigger(1.12, 0.4);
  for (const [delay, text] of [[0.5, "<Steve> what's the ip"], [1.9, "<You> steve.kurbis.host"], [3.3, "<Steve> that's it?"], [4.4, "<You> that's it"]] as const) {
    scripted(landed, delay, text, WHITE, 6.5, 1.0, 1.62);
  }
  // The cave's comings and goings. Arriving, it is the pumpkin that joins first; the scroll plays it.
  for (const [delay, text] of [[0.15, "Kurbis joined the game"], [1.0, "Steve joined the game"], [1.35, "Alex joined the game"]] as const) {
    scripted(tl.trigger(C.WAKE + delay / C.ARRIVAL_RATE, 0.2), 0, text, YELLOW, 6.0, C.WAKE, 3.62);
  }

  // -- toasts ---------------------------------------------------------------------------
  const toasts: Toast[] = (
    [[1.0, "Touched grass", 99], [2.3, "Dirt enthusiast", 99], [C.WAKE + 0.03, "Found the cave", 3.62], [C.PORTAL_LIT + 0.02, "We need to go deeper", 5.5]] as const
  ).map(([start, body, until], i) => ({ card: draw.toast(icons[i], "Advancement made!", body, back), start, stay: 2.6, until, shownAt: null, leftAt: null }));
  // (each is gone before the next place's headline wants its corner)

  // -- the shortcut to the end ------------------------------------------------------------
  const shortcut = draw.button(pearl, "Jump to portal");

  const tips: Tip[] = [];
  const extras: Interface["extras"] = [];
  let before = 0;

  return {
    hud,
    draw,
    tips,
    extras,
    lines,
    say(text, color, time, { stay = 6, from = -Infinity, until = Infinity }) {
      lines.push({ card: draw.chat(text, color), begun: time, stay, from, until, shown: 0 });
      if (lines.length > 40) lines.splice(0, lines.findIndex((line) => !line.script) + 1 || 1);
    },
    update({ s, time, dt, jumped }) {
      hud.begin();
      const base = tall ? 28 : 6; // the narrow frame has no room for chat beside the shortcut: it sits above it

      // Chat: each line pushes the older ones up, and fades after a while or when its place is left.
      for (const line of lines) {
        if (line.script) {
          const since = line.script.trigger.t - line.script.delay;
          line.begun = since > 0 ? time - since : null;
        }
        // Talk belongs to its place: a line said elsewhere, or long ago, is not there (and takes no room).
        line.here = line.begun !== null && time >= line.begun && s >= line.from && s < line.until && time - line.begun < line.stay + 0.5;
        line.shown = line.here ? Math.min(line.shown + dt / 0.1, 1) : Math.max(line.shown - dt / 0.3, 0);
      }
      for (const line of lines) {
        if (line.begun === null || line.shown <= 0) continue;
        const age = time - line.begun;
        const later: number[] = [];
        let risen = 0;
        for (const other of lines) {
          if (other.begun === null || other.begun <= line.begun || !(other.here || other.shown > 0)) continue;
          later.push(other.begun);
          risen += easeOut((time - other.begun) / 0.12, 2);
        }
        const newer = later.length;
        // Crowded out by the line that made it one too many.
        const crowded = newer >= CHAT_LINES ? later.sort((a, b) => a - b)[CHAT_LINES - 1] : Infinity;
        let alpha = line.shown * (1 - clamp01((age - line.stay) / 0.5));
        if (crowded < Infinity) alpha *= 1 - clamp01((time - crowded) / 0.2);
        hud.place(line.card, 4, base + LINE * risen, { alpha });
      }

      // Toasts, one at a time: each leaves when its time is up, when the next one is due, or when its place is left.
      for (const toast of toasts) {
        if (!jumped && before < toast.start && s >= toast.start) {
          for (const other of toasts) if (other.shownAt !== null && other.leftAt === null) other.leftAt = time;
          toast.shownAt = time;
          toast.leftAt = null;
        }
        if (toast.shownAt === null) continue;
        const age = time - toast.shownAt;
        if (toast.leftAt === null && (age >= TRAVEL + toast.stay || s >= toast.until || s < toast.start)) toast.leftAt = time;
        let away = 1 - easeOut(age / TRAVEL, 3);
        if (toast.leftAt !== null) {
          if (time - toast.leftAt > TRAVEL) {
            toast.shownAt = null;
            continue;
          }
          const reached = 1 - easeOut((toast.leftAt - toast.shownAt) / TRAVEL, 3); // how far out it still was when sent off
          away = reached + (1 - reached) * easeIn((time - toast.leftAt) / TRAVEL, 3);
        }
        hud.place(toast.card, hud.width - 4 - TOAST[0] + (TOAST[0] + 16) * away, hud.height - 4, { anchor: [0, 1] });
      }
      before = s;

      // The shortcut: not needed once you are there.
      hud.place(shortcut, hud.width - 5, 6, { anchor: [1, 0], alpha: 1 - clamp01((s - 5.45) / 0.15), press: "shortcut" });

      for (const tip of tips) hud.place(tip.card, tip.at[0], tip.at[1], { anchor: tip.anchor, alpha: tip.alpha, inside: 4 });
      for (const extra of extras) hud.place(extra.card, extra.x, extra.y, extra);
      tips.length = 0;
      extras.length = 0;
    },
  };
}

export { GREY, WHITE, YELLOW };
