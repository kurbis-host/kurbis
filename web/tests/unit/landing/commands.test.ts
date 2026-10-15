/**
 * The film's chat (lib/landing/film/commands.ts): a line is read as the game reads one,
 * its errors point where the game's do, the game's suggestions and Tab finish what is
 * typed, and the field keeps the lines sent before for ↑ and ↓.
 */

import { describe, expect, it } from "vitest";
import { COPY } from "../../../lib/landing/copy";
import { Draw, GREY } from "../../../lib/landing/engine/hud";
import { Field, LONGEST, RED, normalize, parse, pointAt, seedOf, suggest, usage } from "../../../lib/landing/film/commands";

describe("a line, as the game reads it", () => {
  it("runs the commands the chat knows, word by word", () => {
    expect(parse("/time set night")).toEqual({ kind: "command", words: ["time", "set", "night"] });
    expect(parse("/time set day")).toEqual({ kind: "command", words: ["time", "set", "day"] });
    expect(parse("/weather rain")).toEqual({ kind: "command", words: ["weather", "rain"] });
    for (const name of ["stop", "kill", "seed", "help"]) expect(parse(`/${name}`)).toEqual({ kind: "command", words: [name] });
  });

  it("says what has no slash", () => {
    expect(parse("can i have op")).toEqual({ kind: "said", text: "can i have op" });
  });

  it("does not know a command it was not given, pointing at the start of it", () => {
    expect(parse("/foo bar")).toEqual({ kind: "error", message: COPY.chat.unknown, input: "foo bar", at: 0 });
    expect(parse("/")).toEqual({ kind: "error", message: COPY.chat.unknown, input: "", at: 0 });
    expect(parse("/tim")).toEqual({ kind: "error", message: COPY.chat.unknown, input: "tim", at: 0 });
    // (Commands are case and all, as the game's are.)
    expect(parse("/TIME set day")).toMatchObject({ kind: "error", message: COPY.chat.unknown, at: 0 });
  });

  it("calls a command that wants more incomplete, pointing past its end", () => {
    expect(parse("/time set")).toEqual({ kind: "error", message: COPY.chat.unknown, input: "time set", at: 8 });
    expect(parse("/weather")).toEqual({ kind: "error", message: COPY.chat.unknown, input: "weather", at: 7 });
  });

  it("calls a word that does not follow the ones before it an incorrect argument, pointing at it", () => {
    expect(parse("/time set noon")).toEqual({ kind: "error", message: COPY.chat.incorrect, input: "time set noon", at: 9 });
    expect(parse("/weather snow")).toEqual({ kind: "error", message: COPY.chat.incorrect, input: "weather snow", at: 8 });
    expect(parse("/stop now")).toEqual({ kind: "error", message: COPY.chat.incorrect, input: "stop now", at: 5 });
  });

  it("is sent without the spaces round it, and any run of them inside it as one", () => {
    expect(normalize("  /time   set  night ")).toBe("/time set night");
    expect(parse(normalize("/time  set night"))).toMatchObject({ kind: "command" });
  });
});

describe("where an error points", () => {
  it("shows the command up to there in grey, what follows red and underlined, and the game's pointer", () => {
    expect(pointAt("time set noon", 9)).toEqual([
      { text: "time set ", color: GREY },
      { text: "noon", color: RED, underline: true },
      { text: COPY.chat.here, color: RED },
    ]);
    expect(pointAt("foo bar", 0)).toEqual([
      { text: "foo bar", color: RED, underline: true },
      { text: COPY.chat.here, color: RED },
    ]);
    expect(pointAt("time set", 8)).toEqual([
      { text: "time set", color: GREY },
      { text: COPY.chat.here, color: RED },
    ]);
  });

  it("shows no more than ten letters before it, after an ellipsis", () => {
    expect(pointAt("weather rain please", 13)[0]).toEqual({ text: "...ther rain ", color: GREY });
  });
});

