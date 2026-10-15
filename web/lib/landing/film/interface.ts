/**
 * The interface over the film: a bar along the top with the ways in, chat that belongs to
 * a place, and a toast for each discovery; and the game's chat screen, which "/" opens, and
 * its death screen (./chat.ts). What is said and shown is scripted by the scroll and by what
 * the visitor does; how it is drawn is the engine's (engine/hud.ts).
 */

import type { Object3D } from "three";
import { clamp01, easeOut } from "../anim/easing";
import type { Frame, Trigger } from "../anim/timeline";
import { Draw, GREEN, GREY, Hud, LINE, TOAST, WHITE, YELLOW, fineCard, loadIcon, type Card, type Rgb, type Span, type Words } from "../engine/hud";
import { COPY } from "../copy";
import { HINTS, youOf, type Chat, type Dead } from "./chat";
import { RED as WRONG, fill, parse } from "./commands";
import type { Ctx } from "./context";
import { Toasts } from "./toasts";

/** How many lines of chat show at once. */
const CHAT_LINES = 4;
/** And while the chat is open, as many as the game's shows (as many again of the visitor's own, for a while after it has closed). */
const OPEN_LINES = 10;
/** The chat's field, as the game's: a see-through bar this tall, this far over the frame's foot and in from its sides. */
const FIELD = 12;
const FOOT = 2;
/** Its letters' light grey, the faint grey of the rest of a suggestion after them, its cursor's grey, and the yellow of the suggestion picked (all the game's). */
const INK: Rgb = [0.878, 0.878, 0.878];
const FAINT: Rgb = [0.5, 0.5, 0.5];
const CARET = [0.816, 0.816, 0.816, 1] as const;
const PICKED: Rgb = [1, 1, 0];
/** A row of the suggestions (or of what a command takes), and the dark under them. */
const ROW = 12;
const UNDER = [0, 0, 0, 0.816] as const;
/** The death screen's red, from the top of the frame down: see-through at the top, less so at the foot (the game's). */
const DEATH: readonly [number, Rgb, number][] = [[0, [0.314, 0, 0], 0.376], [1, [0.502, 0.188, 0.188], 0.627]];
/** What is known of the name in the claim card: still being checked; free; taken; the visitor's own; not a name at all. */
export type Status = "checking" | "free" | "taken" | "mine" | "short" | "dash";
const RED: Rgb = [1, 0.4, 0.35];

/** The bar along the top: its margin to the frame's edges, the room between its pieces, and how tall it is. */
const MARGIN = 4;
const GAP = 3;
const BAR = 20;
/** The bottom of the action bar's last line: its letters 64 to 72 pixels over the frame's bottom, where the game's are (over the hotbar and the hearts). */
const ACTION_BAR = 63;
/** How long the action bar's message stays, and fades (the game's 3 s, the last of them fading). */
const ACTION_STAYS = 2;
const ACTION_FADES = 1;
/** The credits' night, from the top of the frame down: deep plum, to the page's own dark at the foot. */
const NIGHT: readonly [number, Rgb][] = [[0, [0.035, 0.024, 0.063]], [1, [0.106, 0.063, 0.125]]];
/** The film's cover: the sunset the page shows while the film is on its way (.landing-loading in app/globals.css). */
const COVER: readonly [number, Rgb][] = [[0, [0.165, 0.082, 0.208]], [0.48, [0.478, 0.227, 0.302]], [0.78, [1, 0.69, 0.38]], [1, [0.851, 0.467, 0.173]]];

interface Line {
  card: Card;
  /** The moment it arrived, or null while it has not. */
  begun: number | null;
  stay: number;
  /** The stretch of scroll it belongs to: outside it, it goes. */
  from: number;
  until: number;
  /** Said by the visitor's chat (./chat.ts), which shows more of them at once, as the game's does. */
  own?: boolean;
  /** Scripted by the scroll: it arrives `delay` seconds after the trigger, unless `hush` says not to (by when it would). */
  script?: { trigger: Trigger; delay: number; hush?: (begun: number) => boolean };
  /** How much of it is there, eased. */
  shown: number;
  /** Whether it is in its place and time now. */
  here?: boolean;
}

export interface Tip {
  card: Card;
  /** Where it points, in interface pixels from the bottom left, and which point of the card lies there. */
  at: [number, number];
  anchor: readonly [number, number];
  alpha: number;
  /** Whose nametag it is (the art's rig), for a speech bubble to stand over (./speech.ts). */
  of?: string;
  /** A link on it, pressed by its name: as it rests and under the pointer, its top left this far from the tooltip's. */
  link?: { card: readonly [Card, Card]; x: number; y: number; press: string };
  /** Where it came to lie, link and all, as [left, bottom, right, top] (set as it is drawn: the hand looks for it there, ./touch.ts). */
  drawn?: readonly [number, number, number, number];
}

