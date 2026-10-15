/**
 * The few commands the film's chat takes (./chat.ts), and the chat's field as the game's
 * works: what is typed, where the cursor is, the lines sent before, and the game's
 * suggestions for what is being typed.
 *
 * A line is read as the game reads one (Brigadier, its command parser): word by word, case
 * and all, each word one of those that may follow the last. Where it stops making sense is
 * where its error points. The suggestions, and what Tab puts in, come from the same words.
 *
 * Nothing here draws or does anything: it is all words, so it can be tested on its own.
 */

import { COPY } from "../copy";
import { GREY, type Rgb, type Span } from "../engine/hud";

/** The game's red (its chat's errors, and what is typed of a command that cannot be one). */
export const RED: Rgb = [1, 0.33, 0.33];

/** A command's words: each word that may come next, and what may follow it (nothing: the command is whole). */
export interface Tree {
  readonly [word: string]: Tree;
}

/** What the chat takes. (What /help lists of them is the copy's: COPY.chat.help.) */
export const COMMANDS: Tree = {
  time: { set: { day: {}, night: {} } },
  weather: { clear: {}, rain: {} },
  stop: {},
  kill: {},
  seed: {},
  help: {},
};

/** The game's numbers for the times of day /time set takes (ticks: night falls at 13000, and the day starts at 1000). */
export const TIMES = { day: 1000, night: 13000 } as const;

/** The longest line the game's chat takes. */
export const LONGEST = 256;

/** The word that may follow, in a command's words (none: null). */
function next(tree: Tree, word: string): Tree | null {
  return Object.prototype.hasOwnProperty.call(tree, word) ? tree[word] : null;
}

/** A line as the game sends it: without the spaces round it, and any run of spaces inside it as one. */
export function normalize(line: string): string {
  return line.trim().replace(/\s+/g, " ");
}

/** What a line is: something said; a command the chat can run (its words); or one it cannot, and where it goes wrong. */
export type Parsed =
  | { kind: "said"; text: string }
  | { kind: "command"; words: string[] }
  /** `input` is the command without its slash, as the game's error shows it; `at` is where in it the error points. */
  | { kind: "error"; message: string; input: string; at: number };

/**
 * Read a line as the game reads it (as typed: the chat's field colours it so; `normalize` it
 * first for what is sent). A command none of whose words the chat knows, and one that is not
 * whole yet, is "Unknown or incomplete"; one that goes wrong after a word it knows has an
 * "Incorrect argument", pointing at what follows that word.
 */
export function parse(line: string): Parsed {
  if (!line.startsWith("/")) return { kind: "said", text: line };
  const input = line.slice(1);
  const words: string[] = [];
  let tree = COMMANDS;
  let at = 0;
  for (const word of input.split(" ")) {
    const then = next(tree, word);
    if (!then) return { kind: "error", message: words.length ? COPY.chat.incorrect : COPY.chat.unknown, input, at };
    words.push(word);
    tree = then;
    at += word.length + 1;
  }
  // Every word was one it knows, but the command wants more: it points past the end.
  if (Object.keys(tree).length) return { kind: "error", message: COPY.chat.unknown, input, at: input.length };
  return { kind: "command", words };
}

/**
 * The game's second line for an error: the command up to where it went wrong (no more than
 * ten letters of it, after "..."), in grey; what follows, red and underlined; and the game's
 * pointer, in red.
 */
export function pointAt(input: string, at: number): Span[] {
  const before = at > 10 ? `...${input.slice(at - 10, at)}` : input.slice(0, at);
  const spans: Span[] = [];
  if (before) spans.push({ text: before, color: GREY });
  if (at < input.length) spans.push({ text: input.slice(at), color: RED, underline: true });
  spans.push({ text: COPY.chat.here, color: RED });
  return spans;
}

/** What the game suggests for a line as typed up to the cursor: the words that would do there, and where the word they finish begins and ends (the cursor's word, on to the next space). */
export interface Hints {
  start: number;
  end: number;
  words: string[];
}

/**
 * The game's suggestions for a command as typed up to the cursor: each word that may come
 * where the cursor is and that begins with what is typed of it there (in either case), in
 * the order of the alphabet; but not the word as it is typed already, which there is
 * nothing to add to (as the game leaves it out). Nothing for a line that is not a command,
 * or after a word the chat does not know.
 */