describe("the game's suggestions", () => {
  it("are every command for a slash alone, in the order of the alphabet", () => {
    expect(suggest("/")).toEqual({ start: 1, end: 1, words: ["help", "kill", "seed", "stop", "time", "weather"] });
  });

  it("are the words that begin with what is typed, in either case", () => {
    expect(suggest("/t")).toEqual({ start: 1, end: 2, words: ["time"] });
    expect(suggest("/ti")).toEqual({ start: 1, end: 3, words: ["time"] });
    expect(suggest("/s")).toEqual({ start: 1, end: 2, words: ["seed", "stop"] });
    expect(suggest("/w")).toEqual({ start: 1, end: 2, words: ["weather"] });
    expect(suggest("/TI")).toEqual({ start: 1, end: 3, words: ["time"] });
  });

  it("leave out the word as it is typed already, which there is nothing to add to", () => {
    expect(suggest("/time")).toBeNull();
    expect(suggest("/help")).toBeNull();
    expect(suggest("/time set day")).toBeNull();
    expect(suggest("/TIME")).toEqual({ start: 1, end: 5, words: ["time"] }); // (the word as the chat has it)
  });

  it("are what may follow the words before, from where the word begins", () => {
    expect(suggest("/time ")).toEqual({ start: 6, end: 6, words: ["set"] });
    expect(suggest("/time s")).toEqual({ start: 6, end: 7, words: ["set"] });
    expect(suggest("/time set ")).toEqual({ start: 10, end: 10, words: ["day", "night"] });
    expect(suggest("/time set n")).toEqual({ start: 10, end: 11, words: ["night"] });
    expect(suggest("/weather ")).toEqual({ start: 9, end: 9, words: ["clear", "rain"] });
    expect(suggest("/weather r")).toEqual({ start: 9, end: 10, words: ["rain"] });
  });

  it("are nothing for what is not a command, or after a word the chat does not know", () => {
    expect(suggest("hello")).toBeNull();
    expect(suggest("/foo ")).toBeNull();
    expect(suggest("/x")).toBeNull();
    expect(suggest("/stop ")).toBeNull();
  });

  it("are for the word the cursor is in, as typed up to it, and reach to the end of that word", () => {
    expect(suggest("/time set day", 6)).toEqual({ start: 6, end: 9, words: ["set"] });
    expect(suggest("/time set day", 3)).toEqual({ start: 1, end: 5, words: ["time"] });
    expect(suggest("/time set day", 11)).toEqual({ start: 10, end: 13, words: ["day"] });
    expect(suggest("/time set day", 5)).toBeNull(); // ("time" is whole there)
  });
});

describe("what a command takes, over the field", () => {
  it("is its line of /help while the command wants more", () => {
    expect(usage("/weather")).toBe("/weather <clear|rain>");
    expect(usage("/weather ")).toBe("/weather <clear|rain>");
    expect(usage("/time")).toBe("/time set <day|night>");
    expect(usage("/time set")).toBe("/time set <day|night>");
    expect(usage("/time set ")).toBe("/time set <day|night>");
  });

  it("is its line of /help where a word after its name is wrong", () => {
    expect(usage("/weather snow")).toBe("/weather <clear|rain>");
    expect(usage("/time sat")).toBe("/time set <day|night>");
    expect(usage("/stop now")).toBe("/stop");
  });

  it("is the names the chat knows, for a name it does not", () => {
    expect(usage("/foo")).toBe("/help /kill /seed /stop /time /weather");
    expect(usage("/foo bar")).toBe("/help /kill /seed /stop /time /weather");
    expect(usage("/TIME")).toBe("/help /kill /seed /stop /time /weather");
  });

  it("is nothing for a slash alone, a command that can run, what is not a command, or with the cursor inside the line", () => {
    expect(usage("/")).toBeNull();
    expect(usage("/kill")).toBeNull();
    expect(usage("/kill ")).toBeNull();
    expect(usage("/time set day")).toBeNull();
    expect(usage("hello")).toBeNull();
    expect(usage("/weather", 3)).toBeNull();
  });

  it("are all of /help's lines, for the commands /help lists", () => {
    for (const name of ["help", "kill", "seed", "stop", "time", "weather"]) expect(COPY.chat.help.filter((said) => said.split(" ")[0] === `/${name}`)).toHaveLength(1);
  });
});