export interface Interface {
  hud: Hud;
  draw: Draw;
  /** The brand's mark as the bar shows it (16 interface pixels square, its 32 pixels drawn finer), if it loaded. */
  mark: Card | null;
  /**
   * A line of chat, now (in one colour, or in spans of their own). It goes when the scroll
   * leaves from..until. `own`: the visitor's chat said it (./chat.ts).
   */
  say(text: Words, color: Rgb, time: number, options: { stay?: number; from?: number; until?: number; own?: boolean }): void;
  /**
   * The interface is put away (F1, as in the game): the bar, chat, the action bar, toasts,
   * tooltips and nametags, speech bubbles, the claim card and the debug screen are not drawn,
   * and nothing of them can be pressed. What the film tells in the interface's manner (the
   * extras: the book, the market) stays, and so do the credits and the film's covers.
   */
  hidden: boolean;
  /** The debug screen's columns (F3: ./debug.ts), under the bar on the left and the right; null while it is off. */
  debug: { left: Card | null; right: Card | null };
  /**
   * A toast for something that happened, now: "Advancement made!" and `title` (from
   * COPY.toasts.events), with the pack's picture under `icon` (a key of HUD in the art's
   * scripts/export_web.py). It joins the scrolled toasts, one at a time: it sends off the
   * one showing, and goes when its time is up or when the scroll leaves from..until (which
   * ends before the next place's headline wants the corner). A title shows once a visit,
   * unless `again`; and not at all if the scroll is outside its stretch. Given headlines
   * (`under`: the place's, and the next place's), it comes in under the one in sight where
   * the corner would put it on its letters (in a narrow frame, a headline at the top
   * reaches across it); and on its way out, it is gone at once if one comes over it. Given
   * a `note` (the advancement's description), the card says the title and that, in grey.
   */
  toast(title: string, icon: string, time: number, options: { from: number; until: number; again?: boolean; under?: readonly (Object3D | null)[]; note?: string }): void;
  /**
   * A message on the action bar, now: white letters over where the game's hotbar would be,
   * in the middle near the bottom, as the game says what cannot be done ("You may not rest
   * now; there are monsters nearby") or how to get out of a cart ("Press Left Shift to
   * Dismount"). One at a time: a new one takes the place of the one showing. As in the game
   * it stays 2 s and fades over the next; it also goes when the scroll leaves from..until,
   * and does not come if the scroll is outside it. It stands clear of the chat and of the
   * claim card (tooltips step off it), on a soft shade so it reads on anything; and where
   * the frame is too narrow for one line (the game's never is) it breaks into even ones.
   */
  actionBar(text: string, time: number, options: { from: number; until: number }): void;
  /** Tooltips to draw this frame (whoever owns one adds it each frame it shows). */
  tips: Tip[];
  /** Every line of chat, said and to be said (for looking into it). */
  lines: readonly { begun: number | null; stay: number; from: number; until: number; shown: number }[];
  /** A card to draw this frame, pressable under `press`. */
  extras: { card: Card; x: number; y: number; anchor: readonly [number, number]; alpha: number; size?: number; press?: string }[];
  /**
   * What lies under all the rest this frame (whoever needs it sets it each frame): `night`,
   * how far the frame has gone dark (0..1), and the cards on that dark (the credits).
   */
  under: Interface["extras"];
  night: number;
  /** How much of the frame the film's cover hides this frame, the interface and all (0..1). */
  cover: number;
  /**
   * Where the claim card should keep out of the way this frame (whoever needs its place
   * sets it each frame): `left`, how far over to the frame's left it goes (0..1, a wide
   * frame only); `hidden`, how far it is put away (0..1).
   */
  aside: { left: number; hidden: number };
  /** The top of the claim card as last drawn, in interface pixels from the bottom (0 when none shows). */
  readonly cardTop: number;
  /** And where it lay, as [left, bottom, right, top] (null when none shows). */
  readonly cardBox: readonly [number, number, number, number] | null;
  /** How far the chat's field reaches up from the foot of the frame, while it is open (0 while it is not): what stands on the foot stands on it. */
  readonly foot: number;
  /** The name the claim card holds, while it is open. */
  readonly claimed: string | null;
  /** Open the claim card for a free name; close it; say the address was copied. */
  claim(name: string, time: number, status: Status): void;
  unclaim(time: number): void;
  copied(time: number): void;
  /** The visitor's own name, for the chat to give as the address (or none: the film's own). */
  named(name: string | null): void;
  /** Who the friends are has changed (the visitor took one's place, or gave it back): the chat follows. */
  renamed(): void;
  update(frame: Frame): void;
}

