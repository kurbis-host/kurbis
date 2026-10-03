/**
 * Console text is treated as untrusted plain text. This parser understands the
 * styling escape sequences emitted by Pumpkin/Java and keeps them out of both
 * search and clipboard/download output.
 */

export interface ConsoleTextStyle {
  foreground?: string;
  background?: string;
  bold?: boolean;
  dim?: boolean;
  italic?: boolean;
  underline?: boolean;
  strike?: boolean;
  obfuscated?: boolean;
}

export interface ConsoleTextToken {
  text: string;
  style: ConsoleTextStyle;
}

const MINECRAFT_COLORS: Record<string, string> = {
  0: "#000000",
  1: "#0000aa",
  2: "#00aa00",
  3: "#00aaaa",
  4: "#aa0000",
  5: "#aa00aa",
  6: "#ffaa00",
  7: "#aaaaaa",
  8: "#555555",
  9: "#5555ff",
  a: "#55ff55",
  b: "#55ffff",
  c: "#ff5555",
  d: "#ff55ff",
  e: "#ffff55",
  f: "#ffffff",
};

const ANSI_COLORS: Record<number, string> = {
  30: "#000000",
  31: "#aa0000",
  32: "#00aa00",
  33: "#ffaa00",
  34: "#0000aa",
  35: "#aa00aa",
  36: "#00aaaa",
  37: "#aaaaaa",
  90: "#555555",
  91: "#ff5555",
  92: "#55ff55",
  93: "#ffff55",
  94: "#5555ff",
  95: "#ff55ff",
  96: "#55ffff",
  97: "#ffffff",
};

const ANSI_BACKGROUND_COLORS: Record<number, string> = Object.fromEntries(
  Object.entries(ANSI_COLORS).map(([code, color]) => [Number(code) + 10, color]),
);

const emptyStyle = (): ConsoleTextStyle => ({});

function cloneStyle(style: ConsoleTextStyle): ConsoleTextStyle {
  return { ...style };
}

function resetStyle(style: ConsoleTextStyle) {
  for (const key of Object.keys(style) as Array<keyof ConsoleTextStyle>) {
    delete style[key];
  }
}

function setMinecraftColor(style: ConsoleTextStyle, color: string) {
  resetStyle(style);
  style.foreground = color;
}

function setAnsiColor(style: ConsoleTextStyle, color: string) {
  style.foreground = color;
}

function setAnsiBackground(style: ConsoleTextStyle, color: string) {
  style.background = color;
}

function ansi256Color(index: number) {
  if (index < 16) {
    const basic = [
      "#000000", "#800000", "#008000", "#808000", "#000080", "#800080", "#008080", "#c0c0c0",
      "#808080", "#ff0000", "#00ff00", "#ffff00", "#0000ff", "#ff00ff", "#00ffff", "#ffffff",
    ];
    return basic[index];
  }
  if (index < 232) {
    const cube = index - 16;
    const red = Math.floor(cube / 36);
    const green = Math.floor((cube % 36) / 6);
    const blue = cube % 6;
    const channel = (value: number) => (value === 0 ? 0 : 55 + value * 40);
    return `rgb(${channel(red)}, ${channel(green)}, ${channel(blue)})`;
  }
  const gray = 8 + (index - 232) * 10;
  return `rgb(${gray}, ${gray}, ${gray})`;
}

function applyAnsiSgr(style: ConsoleTextStyle, params: number[]) {
  const values = params.length ? params : [0];
  for (let index = 0; index < values.length; index += 1) {
    const value = values[index];
    if (value === 0) resetStyle(style);
    else if (value === 1) style.bold = true;
    else if (value === 2) style.dim = true;
    else if (value === 3) style.italic = true;
    else if (value === 4) style.underline = true;
    else if (value === 9) style.strike = true;
    else if (value === 22) {
      style.bold = undefined;
      style.dim = undefined;
    } else if (value === 23) style.italic = undefined;
    else if (value === 24) style.underline = undefined;
    else if (value === 29) style.strike = undefined;
    else if (value === 39) style.foreground = undefined;
    else if (value === 49) style.background = undefined;
    else if (ANSI_COLORS[value]) setAnsiColor(style, ANSI_COLORS[value]);
    else if (ANSI_BACKGROUND_COLORS[value]) setAnsiBackground(style, ANSI_BACKGROUND_COLORS[value]);
    else if (value === 38 || value === 48) {
      const isBackground = value === 48;
      const mode = values[index + 1];
      if (mode === 5 && values[index + 2] !== undefined) {
        const color = ansi256Color(Math.max(0, Math.min(255, values[index + 2])));
        if (isBackground) setAnsiBackground(style, color);
        else setAnsiColor(style, color);
        index += 2;
      } else if (mode === 2 && values[index + 4] !== undefined) {
        const channels = values.slice(index + 2, index + 5)
          .map((channel) => Math.max(0, Math.min(255, channel)));
        const color = `rgb(${channels.join(", ")})`;
        if (isBackground) setAnsiBackground(style, color);
        else setAnsiColor(style, color);
        index += 4;
      }
    }
  }
}