describe("the chat's field", () => {
  const opened = (text = "/") => {
    const field = new Field();
    field.open(text);
    return field;
  };

  it("opens with the slash typed, and no suggestions until something is typed or Tab is pressed (as the game's chat opens)", () => {
    const field = opened();
    expect([field.text, field.cursor, field.hints, field.usage]).toEqual(["/", 1, null, null]);
    field.type("t");
    expect(field.hints).toMatchObject({ start: 1, words: ["time"], picked: 0 });
    field.erase(1);
    expect(field.hints?.words).toEqual(["help", "kill", "seed", "stop", "time", "weather"]);
    const tabbed = opened();
    tabbed.tab();
    expect([tabbed.text, tabbed.hints?.words.length, tabbed.hints?.picked, tabbed.rest]).toEqual(["/", 6, 0, "help"]); // (shown, not put in)
    tabbed.tab();
    expect(tabbed.text).toBe("/help");
  });

  it("goes from a slash to a command run, a word at a time", () => {
    const field = opened();
    field.type("ti");
    expect([field.hints?.words, field.rest]).toEqual([["time"], "me"]);
    field.tab();
    expect([field.text, field.cursor, field.rest]).toEqual(["/time", 5, ""]);
    field.type(" ");
    expect(field.hints).toMatchObject({ start: 6, words: ["set"], picked: 0 });
    expect(field.rest).toBe("set");
    field.tab();
    field.type(" ");
    expect(field.hints).toMatchObject({ start: 10, words: ["day", "night"], picked: 0 });
    field.step(1);
    expect(field.rest).toBe("night");
    field.tab();
    expect([field.text, field.cursor]).toEqual(["/time set night", 15]);
    expect(parse(field.send())).toEqual({ kind: "command", words: ["time", "set", "night"] });
  });

  it("finishes a command's name with Tab, and shows the rest of it faint until then", () => {
    const field = opened();
    field.type("he");
    expect(field.rest).toBe("lp");
    field.tab();
    expect([field.text, field.cursor, field.rest]).toEqual(["/help", 5, ""]);
  });

  it("puts the next suggestion in at each Tab after the first, and the one before with Shift", () => {
    const field = opened();
    field.type("s");
    field.tab();
    expect(field.text).toBe("/seed");
    field.tab();
    expect(field.text).toBe("/stop");
    field.tab();
    expect(field.text).toBe("/seed");
    field.tab(true);
    expect(field.text).toBe("/stop");
  });

  it("finishes the words after a command's name too", () => {
    const field = opened();
    field.type("time ");
    field.tab();
    expect(field.text).toBe("/time set");
    field.type(" n");
    field.tab();
    expect(field.text).toBe("/time set night");
  });

  it("goes through the suggestions with the arrows while they show, without putting one in", () => {
    const field = opened();
    field.tab();
    field.step(-1);
    expect(field.hints?.picked).toBe(5); // (round to the last)
    field.step(1);
    field.step(1);
    expect(field.hints?.picked).toBe(1);
    expect(field.text).toBe("/");
    expect(field.rest).toBe("kill");
    field.tab();
    expect(field.text).toBe("/kill"); // (the one the arrows picked, not the one after it)
  });

  it("puts the suggestions away on Esc, and shows them again as the line changes or on Tab", () => {
    const field = opened();
    field.type("weather ");
    expect(field.usage).toBeNull(); // (the suggestions say it)
    field.hide();
    expect([field.hints, field.rest, field.usage]).toEqual([null, "", "/weather <clear|rain>"]);
    field.move(3);
    expect(field.hints).toBeNull(); // (the cursor does not bring them back)
    field.move(99);
    field.tab();
    expect(field.hints?.words).toEqual(["clear", "rain"]);
    field.hide();
    field.type("r");
    expect(field.hints?.words).toEqual(["rain"]);
  });

  it("goes back through the lines sent with the arrows once Esc has put the suggestions away", () => {
    const field = opened();
    field.type("seed");
    field.send();
    field.open("/");
    field.type("s");
    field.step(-1);
    expect([field.text, field.hints?.picked]).toEqual(["/s", 1]);
    field.hide();
    field.step(-1);
    expect(field.text).toBe("/seed");
  });

  it("shows the suggestions again on Tab, where there were none showing", () => {
    const field = opened("/ki");
    field.step(-1); // (nothing sent yet: nothing to go back to)
    expect(field.hints).toBeNull();
    field.tab();
    expect(field.hints?.words).toEqual(["kill"]);
    expect(field.text).toBe("/ki");
    field.tab();
    expect(field.text).toBe("/kill");
  });

  it("shows nothing for a command typed whole, and what a command takes where it wants more", () => {
    const field = opened();
    field.type("time");
    expect([field.hints, field.usage]).toEqual([null, "/time set <day|night>"]);
    field.tab(); // (nothing to finish)
    expect([field.text, field.hints]).toEqual(["/time", null]);
    field.type(" set day");
    expect([field.hints, field.usage]).toEqual([null, null]);
    field.erase(0);
    field.type("/foo");
    expect(field.usage).toBe("/help /kill /seed /stop /time /weather");
  });

  it("suggests for the word the cursor is moved into, and puts a suggestion in in place of that whole word", () => {
    const field = opened();
    field.type("time set day");
    expect(field.hints).toBeNull();
    field.move(3);
    expect(field.hints).toMatchObject({ start: 1, end: 5, words: ["time"] });
    expect(field.rest).toBe(""); // (the rest shows only at the end of the line)
    field.tab();
    expect([field.text, field.cursor]).toEqual(["/time set day", 5]);
    field.move(8);
    expect(field.hints).toMatchObject({ start: 6, words: ["set"] });
    field.move(field.text.length);
    expect(field.hints).toBeNull();
    field.erase(0);
    field.type("/stop");
    field.move(2);
    expect(field.hints?.words).toEqual(["seed", "stop"]);
    field.step(1);
    field.tab();
    expect([field.text, field.cursor]).toEqual(["/stop", 5]);
    field.tab(true);
    expect(field.text).toBe("/seed");
  });

  it("puts a suggestion clicked in, as Tab does", () => {
    const field = opened();
    field.type("s");
    field.pick(1);
    field.use();
    expect([field.text, field.hints?.tabbed]).toEqual(["/stop", true]);
    field.tab();
    expect(field.text).toBe("/seed");
  });

  it("keeps the lines sent for ↑ and ↓, and what was being typed", () => {
    const field = opened();
    field.type("help");
    expect(field.send()).toBe("/help");
    field.open("/");
    field.erase(0);
    field.type("hello  there ");
    expect(field.send()).toBe("hello there");
    field.open("/");
    field.erase(0);
    field.type("half");
    field.step(-1);
    expect(field.text).toBe("hello there");
    expect(field.hints).toBeNull(); // (no suggestions for a line brought back, until it is changed)
    field.step(-1);
    expect(field.text).toBe("/help");
    expect(field.hints).toBeNull();
    field.step(-1);
    expect(field.text).toBe("/help"); // (the oldest)
    field.step(1);
    field.step(1);
    expect(field.text).toBe("half");
    field.type("!");
    expect(field.text).toBe("half!");
  });

  it("keeps a line sent twice in a row once", () => {
    const field = opened();
    field.type("seed");
    field.send();
    field.open("/seed");
    field.send();
    field.open("");
    field.step(-1);
    field.step(-1);
    expect(field.text).toBe("/seed");
    field.step(1);
    expect(field.text).toBe("");
  });

  it("edits at the cursor, a letter or a word at a time", () => {
    const field = opened("/time set day");
    field.move(field.word(-1));
    expect(field.cursor).toBe(10);
    field.erase(field.word(-1));
    expect([field.text, field.cursor]).toEqual(["/time day", 6]);
    field.erase(field.cursor + 1);
    expect(field.text).toBe("/time ay");
    field.move(0);
    field.erase(-1); // (nothing before the start)
    expect(field.text).toBe("/time ay");
    field.move(99);
    expect(field.cursor).toBe(field.text.length);
  });

  it("takes no more than the game's chat does", () => {
    const field = opened();
    field.type("x".repeat(LONGEST + 20));
    expect(field.text.length).toBe(LONGEST);
  });
});

