/**
 * The game's keys on the page (lib/landing/film/debug.ts, film.ts): the debug screen says
 * where the camera is on the game's axes and in its words, lays its two columns out for the
 * frame, and a screenshot is named as the game names one.
 */

import { describe, expect, it } from "vitest";
import { Draw } from "../../../lib/landing/engine/hud";
import { screenshotName } from "../../../lib/landing/film/dates";
import { arrange, facing, gameAxes, whereLines, wrapDegrees, yawPitch } from "../../../lib/landing/film/debug";

const v = (x: number, y: number, z: number) => ({ x, y, z });

describe("the game's axes", () => {
  it("are the film's turned Y up: east stays east, up is Y, north is negative Z", () => {
    expect(gameAxes(v(1, 2, 3))).toEqual([1, 3, -2]);
    expect(gameAxes(v(0, 1, 0))).toEqual([0, 0, -1]); // north
    expect(gameAxes(v(0, 0, 1))).toEqual([0, 1, -0]); // up
  });

  it("turn a way to look into the game's yaw and pitch", () => {
    const close = ([yaw, pitch]: [number, number], want: [number, number]) => {
      expect(yaw).toBeCloseTo(want[0], 6);
      expect(pitch).toBeCloseTo(want[1], 6);
    };
    close(yawPitch(v(0, -1, 0)), [0, 0]); // south
    close(yawPitch(v(-1, 0, 0)), [90, 0]); // west
    close(yawPitch(v(0, 1, 0)), [-180, 0]); // north (180 is -180, as the game has it)
    close(yawPitch(v(1, 0, 0)), [-90, 0]); // east
    expect(yawPitch(v(0, 0, -1))[1]).toBeCloseTo(90, 6); // straight down is positive (and has no yaw of its own)
    close(yawPitch(v(0, 1, 1)), [-180, -45]); // north and up
  });

  it("wrap angles into -180..180 as the game does", () => {
    expect(wrapDegrees(180)).toBe(-180);
    expect(wrapDegrees(-180)).toBe(-180);
    expect(wrapDegrees(270)).toBe(-90);
    expect(wrapDegrees(-190)).toBe(170);
    expect(wrapDegrees(725)).toBe(5);
  });
});

describe("the facing words", () => {
  it("name the nearest of the four ways, as the game rounds a yaw", () => {
    expect([0, 90, 180, -180, -90, 270].map(facing)).toEqual(["south", "west", "north", "north", "east", "east"]);
    expect([44.9, 45, 134.9, 135, -45, -45.1].map(facing)).toEqual(["south", "west", "west", "north", "south", "east"]);
  });
});

describe("where the debug screen says the camera is", () => {
  it("says it in the game's lines", () => {
    expect(whereLines(v(1.5, -2.25, 64), v(0, 1, 0))).toEqual([
      "XYZ: 1.500 / 64.00000 / 2.250",
      "Block: 1 64 2 [1 0 2]",
      "Chunk: 0 4 0 [0 0 in r.0.0.mca]",
      "Facing: north (Towards negative Z) (-180.0 / 0.0)",
    ]);
  });

  it("counts blocks, chunks and regions below zero as the game does", () => {
    expect(whereLines(v(-17.2, 40.5, -30.1), v(1, 0, 0)).slice(1)).toEqual([
      "Block: -18 -31 -41 [14 1 7]",
      "Chunk: -2 -2 -3 [30 29 in r.-1.-1.mca]",
      "Facing: east (Towards positive X) (-90.0 / 0.0)",
    ]);
  });

  it("says which way it faces without its axis, in a frame too narrow for all of it", () => {
    expect(whereLines(v(0, 0, 0), v(-1, 0, -1), true)[3]).toBe("Facing: west (90.0 / 45.0)");
  });
});

describe("the debug screen's columns", () => {
  const width = (line: string | readonly { text: string }[]) => (typeof line === "string" ? line.length : line.map((span) => span.text).join("").length);
  const left = ["aaaa", "bbbbbbbbbb", "", "cc"];
  const right = ["xx", "yyyyyyyyyy", "zz", "", "server"];
  const more = ["", "server"];

  it("stand side by side, a right-hand line left out where it would meet the left's", () => {
    const [one, two] = arrange(left, right, more, 30, width, false);
    expect(one).toEqual(left);
    // (Row 2: 10 + 3 and 10 + 3, with room between, is too wide for 30.)
    expect(two).toEqual(["xx", "", "zz", "", "server"]);
  });

  it("are one column in a narrow frame, without what does not fit", () => {
    const [one, two] = arrange([...left, "a line far too wide for it"], right, more, 20, width, true);
    expect(one).toEqual([...left, "", "server"]);
    expect(two).toEqual([]);
  });
});

describe("a line in spans", () => {
  // (Every letter of this face is two pixels wide.)
  const draw = new Draw({ rows: 8, glyphs: Object.fromEntries([..."abc ?[]:"].map((ch) => [ch, ["##"]])) });

  it("is as wide as its letters, a bold one a pixel wider", () => {
    expect(draw.widthOf("abc")).toBe(draw.measure("abc"));
    expect(draw.widthOf([{ text: "ab" }, { text: "c" }])).toBe(draw.measure("abc"));
    expect(draw.widthOf([{ text: "[a]:", bold: true }, { text: " b" }])).toBe(draw.measure("[a]: b") + 4);
  });
});

describe("a screenshot's name", () => {
  it("is the moment it was taken, in local time, as the game names one", () => {
    expect(screenshotName(new Date(2026, 9, 5, 7, 3, 9))).toBe("2026-10-05_07.03.09.png");
    expect(screenshotName(new Date(2026, 11, 31, 23, 59, 58))).toBe("2026-12-31_23.59.58.png");
  });
});