function csiSequenceEnd(input: string, start: number) {
  for (let index = start; index < input.length; index += 1) {
    const code = input.charCodeAt(index);
    if (code >= 0x40 && code <= 0x7e) return index + 1;
  }
  return input.length;
}

function ansiSequenceEnd(input: string, start: number) {
  const next = input[start + 1];
  if (next === "[") return csiSequenceEnd(input, start + 2);
  if (next === "]") {
    for (let index = start + 2; index < input.length; index += 1) {
      if (input[index] === "\u0007") return index + 1;
      if (input[index] === "\u001b" && input[index + 1] === "\\") return index + 2;
    }
    return input.length;
  }
  // Fe, Fp, and other two-byte escape sequences are not printable console
  // content. Keep the following character out of the output as well.
  return Math.min(input.length, start + 2);
}

function pushToken(tokens: ConsoleTextToken[], text: string, style: ConsoleTextStyle) {
  if (!text) return;
  const previous = tokens[tokens.length - 1];
  if (previous && JSON.stringify(previous.style) === JSON.stringify(style)) {
    previous.text += text;
  } else {
    tokens.push({ text, style: cloneStyle(style) });
  }
}

/** Parse ANSI SGR and Minecraft section-sign formatting into safe text tokens. */
export function parseConsoleText(input: string): ConsoleTextToken[] {
  const tokens: ConsoleTextToken[] = [];
  const style = emptyStyle();
  let text = "";
  const flush = () => {
    pushToken(tokens, text, style);
    text = "";
  };

  for (let index = 0; index < input.length;) {
    const character = input[index];
    if (character === "\u001b" || character === "\u009b") {
      flush();
      const csi = character === "\u009b" || input[index + 1] === "[";
      const end = character === "\u009b"
        ? csiSequenceEnd(input, index + 1)
        : ansiSequenceEnd(input, index);
      const sequence = input.slice(index, end);
      if (csi && sequence.endsWith("m")) {
        const body = sequence.slice(character === "\u009b" ? 1 : 2, -1);
        const params = body ? body.split(";").map((value) => Number(value || 0)) : [];
        if (params.every(Number.isFinite)) applyAnsiSgr(style, params);
      }
      index = end;
      continue;
    }
    if (character === "§") {
      const code = input[index + 1]?.toLowerCase();
      const hexColor = code === "x" ? input.slice(index, index + 14) : "";
      const hexMatch = /^§x(?:§[0-9a-f]){6}$/i.test(hexColor);
      if (hexMatch) {
        flush();
        style.foreground = `#${hexColor.replace(/(?:§x|§)/gi, "")}`;
        style.background = undefined;
        style.bold = undefined;
        style.dim = undefined;
        style.italic = undefined;
        style.underline = undefined;
        style.strike = undefined;
        style.obfuscated = undefined;
        index += 14;
        continue;
      }
      if (code && (MINECRAFT_COLORS[code] || "klmnor".includes(code))) {
        flush();
        if (MINECRAFT_COLORS[code]) setMinecraftColor(style, MINECRAFT_COLORS[code]);
        else if (code === "k") style.obfuscated = true;
        else if (code === "l") style.bold = true;
        else if (code === "m") style.strike = true;
        else if (code === "n") style.underline = true;
        else if (code === "o") style.italic = true;
        else if (code === "r") resetStyle(style);
        index += 2;
        continue;
      }
    }
    const code = input.charCodeAt(index);
    // Keep tabs/newlines; discard terminal control characters and CR (which
    // is a cursor movement rather than visible text in a log line).
    const control = (code >= 0x00 && code < 0x20) || (code >= 0x7f && code <= 0x9f);
    if (character !== "\r" && (!control || character === "\n" || character === "\t")) {
      text += character;
    }
    index += 1;
  }
  flush();
  return tokens;
}

/** Return text a user can read, search, copy, or download without formatting codes. */
export function visibleConsoleText(input: string): string {
  return parseConsoleText(input).map((token) => token.text).join("");
}

export type ConsoleCommandValidation =
  | { success: true; command: string }
  | { success: false; reason: "empty" | "control_character" | "too_long" };

/** Normalize the panel's visible slash prefix into one backend command line. */
export function normalizeConsoleCommand(input: string): ConsoleCommandValidation {
  const command = input.trim().replace(/^\/+/, "");
  if (!command) return { success: false, reason: "empty" };
  if (/\p{Cc}/u.test(command)) return { success: false, reason: "control_character" };
  if (command.length > 512) return { success: false, reason: "too_long" };
  return { success: true, command };
}

export interface ReadableLogEntry {
  created_at: string;
  line: string;
}

export function readableConsoleText(entries: ReadableLogEntry[]): string {
  return entries
    .map((entry) => `[${new Date(entry.created_at).toISOString()}] ${visibleConsoleText(entry.line)}`)
    .join("\n");
}