export async function setInterface(ctx: Ctx, canvas: HTMLCanvasElement): Promise<Interface> {
  const { tl, C, stage, tall } = ctx;
  const hud = new Hud(canvas);
  const draw = new Draw(stage.data.glyphs.pixel);
  const host = COPY.surface.host;
  const icon = async (key: string) => (stage.data.hud?.[key] && stage.pack.has(stage.data.hud[key]) ? loadIcon(stage.pack.picture(stage.data.hud[key])) : null);
  // Every picture the art gave the interface, by its key: the toast's card, and the icons for toasts.
  const keys = Object.keys(stage.data.hud ?? {}).filter((key) => key !== "toast");
  const [back, mark, ...pictures] = await Promise.all([icon("toast"), brandMark(), ...keys.map(icon)]);
  const icons = new Map(keys.map((key, i) => [key, pictures[i]]));

  // -- chat ---------------------------------------------------------------------------
  const lines: Line[] = [];
  const scripted = (trigger: Trigger, delay: number, text: string, color: Rgb, stay: number, from: number, until: number, hush?: (begun: number) => boolean) => {
    lines.push({ card: draw.chat(text, color), begun: null, stay, from, until, script: { trigger, delay, hush }, shown: 0 });
  };
  // A friend asks for the address. It starts as the pumpkin lands and runs in real time.
  const landed = tl.trigger(1.12, 0.4);
  // (Each line is written from its template again whenever who is who changes.)
  const talk = (template: string) =>
    template
      .replace("{friend}", ctx.wearer === "Steve" ? ctx.players.Alex : ctx.players.Steve)
      .replace("{you}", youOf(ctx))
      .replace("{address}", `${ctx.visitor ?? "steve"}${host}`);
  const talks = [0.5, 1.9, 3.3, 4.4, 5.6].map((delay, i) => {
    scripted(landed, delay, talk(COPY.surface.chat[i]), WHITE, 6.5, 1.0, 1.62);
    return [lines[lines.length - 1], COPY.surface.chat[i]] as const;
  });
  // The cave's comings and goings. Arriving, it is the pumpkin that joins first; the scroll plays it.
  // Once the lever has been pulled the lever tells who comes and goes: an arrival the scroll plays again after that is not said.
  const pulled = (begun: number) => {
    const at = ctx.cave?.pulledAt();
    return at != null && begun >= at;
  };
  const woke = (name: string | null) => `${name ? `${name}${host}` : COPY.cave.server} started`;
  scripted(tl.trigger(C.WAKE + 0.02 / C.ARRIVAL_RATE, 0.2), 0, woke(null), GREY, 6.0, C.WAKE, 3.62, pulled);
  const waking = lines[lines.length - 1];
  scripted(tl.trigger(C.WAKE + 0.15 / C.ARRIVAL_RATE, 0.2), 0, "Kurbis joined the game", YELLOW, 6.0, C.WAKE, 3.62, pulled);
  const joins = (["Steve", "Alex"] as const).map((friend, i) => {
    scripted(tl.trigger(C.WAKE + [1.0, 1.35][i] / C.ARRIVAL_RATE, 0.2), 0, `${ctx.players[friend]} joined the game`, YELLOW, 6.0, C.WAKE, 3.62, pulled);
    return [lines[lines.length - 1], friend] as const;
  });
  // The creeper's blast at the homestead, as the game prints a death. The scroll says it as
  // it passes the blast, and it goes as time is turned back.
  const [, blast, turnBack] = C.REWIND;
  scripted(tl.trigger(blast, 0.2), 0, COPY.homestead.rewind.boom, WHITE, 8.0, blast, turnBack);

  // -- toasts ---------------------------------------------------------------------------
  const toastCard = (title: string, key: string, note?: string) =>
    note ? draw.toast(icons.get(key) ?? null, title, note, back, GREY) : draw.toast(icons.get(key) ?? null, "Advancement made!", title, back);
  // (Each with the headline it keeps off, if it was given one: that, the stretch it belongs to, and where it last put it.)
  const toasts = new Toasts<{ card: Card; keep?: { under: readonly (Object3D | null)[]; from: number; until: number; top?: number; gone?: boolean } }>(
    ([["grass", 1.0, 99], ["dirt", 2.3, 99], ["cave", C.WAKE + 0.03, 3.62], ["deeper", C.PORTAL_LIT + 0.02, 5.5]] as const).map(([key, from, until], i) => ({ card: { card: toastCard(COPY.toasts.scrolled[i], key) }, from, until })),
  );
  // (each is gone before the next place's headline wants its corner)

  // -- the action bar: one message at a time, in the middle near the bottom ----------------
  // (`left`: when the scroll left its stretch. It is drawn again for each width of frame.)
  let message: { text: string; at: number; from: number; until: number; left: number | null; card: Card | null; room: number } | null = null;

  // -- the bar along the top: who we are, and the two ways in -----------------------------
  // Each button is drawn twice: as it rests, and under the pointer.
  const bar = {
    mark: mark ? fineCard(mark, 16, 16, 2) : null,
    name: draw.words("kurbis"),
    signIn: [false, true].map((hover) => draw.button(null, COPY.bar.signIn, "stone", hover)),
    get: [false, true].map((hover) => draw.button(null, COPY.bar.get, "green", hover)),
    home: [false, true].map((hover) => draw.button(null, COPY.bar.home, "green", hover)),
  };

  // -- the claim card: a free name, to copy into the game or to sign in with ---------------
  let claim: { name: string; status: Status; at: number; copiedAt: number | null; closedAt: number | null; built: Built | null } | null = null;
  interface Built {
    /** The frame width it was laid out for. */
    room: number;
    panel: Card;
    width: number;
    height: number;
    /** Each piece with its place on the panel (from its top left), pressable under its name. */
    pieces: { card: Card | Card[]; x: number; y: number; press?: string; when?: "before" | "after" }[];
  }
  const PAD = 6;
  const ICON = 22; // the icon's width, and room after it
  const build = (name: string, status: Status, room: number): Built => {
    const ours = status === "free" || status === "mine";
    const width = Math.min(room, 236);
    const inner = width - 2 * PAD;
    const close = [false, true].map((hover) => draw.close("stone", hover));
    // The address on one line if it fits beside the mark and the close button, else broken before the host.
    const beside = inner - ICON - close[0].width - GAP;
    const address = `${name}${host}`;
    const fit = (text: string) => {
      let t = text;
      while (t.length > 1 && draw.measure(t) + 1 > beside) t = t.slice(0, -1);
      return t === text ? t : `${t.slice(0, -2)}..`;
    };
    const title = (draw.measure(address) + 1 <= beside ? [address] : [fit(name), host]).map((text) => draw.words(text, YELLOW));
    // What it says follows what is known of the name as it stands.
    const [text, color]: [string, Rgb] =
      status === "free" ? [COPY.claim.said, WHITE] : status === "mine" ? [COPY.claim.home, WHITE] : status === "checking" ? [COPY.claim.checking, GREY] : [COPY.claim[status], RED];
    const said = draw.wrap(text, inner).map((line) => draw.words(line, color));
    const copied = draw.wrap(COPY.claim.copied, inner).map((text) => draw.words(text, WHITE));
    const copy = [false, true].map((hover) => draw.button(null, COPY.claim.copy, "green", hover));
    const done = [false, true].map((hover) => draw.button(null, COPY.claim.done, "green", hover));
    const signIn = [false, true].map((hover) => draw.button(null, status === "mine" ? COPY.claim.panel : COPY.claim.signIn, "stone", hover));
    const pieces: Built["pieces"] = [];
    let y = PAD;
    // The visitor's head, once the film knows their skin; else the mark.
    const icon = ctx.face ?? bar.mark;
    if (icon) pieces.push({ card: icon, x: PAD, y: y + 2 });
    pieces.push({ card: close, x: width - PAD - close[0].width, y, press: "claim-close" });
    title.forEach((card, i) => pieces.push({ card, x: PAD + ICON, y: y + i * LINE }));
    y += Math.max(20, title.length * LINE + 8) + 2;
    // Their name is a Java player's too, and the film shows them in that skin. (Someone it is not changes the name.)
    if (ctx.wearer && status === "free") {
      pieces.push({ card: draw.words(COPY.claim.you, GREEN), x: PAD, y: y - 4 });
      y += LINE + 5;
    }
    const lines = ours ? Math.max(said.length, copied.length) : said.length;
    said.forEach((card, i) => pieces.push({ card, x: PAD, y: y - 6 + i * LINE, when: ours ? "before" : undefined }));
    if (ours) copied.forEach((card, i) => pieces.push({ card, x: PAD, y: y - 6 + i * LINE, when: "after" }));
    y += lines * LINE + 4;
    // Copy and sign in (or, for a visitor who is back, their panel) once the name is theirs to use.
    if (ours) {
      pieces.push({ card: copy, x: PAD, y, press: "claim-copy", when: "before" });
      pieces.push({ card: done, x: PAD, y, press: "claim-copy", when: "after" });
      pieces.push({ card: signIn, x: PAD + Math.max(copy[0].width, done[0].width) + GAP, y, press: status === "mine" ? "claim-panel" : "claim-sign-in" });
      y += 20 + PAD;
    } else y += PAD - 4;
    return { room, panel: draw.panel(width, y), width, height: y, pieces };
  };

  // -- the chat's field, while it is open (./chat.ts), as the game's chat screen draws it -------
  let strip: Card | null = null;
  let from = 0; // the first letter of a long line that is in sight (the field scrolls along it, as the game's does)
  let line: { key: string; card: Card } | null = null;
  const caret = { end: draw.line("_", INK), inside: draw.box(1, FIELD - 1, CARET) };
  const rows = new Map<number, Card>();
  const hints = new Map<string, Card>();
  let takes: { text: string; card: Card; box: Card } | null = null;
  const typed = (chat: Chat, time: number) => {
    const { field } = chat;
    const { text, cursor } = field;
    const width = Math.round(hud.width - 2 * FOOT);
    if (strip?.width !== width) strip = draw.box(width, FIELD, [0, 0, 0, 0.5]);
    hud.place(strip, FOOT, FOOT);
    // What of the line is in sight: from where the cursor can be seen, as much as there is room for.
    const x = FOOT + 2;
    const room = hud.width - 2 * x;
    from = Math.min(from, cursor);
    while (from < cursor && draw.measure(text.slice(from, cursor)) + 6 > room) from += 1;
    while (from > 0 && draw.measure(text.slice(from - 1)) + 6 <= room) from -= 1;
    let shown = text.slice(from);
    while (shown && draw.measure(shown) > room) shown = shown.slice(0, -1);
    // As the game colours what is typed: a command in grey, and red from where it stops being one; anything else in the field's own grey.
    const parsed = text.startsWith("/") ? parse(text) : null;
    const wrong = parsed?.kind === "error" && parsed.at < parsed.input.length ? parsed.at + 1 : Infinity;
    const spans: Span[] = [];
    const add = (piece: string, color: Rgb) => {
      const last = spans[spans.length - 1];
      if (last?.color === color) last.text += piece;
      else if (piece) spans.push({ text: piece, color });
    };
    [...shown].forEach((ch, i) => add(ch, !parsed ? INK : from + i >= wrong ? WRONG : GREY));
    // And after it, the rest of the suggestion picked, faint, where there is room for it.
    let rest = from + shown.length === text.length ? field.rest : "";
    while (rest && draw.measure(shown + rest) > room) rest = rest.slice(0, -1);
    add(rest, FAINT);
    const key = JSON.stringify(spans);
    if (line?.key !== key) line = { key, card: draw.line(spans.length ? spans : "") };
    hud.place(line.card, x, FOOT + 1);
    // The cursor, blinking as the game's does (0.3 s each way, from the last key): "_" at the end of the line, a bar inside it.
    if (Math.floor((time - chat.since) / 0.3) % 2 === 0) {
      const before = text.slice(from, cursor);
      const at = x + (before ? draw.measure(before) + 1 : 0);
      if (cursor === text.length) hud.place(caret.end, at, FOOT + 1);
      else hud.place(caret.inside, at - 1, FOOT);
    }
    // While none show, what the command typed takes (the game's usage), in grey on the same dark, over the field from its slash.
    const usage = field.usage;
    if (usage) {
      if (takes?.text !== usage) {
        const card = draw.line(usage, GREY);
        takes = { text: usage, card, box: draw.box(draw.measure(usage) + 3, ROW, UNDER) };
      }
      const left = Math.max(0, Math.min(x - 1, hud.width - takes.box.width));
      hud.place(takes.box, left, FOOT + FIELD + 1);
      hud.place(takes.card, left + 1, FOOT + FIELD + 2);
    }
    // The game's suggestions for what is typed, over the field from where the word they finish begins: the one picked in yellow, the rest in grey.
    if (!field.hints) return;
    const words = field.hints.words.slice(0, HINTS);
    const widest = Math.max(...words.map((word) => draw.measure(word))) + 2;
    const begins = text.slice(from, field.hints.start);
    const left = Math.max(0, Math.min(x + (field.hints.start > from && begins ? draw.measure(begins) + 1 : 0) - 1, hud.width - x - widest));
    const top = FOOT + FIELD + 1 + words.length * ROW;
    let row = rows.get(widest);
    if (!row) rows.set(widest, (row = draw.box(widest, ROW, UNDER)));
    words.forEach((word, i) => {
      const bottom = top - ROW * (i + 1);
      const picked = i === field.hints!.picked;
      const name = `${word}:${picked}`;
      let card = hints.get(name);
      if (!card) hints.set(name, (card = draw.line(word, picked ? PICKED : GREY)));
      hud.place(row, left, bottom, { press: `chat-hint-${i}` });
      hud.place(card, left + 1, bottom + 1);
    });
  };

  // -- the death screen (/kill), as the game's: over everything, its buttons pressable after a second --
  let death: { for: string; title: Card; cause: Card; score: Card; buttons: Card[][] } | null = null;
  const died = (dead: Dead, time: number) => {
    const alpha = dead.gone === null ? clamp01((time - dead.at) / 0.12) : 1 - clamp01((time - dead.gone) / 0.25);
    if (alpha <= 0) return;
    hud.cover(); // (nothing under it can be pressed)
    hud.wash(DEATH, alpha);
    const name = youOf(ctx);
    if (death?.for !== name) {
      const title = draw.line(COPY.chat.died);
      const [before, after] = COPY.chat.score.split("{score}");
      death = {
        for: name,
        title: fineCard(title.image, title.width * 2, title.height * 2, 1), // (twice the size, as the game draws it)
        cause: draw.line(fill(COPY.chat.killed, { name })),
        score: draw.line([{ text: before }, { text: "0", color: YELLOW }, { text: after }]),
        buttons: [COPY.chat.respawn, COPY.chat.title].map((label) => [draw.button(null, label, "stone", false, 200), draw.button(null, label, "stone", true, 200), draw.button(null, label, "off", false, 200)]),
      };
    }
    // Where the game puts each, counted as it counts them, from the top: the title, what killed you, the score, and the buttons.
    // (On a soft shade, as the action bar's letters are: the film may have its own type right behind them.)
    const centre = (card: Card) => Math.floor((hud.width - card.width) / 2);
    const across = Math.max(death.title.width, death.cause.width, death.score.width) + 12;
    hud.soften(Math.floor((hud.width - across) / 2), hud.height - 99 - death.score.height - 2, across, 99 + death.score.height + 2 - 56, 0.45 * alpha, 8);
    hud.place(death.title, centre(death.title), hud.height - 58, { anchor: [0, 1], alpha });
    hud.place(death.cause, centre(death.cause), hud.height - 84, { anchor: [0, 1], alpha });
    hud.place(death.score, centre(death.score), hud.height - 99, { anchor: [0, 1], alpha });
    const ready = dead.gone === null && time - dead.at >= 1;
    (["respawn", "title-screen"] as const).forEach((press, k) => {
      const lit = ready && (ctx.touch.pressing === press || dead.focus === k);
      const top = Math.floor(hud.height / 4) + 72 + 24 * k;
      hud.place(death!.buttons[k][ready ? (lit ? 1 : 0) : 2], Math.floor(hud.width / 2) - 100, hud.height - top, { anchor: [0, 1], alpha, press: ready ? press : undefined });
    });
  };

  const tips: Tip[] = [];
  const extras: Interface["extras"] = [];
  const under: Interface["extras"] = [];
  const aside = { left: 0, hidden: 0 };
  let cardTop = 0;
  let cardBox: [number, number, number, number] | null = null;
  let rise = 0; // (how far the chat's field has come in, 0..1)

  const ui: Interface = {
    hud,
    draw,
    mark: bar.mark,
    tips,
    extras,
    under,
    night: 0,
    cover: 0,
    lines,
    aside,
    hidden: false,
    debug: { left: null, right: null },
    get cardTop() {
      return cardTop;
    },
    get cardBox() {
      return cardTop > 0 ? cardBox : null;
    },
    get foot() {
      return Math.round(rise * (FOOT + FIELD));
    },
    get claimed() {
      return claim && claim.closedAt === null ? claim.name : null;
    },
    claim(name, time, status) {
      // Open already: it shows the name as it stands, where it is, without coming in again.
      if (claim && claim.closedAt === null) {
        if (claim.name === name && claim.status === status) return;
        if (claim.name !== name) claim.copiedAt = null;
        claim.name = name;
        claim.status = status;
        claim.built = null;
        return;
      }
      claim = { name, status, at: time, copiedAt: null, closedAt: null, built: null };
    },
    unclaim(time) {
      if (claim && claim.closedAt === null) claim.closedAt = time;
    },
    copied(time) {
      if (claim) claim.copiedAt = time;
    },
    named(name) {
      for (const [line, template] of talks) line.card = draw.chat(talk(template), WHITE);
      waking.card = draw.chat(woke(name), GREY);
    },
    renamed() {
      for (const [line, template] of talks) line.card = draw.chat(talk(template), WHITE);
      for (const [line, friend] of joins) line.card = draw.chat(`${ctx.players[friend]} joined the game`, YELLOW);
      if (claim) claim.built = null; // (the card says "That's you, right?" while the visitor wears a skin)
    },
    say(text, color, time, { stay = 6, from = -Infinity, until = Infinity, own = false }) {
      lines.push({ card: draw.chat(text, color), begun: time, stay, from, until, own, shown: 0 });
      // (The oldest line said goes; the scripted ones are the film's own and stay.)
      if (lines.length > 40) lines.splice(lines.findIndex((line) => !line.script), 1);
    },
    toast(title, key, time, options) {
      const { under, from, until, note } = options;
      toasts.send(title, () => ({ card: toastCard(title, key, note), keep: under && { under, from, until } }), time, options);
    },
    actionBar(text, time, { from, until }) {
      if (tl.frame.s < from || tl.frame.s >= until) return;
      message = { text, at: time, from, until, left: null, card: null, room: 0 };
    },
    update({ s, time, dt, jumped }) {
      hud.begin();
      // The dark the credits roll up, and what is on it, under all the rest.
      hud.wash(NIGHT, ui.night);
      for (const card of under) hud.place(card.card, card.x, card.y, card);
      hud.hidden = ui.hidden; // (F1: what follows is the interface, which may be put away)
      hud.shade(MARGIN + BAR + 14, 0.45); // under everything, so the bar reads on any sky

      // The debug screen (F3), a pixel in from each side and under the bar. As in the game,
      // what the interface shows of its own lies over it; the bubbles over the friends' heads
      // do not step round it (in the game they are in the world, under it).
      const kept = hud.placed.length;
      if (ui.debug.left) hud.place(ui.debug.left, 1, hud.height - MARGIN - BAR - GAP, { anchor: [0, 1] });
      if (ui.debug.right) hud.place(ui.debug.right, hud.width - 1, hud.height - MARGIN - BAR - GAP, { anchor: [1, 1] });
      hud.placed.length = kept;

      // The claim card: along the bottom of a narrow frame (the chat moves up over it), at the bottom right of a wide one.
      let base = 6;
      let carded = 0; // how much of the claim card shows
      let held: [number, number, number, number] | null = null; // and where it is: left, bottom, right, top
      const room = tall ? hud.width - 2 * MARGIN : 236;
      if (claim) {
        if (claim.built?.room !== room) claim.built = build(claim.name, claim.status, room);
        const { panel, pieces, width, height } = claim.built;
        const opened = claim.closedAt === null ? easeOut((time - claim.at) / 0.2, 3) : 1 - clamp01((time - claim.closedAt) / 0.15);
        const shown = opened * (1 - aside.hidden);
        if (opened <= 0 && claim.closedAt !== null) claim = null;
        else {
          carded = shown;
          // At the bottom right of a wide frame, unless something there needs the room: then over to the left.
          const over = tall ? 0 : easeOut(aside.left, 2);
          const left = tall ? (hud.width - width) / 2 : hud.width - MARGIN - width + (2 * MARGIN + width - hud.width) * over;
          const bottom = MARGIN - (1 - shown) * 12;
          const top = bottom + height; // (pieces are placed from the panel's top left)
          hud.place(panel, left, bottom, { alpha: shown });
          held = [left, bottom, left + width, top];
          const after = claim.copiedAt !== null && time - claim.copiedAt < 4;
          for (const piece of pieces) {
            if (piece.when === "before" && after) continue;
            if (piece.when === "after" && !after) continue;
            const card = Array.isArray(piece.card) ? piece.card[ctx.touch.pressing === piece.press ? 1 : 0] : piece.card;
            hud.place(card, left + piece.x, top - piece.y, { anchor: [0, 1], alpha: shown, press: shown > 0.9 ? piece.press : undefined });
          }
          if (tall) base = MARGIN + height + 4;
        }
      }
      cardTop = carded > 0 && claim?.built ? MARGIN + claim.built.height : 0;
      cardBox = held;
      aside.left = 0;
      aside.hidden = 0;
      // While the chat is open, its field has the foot of the frame, and the lines sit over it (the claim card has stepped away: ./chat.ts),
      // a row up: as the game's lines keep clear of its field, so what a command takes, or a lone suggestion, lies between and covers none of them.
      const chat = ctx.chat;
      const typing = chat?.open ? chat : null;
      if (typing) base = FOOT + FIELD + 1 + ROW + 2;
      rise = typing ? Math.min(rise + dt / 0.12, 1) : Math.max(rise - dt / 0.15, 0);

      // Chat: each line pushes the older ones up, and fades after a while or when its place is left.
      // Open, it shows what was said here however long ago, as the game's does, up to as many lines as it has room for.
      for (const line of lines) {
        if (line.script) {
          // A scripted line arrives when its trigger has run its delay, and keeps that moment until the scroll takes it back.
          const since = line.script.trigger.t - line.script.delay;
          if (since <= 0) line.begun = null;
          else if (line.begun === null) line.begun = time - since;
        }
        // Talk belongs to its place: a line said elsewhere, or long ago, is not there (and takes no room).
        line.here =
          line.begun !== null && time >= line.begun && s >= line.from && s < line.until && (typing || time - line.begun < line.stay + 0.5) && !line.script?.hush?.(line.begun);
        line.shown = line.here ? Math.min(line.shown + dt / 0.1, 1) : Math.max(line.shown - dt / 0.3, 0);
      }
      // Whatever else lies at the foot of the frame where the lines go has the place (the book at home, across a narrow frame): they sit on it.
      const reach = 4 + lines.reduce((widest, line) => (line.here || line.shown > 0 ? Math.max(widest, line.card.width) : widest), 0);
      const footed = extras.filter((extra) => extra.alpha > 0.05).map((extra) => [...hud.where(extra.card, extra.x, extra.y, extra), extra.card] as const);
      for (const [left, bottom, card] of footed.sort((a, b) => a[1] - b[1])) if (left < reach && left + card.width > 4 && bottom < base + LINE) base = Math.max(base, bottom + card.height + GAP);
      // (What the visitor's chat says takes no room from the film's: while it stays, there is a line more for each, /help's every line.
      // But not over the claim card in a narrow frame, where more would reach the name field.)
      const own = lines.reduce((count, line) => count + (line.own && line.here ? 1 : 0), 0);
      const limit = typing ? OPEN_LINES : tall && carded > 0 ? CHAT_LINES : Math.min(CHAT_LINES + own, OPEN_LINES);
      hud.hidden = ui.hidden && !typing; // (F1 puts the chat away, but not while it is open)
      lines.forEach((line, i) => {
        if (line.begun === null || line.shown <= 0) return;
        const age = time - line.begun;
        const later = newerThan(lines, i);
        let risen = 0;
        for (const begun of later) risen += easeOut((time - begun) / 0.12, 2);
        const newer = later.length;
        // Crowded out by the line that made it one too many.
        const crowded = newer >= limit ? later.sort((a, b) => a - b)[limit - 1] : Infinity;
        let alpha = line.shown * (typing ? 1 : 1 - clamp01((age - line.stay) / 0.5));
        if (crowded < Infinity) alpha *= 1 - clamp01((time - crowded) / 0.2);
        hud.place(line.card, 4, base + LINE * risen, { alpha });
      });
      hud.hidden = ui.hidden;

      // The action bar: in the middle, as high as the game has it; but over the chat (its
      // four lines and a fifth on its way out), and over the claim card where it lies under it.
      // A soft shade under the letters keeps them readable on whatever the film has there.
      let spoken: [number, number, number, number] | null = null; // where it is: left, bottom, right, top
      if (message) {
        const width = hud.width - 2 * MARGIN;
        if (message.room !== width) message.card = null;
        message.room = width;
        const card = (message.card ??= draw.centred(draw.balanced(message.text, width)));
        if (message.left === null && (s < message.from || s >= message.until)) message.left = time;
        const alpha = (1 - clamp01((time - message.at - ACTION_STAYS) / ACTION_FADES)) * (message.left === null ? 1 : 1 - clamp01((time - message.left) / 0.2));
        const left = Math.round((hud.width - card.width) / 2);
        let bottom = Math.max(ACTION_BAR, base + (limit + 1) * LINE + GAP);
        if (held && held[0] < left + card.width && held[2] > left) bottom = Math.max(bottom, held[3] + GAP);
        if (alpha <= 0) message = null;
        else {
          hud.soften(left - 1, bottom, card.width + 2, card.height - 1, 0.6 * alpha);
          hud.place(card, left, bottom, { alpha });
          spoken = [left, bottom, left + card.width, bottom + card.height];
        }
      }

      // Toasts, one at a time (./toasts.ts), sliding in from the right under the bar; and
      // under the letters of a headline it is told to keep off, where they come down that far.
      // It never rises again while it shows (the camera taking the headline away does not
      // take the toast with it); and once the scroll has left its stretch, a headline that
      // comes over it on its way out has the place at once.
      for (const [{ card, keep }, away] of toasts.update(s, time, jumped)) {
        const left = hud.width - MARGIN - TOAST[0];
        let top = keep?.top ?? hud.height - MARGIN - BAR - GAP;
        const x = left + (TOAST[0] + 16) * away;
        if (keep) {
          const boxes = ctx.speech.boxesOf(keep.under, hud).sort((a, b) => b[3] - a[3]);
          if (s >= keep.from && s < keep.until) {
            for (const box of boxes) if (box[0] < hud.width - MARGIN && box[2] > left && box[1] < top && box[3] > top - TOAST[1]) top = Math.floor(box[1]) - GAP;
            keep.top = top;
          } else if (keep.gone || boxes.some((box) => box[0] < x + TOAST[0] && box[2] > x && box[1] < top && box[3] > top - TOAST[1])) {
            keep.gone = true;
            continue;
          }
        }
        hud.place(card, x, top, { anchor: [0, 1] });
      }

      // The bar. Its ways in step aside while the claim card offers them; where the frame is too narrow for all of it, the mark stands for the name.
      const top = hud.height - MARGIN;
      const pressing = ctx.touch.pressing;
      const get = (ctx.home ? bar.home : bar.get)[pressing === "get" ? 1 : 0];
      const signIn = bar.signIn[pressing === "sign-in" ? 1 : 0];
      const away = { anchor: [1, 1] as const, alpha: 1 - carded };
      const [left] = hud.place(get, hud.width - MARGIN, top, { ...away, press: "get" });
      const [from] = hud.place(signIn, left - GAP, top, { ...away, press: "sign-in" });
      let x = MARGIN;
      if (bar.mark) {
        hud.place(bar.mark, x, top - 2, { anchor: [0, 1], press: "home" });
        x += bar.mark.width + GAP + 1;
      }
      if (x + bar.name.width + GAP <= from || !bar.mark) hud.place(bar.name, x, top, { anchor: [0, 1], press: "home" });

      // Tooltips and nametags keep under the bar. A nametag hangs over a head in the world:
      // where that head is up under the bar, or out of the frame over it, the tag is not
      // drawn (it fades over its last few pixels before the bar), as the frame would cut it.
      const ceiling = hud.height - MARGIN - BAR - GAP;
      for (const tip of tips) {
        if (tip.of) tip.alpha *= clamp01((ceiling - hud.where(tip.card, tip.at[0], tip.at[1], { anchor: tip.anchor })[1] - tip.card.height) / 6);
        const [left, below] = hud.where(tip.card, tip.at[0], tip.at[1], { anchor: tip.anchor, inside: 4 });
        let bottom = below;
        // (A tooltip that would lie on the claim card goes over it: the card stays whole. The nametags stay with their heads.)
        const floor = !tip.of && held && left < held[2] && left + tip.card.width > held[0] ? held[3] + GAP : 4;
        if (floor > 4 && bottom < floor && bottom + tip.card.height > held![1]) bottom = floor;
        // (One that would lie on the action bar goes over it, or under it if that is nearer and there is room.)
        if (spoken && left < spoken[2] && left + tip.card.width > spoken[0] && bottom < spoken[3] + GAP && bottom + tip.card.height > spoken[1] - GAP) {
          const up = spoken[3] + GAP - bottom;
          const down = bottom + tip.card.height - (spoken[1] - GAP);
          bottom += down < up && bottom - down >= floor ? -down : up;
        }
        const [x, y] = hud.place(tip.card, left, Math.min(bottom, ceiling - tip.card.height), { alpha: tip.alpha, inside: 4 });
        if (hud.hidden || tip.alpha <= 0.004) continue;
        tip.drawn = [x, y, x + tip.card.width, y + tip.card.height];
        const { link } = tip;
        if (!link) continue;
        // (Lit under the pointer, as the credits' links are; pressable once the tooltip is all there.)
        const [, foot] = hud.place(link.card[ctx.touch.pressing === link.press ? 1 : 0], x + link.x, y + tip.card.height - link.y, { anchor: [0, 1], alpha: tip.alpha, press: tip.alpha > 0.9 ? link.press : undefined });
        tip.drawn = [x, Math.min(y, foot), x + tip.card.width, y + tip.card.height];
      }
      hud.hidden = false;
      for (const extra of extras) hud.place(extra.card, extra.x, extra.y, extra);
      // What the friends say, over their heads: clear of all the above, and under the bar.
      hud.hidden = ui.hidden;
      ctx.speech.update(s, time, dt, hud, draw, tips, ceiling);
      hud.hidden = false;
      // The chat's field, over all of it while it is open (it is the game's chat screen); and over that, the death screen.
      if (typing) typed(typing, time);
      if (chat?.dead) died(chat.dead, time);
      // And over everything, the film's cover, where the page comes round under it (nothing it hides can be pressed).
      if (ui.cover > 0.5) hud.cover();
      hud.wash(COVER, ui.cover);
      tips.length = 0;
      extras.length = 0;
      under.length = 0;
      ui.night = 0;
      ui.cover = 0;
    },
  };
  return ui;
}

/**
 * When the lines of chat newer than line `i` began, of those that are showing: each of
 * them pushes it up a line. Lines that began at the same moment are told apart by their
 * order (the later in the list is the newer), so no two ever take the same place.
 */
export function newerThan(lines: readonly { begun: number | null; here?: boolean; shown: number }[], i: number): number[] {
  const line = lines[i];
  const later: number[] = [];
  if (line.begun === null) return later;
  lines.forEach((other, j) => {
    if (other.begun === null || !(other.here || other.shown > 0)) return;
    if (other.begun > line.begun! || (other.begun === line.begun && j > i)) later.push(other.begun);
  });
  return later;
}

/** The brand's mark, from the page's own icon (the interface does without it, if it will not load). */
async function brandMark(): Promise<HTMLImageElement | null> {
  const image = new Image();
  image.src = "/kurbis.svg";
  try {
    await image.decode();
    return image;
  } catch {
    return null;
  }
}

export { GREY, WHITE, YELLOW };