export function suggest(line: string, cursor = line.length): Hints | null {
  const typed = line.slice(0, cursor);
  if (!typed.startsWith("/")) return null;
  const words = typed.slice(1).split(" ");
  const last = words.pop() ?? "";
  let tree = COMMANDS;
  let start = 1;
  for (const word of words) {
    const then = next(tree, word);
    if (!then) return null;
    tree = then;
    start += word.length + 1;
  }
  const found = Object.keys(tree)
    .filter((word) => word !== last && word.toLowerCase().startsWith(last.toLowerCase()))
    .sort();
  const space = line.indexOf(" ", cursor);
  return found.length ? { start, end: space < 0 ? line.length : space, words: found } : null;
}

/**
 * What the game shows over the field while it has no suggestions, for a command that cannot
 * run as it is typed (the cursor at its end): what the command takes, as /help says it
 * (COPY.chat.help), in grey. For a name the chat does not know, the names it does, each
 * after its slash, in the order the suggestions have them. Nothing for a slash alone (nothing
 * is wrong yet), a command that can run, or a line that is not a command.
 */
export function usage(line: string, cursor = line.length): string | null {
  if (cursor !== line.length || !line.startsWith("/") || line === "/") return null;
  const parsed = parse(normalize(line));
  if (parsed.kind !== "error") return null;
  const [name] = parsed.input.split(" ");
  if (!next(COMMANDS, name)) return Object.keys(COMMANDS).sort().map((known) => `/${known}`).join(" ");
  return COPY.chat.help.find((said) => said === `/${name}` || said.startsWith(`/${name} `)) ?? null;
}

/**
 * The world's seed, as /seed says it: the film's world is made by hand, not grown from a
 * seed, so it is the bundle's own version (a digest of every file of it: engine/version.ts),
 * whose 64 bits are read as the game reads a seed's: one whole number, perhaps below zero.
 */
export function seedOf(version: string): string {
  return BigInt.asIntN(64, BigInt(`0x${version.replace(/[^0-9a-f]/gi, "").slice(-16) || "0"}`)).toString();
}

/** A line of the copy with its values filled in. */
export function fill(template: string, values: Record<string, string | number>): string {
  return template.replace(/\{(\w+)\}/g, (all, key: string) => (key in values ? String(values[key]) : all));
}

/** The suggestions as the field shows them: which is picked, whether Tab has put one in, and the line they were made for. */
interface Shown extends Hints {
  picked: number;
  tabbed: boolean;
  /** The line as it stood when the suggestions came. */
  line: string;
}

/**
 * The chat's field, as the game's chat screen has it. What is typed goes in at the cursor;
 * ↑ and ↓ go back through the lines sent before (the game keeps a hundred), or, while the
 * game's suggestions show, through those. Tab puts the picked suggestion in, and the next
 * one at each Tab after that (Shift+Tab, the one before); without any showing, Tab shows
 * them, and Esc puts them away. They come by themselves as a command is typed, and follow
 * the cursor to the word it is in; but not as the field opens, for a line brought back from
 * before, or once Esc has put them away, until the line is changed (as the game's do).
 */
export class Field {
  text = "";
  cursor = 0;
  /** The game's suggestions for what is typed, while they show. */
  hints: Shown | null = null;
  private readonly sent: string[] = [];
  /** Where ↑ and ↓ have got to among the lines sent (their count: the line being typed), and what that line was. */
  private back = 0;
  private kept = "";
  /** The suggestions come by themselves (once the line has been changed). */
  private allowed = false;

  /** The field opens with this typed ("/"), the cursor after it, and no suggestions yet: ↑ is for the lines sent before. */
  open(text: string) {
    this.text = text.slice(0, LONGEST);
    this.cursor = this.text.length;
    this.back = this.sent.length;
    this.kept = "";
    this.allowed = false;
    this.hints = null;
  }

  /** The line, as it is sent; it is kept for ↑ (unless it is the one sent last, or empty). */
  send(): string {
    const line = normalize(this.text);
    if (line && this.sent[this.sent.length - 1] !== line) this.sent.push(line);
    if (this.sent.length > 100) this.sent.shift();
    return line;
  }

