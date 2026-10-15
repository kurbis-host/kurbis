/**
 * The game's chat, which "/" opens with the slash typed, as it does in the game (the page
 * hands the keyboard over: components/landing/film-canvas.tsx). While it is open, what is
 * typed goes into its field, at the foot of the frame, and Enter sends it; a few of the
 * game's commands do here what they do there (./commands.ts), and say so as the game does:
 *
 *   /help                    the commands, a line each
 *   /time set <day|night>    night falls over the open air, or it is day again (./weather.ts)
 *   /weather <clear|rain>    rain over the open air, or none
 *   /stop                    the server goes to sleep: in the cave, as its lever puts it to
 *                            sleep (./cave.ts); anywhere else, the chat says it is asleep
 *   /kill                    the game's death screen. Respawn, or Title Screen, takes the
 *                            visitor back to the top of the page, as respawning is at spawn
 *   /seed                    the world's seed
 *
 * A line without a slash is said, as a player says it. The field, its suggestions and the
 * death screen are the interface's to draw (./interface.ts).
 */

import { BUNDLE } from "../engine/version";
import { GREEN, GREY, WHITE, type Rgb, type Words } from "../engine/hud";
import { COPY } from "../copy";
import type { Ctx } from "./context";
import { Field, RED, TIMES, fill, parse, pointAt, seedOf } from "./commands";
import type { Weather } from "./weather";

/** How long what the chat says stays (the game's ten seconds). */
const STAY = 10;
/** How far the cave's stretch goes for /stop: where its lever answers, and its chat is heard. */
const CAVE_UNTIL = 3.62;
/** Seconds before the death screen's buttons can be pressed (the game's second), and how long it takes to go. */
const READY = 1;
const GONE = 0.25;
/** The suggestions the field shows at once, as the game's do. */
export const HINTS = 10;

/** The keys held with a key. (AltGr, which some keyboards type letters with, is held as Ctrl and Alt both.) */
export interface Mods {
  shift: boolean;
  alt: boolean;
  ctrl: boolean;
  meta: boolean;
  altGraph?: boolean;
}

/** The death screen, while it is up: when it came, when it went (null while it is there), and which button the keyboard is on. */
export interface Dead {
  at: number;
  gone: number | null;
  focus: 0 | 1 | null;
}

export interface Chat {
  /** The field is open. */
  readonly open: boolean;
  readonly field: Field;
  /** When the field last opened or changed: its cursor blinks from then. */
  readonly since: number;
  readonly dead: Dead | null;
  /** The chat, or the death screen, has the keyboard. */
  readonly keyboard: boolean;
  /** "/" was pressed: the field opens with it typed. */
  start(text: string, time: number): void;
  /** The field closes, as Esc closes it. */
  close(): void;
  /** A key, while the chat or the death screen has the keyboard: true if it took it. */
  key(key: string, mods: Mods, time: number): boolean;
  /** Text pasted, while the field is open. */
  paste(text: string, time: number): void;
}

/** Who the visitor is in the chat: the player whose skin they wear, by their name; else "You" (as the pond's chat names them). */
export function youOf(ctx: Ctx): string {
  return ctx.wearer ? ctx.players[ctx.wearer] : COPY.chat.you;
}