describe("the seed", () => {
  it("is the bundle's version, its 64 bits read as the game reads a seed", () => {
    expect(seedOf("9fde4f200e3d8087")).toBe("-6927012177752784761");
    expect(seedOf("0000000000000001")).toBe("1");
    expect(seedOf("7fffffffffffffff")).toBe("9223372036854775807");
  });
});

describe("a line of chat too long for the frame", () => {
  // (Every letter of this face is two pixels wide: three letters and their gaps are eight pixels.)
  const draw = new Draw({ rows: 8, glyphs: Object.fromEntries([..."abcdefgh <>-[]"].map((ch) => [ch, ["##"]])) });
  const text = (line: { text: string }[]) => line.map((span) => span.text).join("");

  it("breaks at a space where it can, and the space goes", () => {
    expect(draw.breakLine("abc def gh", 20).map(text)).toEqual(["abc def", "gh"]);
  });

  it("breaks inside a word too long for a line of its own", () => {
    expect(draw.breakLine("abcdefghabcdefgh", 23).map(text)).toEqual(["abcdefgh", "abcdefgh"]);
  });

  it("keeps each stretch's style across the break", () => {
    const [one, two] = draw.breakLine([{ text: "ab " }, { text: "cd ef", color: RED, underline: true }, { text: "<--", color: RED }], 20);
    expect(one).toEqual([{ text: "ab " }, { text: "cd", color: RED, underline: true }]);
    expect(two).toEqual([{ text: "ef", color: RED, underline: true }, { text: "<--", color: RED }]);
  });

  it("is one line where it fits", () => {
    expect(draw.breakLine("abc", 100)).toEqual([[{ text: "abc" }]]);
  });
});