  /** Letters in at the cursor (no more than the line has room for). */
  type(letters: string) {
    const room = LONGEST - this.text.length;
    if (room <= 0 || !letters) return;
    const put = letters.slice(0, room);
    this.text = this.text.slice(0, this.cursor) + put + this.text.slice(this.cursor);
    this.cursor += put.length;
    this.edited();
  }

  /** Everything between the cursor and `to` goes. */
  erase(to: number) {
    const [from, until] = [Math.min(to, this.cursor), Math.max(to, this.cursor)].map((at) => Math.min(Math.max(at, 0), this.text.length));
    if (from === until) return;
    this.text = this.text.slice(0, from) + this.text.slice(until);
    this.cursor = from;
    this.edited();
  }

  /** The cursor to there; the suggestions, while they show or come by themselves, go with it to the word it is in. */
  move(to: number) {
    const at = Math.min(Math.max(to, 0), this.text.length);
    if (at === this.cursor) return;
    this.cursor = at;
    if (this.allowed || this.hints) this.suggest(true);
  }

  /** Where the word before the cursor begins (-1), or where the next one does (1), as the game jumps a word. */
  word(way: -1 | 1): number {
    let at = this.cursor;
    if (way > 0) {
      while (at < this.text.length && this.text[at] !== " ") at += 1;
      while (at < this.text.length && this.text[at] === " ") at += 1;
    } else {
      while (at > 0 && this.text[at - 1] === " ") at -= 1;
      while (at > 0 && this.text[at - 1] !== " ") at -= 1;
    }
    return at;
  }

  /** ↑ (-1) or ↓ (1): through the suggestions while they show, else through the lines sent before. */
  step(way: -1 | 1) {
    if (this.hints) {
      this.pick(this.hints.picked + way);
      this.hints.tabbed = false;
      return;
    }
    const to = Math.min(Math.max(this.back + way, 0), this.sent.length);
    if (to === this.back) return;
    if (this.back === this.sent.length) this.kept = this.text;
    this.back = to;
    this.text = to === this.sent.length ? this.kept : this.sent[to];
    this.cursor = this.text.length;
    this.allowed = false;
    this.hints = null;
  }

  /** Tab: the picked suggestion in, or (pressed again) the next one; without suggestions showing, they show. */
  tab(backwards = false) {
    if (!this.hints) {
      this.suggest(true);
      return;
    }
    if (this.hints.tabbed) this.pick(this.hints.picked + (backwards ? -1 : 1));
    this.use();
  }

  /** Esc: the suggestions go, until the line is changed or Tab is pressed (a second Esc closes the chat). */
  hide() {
    this.hints = null;
    this.allowed = false;
  }

  /** The suggestion the pointer is on is the one picked. */
  pick(index: number) {
    const hints = this.hints;
    if (!hints) return;
    const count = hints.words.length;
    hints.picked = ((index % count) + count) % count;
  }

  /** The picked suggestion goes in, in place of the word the cursor was in when the suggestions came; they stay. */
  use() {
    const hints = this.hints;
    if (!hints) return;
    const line = this.applied();
    this.text = line.slice(0, LONGEST);
    this.cursor = Math.min(hints.start + hints.words[hints.picked].length, this.text.length);
    hints.tabbed = true;
  }

  /** What the rest of the picked suggestion would add, shown faint after the cursor at the end of the line (as the game does). */
  get rest(): string {
    if (!this.hints || this.cursor !== this.text.length) return "";
    const line = this.applied();
    return line.startsWith(this.text) ? line.slice(this.text.length) : "";
  }

  /** What the command takes, shown over the field while no suggestions are (`usage`). */
  get usage(): string | null {
    return this.hints ? null : usage(this.text, this.cursor);
  }

  /** The line as it would be with the picked suggestion in. */
  private applied(): string {
    const hints = this.hints!;
    return hints.line.slice(0, hints.start) + hints.words[hints.picked] + hints.line.slice(hints.end);
  }

  private edited() {
    this.allowed = true;
    this.suggest();
  }

  /** The suggestions for the line as it is, the first picked (`asked`: by Tab, or by the cursor moving with them). */
  private suggest(asked = false) {
    const found = this.allowed || asked ? suggest(this.text, this.cursor) : null;
    this.hints = found ? { ...found, picked: 0, tabbed: false, line: this.text } : null;
  }
}