export function setChat(ctx: Ctx, weather: Weather, jump: (screens: number) => void): Chat {
  const { tl, C } = ctx;
  const field = new Field();
  const glyphs = ctx.stage.data.glyphs.pixel.glyphs;
  let open = false;
  let since = 0;
  let dead: Dead | null = null;
  let away = 0; // how far the claim card is put away while the chat has the foot of the frame
  let hovered: string | null = null;

  /** Said in the chat, now (or `delay` seconds on): broken into lines the frame has room for. */
  const say = (words: Words, color: Rgb, time: number, delay = 0) => {
    const ui = ctx.ui;
    if (!ui) return;
    for (const line of ui.draw.breakLine(words, ui.hud.width - 15)) ui.say(line, color, time + delay, { stay: STAY, own: true });
  };
  const server = () => (ctx.visitor ? `${ctx.visitor}${COPY.surface.host}` : COPY.cave.server);

  const run = (line: string, time: number) => {
    const parsed = parse(line);
    if (parsed.kind === "said") return say(fill(COPY.chat.said, { name: youOf(ctx), text: parsed.text }), WHITE, time);
    if (parsed.kind === "error") {
      say(parsed.message, RED, time);
      say(pointAt(parsed.input, parsed.at), RED, time);
      return;
    }
    const [command, ...rest] = parsed.words;
    if (command === "help") for (const usage of COPY.chat.help) say(usage, WHITE, time);
    else if (command === "time") {
      const when = rest[1] as keyof typeof TIMES;
      weather.setNight(when === "night");
      say(fill(COPY.chat.time, { time: TIMES[when] }), WHITE, time);
    } else if (command === "weather") {
      const kind = rest[0] as keyof typeof COPY.chat.weather;
      weather.setRain(kind === "rain");
      say(COPY.chat.weather[kind], WHITE, time);
    } else if (command === "stop") {
      say(COPY.chat.stopping, WHITE, time);
      // In the cave, it is the lever's own sleep (its own lines follow). Anywhere else, the server is asleep until someone joins.
      const s = tl.frame.s;
      const cave = ctx.cave;
      if (cave && s >= C.WAKE + 0.2 && s < CAVE_UNTIL && cave.awake()) cave.pull(time);
      else say(`${server()} ${COPY.cave.sleeps}`, GREY, time, 0.6);
    } else if (command === "kill") {
      dead = { at: time, gone: null, focus: null };
      say(fill(COPY.chat.killed, { name: youOf(ctx) }), WHITE, time);
    } else if (command === "seed") {
      const [before, after] = COPY.chat.seed.split("{seed}");
      say([{ text: before }, { text: seedOf(BUNDLE), color: GREEN }, { text: after }], WHITE, time);
    }
  };

  /** Back at spawn: the top of the page, under the death screen as it goes. */
  const respawn = (time: number) => {
    if (!dead || dead.gone !== null || time - dead.at < READY) return;
    dead.gone = time;
    jump(0);
  };
  for (const name of ["respawn", "title-screen"]) ctx.touch.button(name, respawn);
  // A suggestion clicked goes in, as Tab puts it in.
  for (let i = 0; i < HINTS; i += 1)
    ctx.touch.button(`chat-hint-${i}`, (time) => {
      if (!open || !field.hints) return;
      field.pick(i);
      field.use();
      since = time;
    });

  tl.always(({ time, dt }) => {
    // The claim card steps away while the chat has the foot of the frame, and comes back after.
    away = open ? Math.min(away + dt / 0.15, 1) : Math.max(away - dt / 0.2, 0);
    if (ctx.ui) ctx.ui.aside.hidden = Math.max(ctx.ui.aside.hidden, away);
    // The pointer on a suggestion picks it, as the game's does (once, as it comes onto it: the keys may pick another after).
    const pressing = open ? ctx.touch.pressing : null;
    if (pressing !== hovered && pressing?.startsWith("chat-hint-")) field.pick(Number(pressing.slice(10)));
    hovered = pressing;
    // While the death screen is up, the world under it cannot be touched; once it has gone, it is put away.
    if (dead?.gone != null && time - dead.gone > GONE) dead = null;
    ctx.touch.screen = !!dead && dead.gone === null;
  });

  const chat: Chat = {
    get open() {
      return open;
    },
    field,
    get since() {
      return since;
    },
    get dead() {
      return dead;
    },
    get keyboard() {
      return open || (!!dead && dead.gone === null);
    },
    start(text, time) {
      if (open || (dead && dead.gone === null)) return;
      open = true;
      field.open(text);
      since = time;
    },
    close() {
      open = false;
    },
    key(key, mods, time) {
      // The browser's own shortcuts (copy, paste, reload) are the browser's; but for the field's own.
      const editing = /^(Backspace|Delete|ArrowLeft|ArrowRight)$/.test(key);
      if ((mods.ctrl || mods.meta) && !editing && !mods.altGraph) return false;
      if (dead && dead.gone === null) {
        // The death screen: Esc puts it away; Tab and the arrows go from one button to the other, and Enter or Space presses it.
        if (key === "Escape") dead.gone = time;
        else if (key === "Tab" || key === "ArrowDown" || key === "ArrowUp") {
          const back = key === "ArrowUp" || (key === "Tab" && mods.shift);
          dead.focus = dead.focus === null ? (back ? 1 : 0) : dead.focus === 0 ? 1 : 0; // (of two, the next is the other either way)
        } else if ((key === "Enter" || key === " ") && dead.focus !== null) respawn(time);
        return true;
      }
      if (!open) return false;
      const word = (mods.alt || mods.ctrl) && !mods.altGraph;
      const end = field.text.length;
      switch (key) {
        case "Escape":
          // (As the game's: the suggestions go first, and the chat with the next.)
          if (field.hints) field.hide();
          else open = false;
          break;
        case "Enter": {
          const line = field.send();
          open = false;
          if (line) run(line, time);
          break;
        }
        case "Tab":
          field.tab(mods.shift);
          break;
        case "ArrowUp":
        case "ArrowDown":
          field.step(key === "ArrowUp" ? -1 : 1);
          break;
        case "ArrowLeft":
          field.move(word ? field.word(-1) : mods.meta ? 0 : field.cursor - 1);
          break;
        case "ArrowRight":
          field.move(word ? field.word(1) : mods.meta ? end : field.cursor + 1);
          break;
        case "Home":
          field.move(0);
          break;
        case "End":
          field.move(end);
          break;
        case "Backspace":
          field.erase(word ? field.word(-1) : mods.meta ? 0 : field.cursor - 1);
          break;
        case "Delete":
          field.erase(word ? field.word(1) : mods.meta ? end : field.cursor + 1);
          break;
        default:
          // A letter the chat's type has (the game takes any it can draw). Every other key is the field's too, and does nothing.
          if ([...key].length === 1 && glyphs[key]) field.type(key);
      }
      since = time;
      return true;
    },
    paste(text, time) {
      if (!open) return;
      // (As the game takes it: on one line, and only what its type can draw.)
      field.type([...text.replace(/[\r\n\t]+/g, " ")].filter((ch) => glyphs[ch]).join(""));
      since = time;
    },
  };
  return chat;
}
