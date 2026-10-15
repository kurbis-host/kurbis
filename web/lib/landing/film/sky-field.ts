/**
 * The name field in the sky: under the hero's caption, `____.KURBIS.HOST`, to be typed
 * into from the first frame. While it is empty, names type themselves into it, ghosted;
 * a gold stretch of its underline blinks where the next letter goes. What the page finds
 * out about the name shows under it, and the cue steps aside. Once the claim card holds the
 * name, the line gives way to the card where the card would lie on it.
 */

import { Box3, Object3D, Vector3, type Material, type Mesh } from "three";
import { POP, UNPOP } from "../anim/easing";
import { grow } from "../anim/timeline";
import type { Palette } from "../engine/bundle";
import type { Card } from "../engine/hud";
import { lineMask, scrimMesh } from "../engine/type";
import { COPY } from "../copy";
import type { Ctx } from "./context";
import { REMOVED, REMOVED_FOR, SYSTEM, eggOf } from "./eggs";
import { typedLine } from "./name-line";
import type { NameField, Verdict } from "./nether";
import { makeSplash, throb, touchSplash } from "./splash";

/** The placeholder: plum faded into the cloud it stands on. */
const GHOST: Palette = { front: [0.56, 0.3, 0.44], top: [0.64, 0.38, 0.5], side: [0.42, 0.2, 0.32], bottom: [0.3, 0.12, 0.22] };
/** Seconds a letter of the placeholder takes to type, and to delete; how long a whole name rests. */
const KEY = 0.11;
const BACK = 0.045;
const REST = 1.3;
/** The splash's tilt (./splash.ts), and how much nearer the lens than the field it and the messages are in the tall frame (in the field's terms: a block). */
const TILT = (20 * Math.PI) / 180;
const NEARER = 2;

export interface SkyField extends NameField {
  /** Whether the field is in sight (and can be typed in). */
  readonly open: boolean;
  /** Whether a free name's splash is up (on Halloween the title's own steps aside for it: ./halloween.ts). */
  readonly splashing: boolean;
}

export function setSkyField(ctx: Ctx): SkyField {
  const { tl, C, stage, tall } = ctx;
  const cue = ctx.node("Hero_Cue");

  // The field stands out of the cloud's face under the caption. The art put its place there (the middle
  // of its line's foot) and said its cell, and how many cells a name has before it is set smaller.
  const field = ctx.node("Hero_Field");
  const { cell, room, depth } = JSON.parse(field.userData.kurbis as string) as { cell: number; room: number; depth: number };
  const front = -depth * cell; // (where the letters' faces are)

  // One line, centred on the cloud: the name's slot, underlined, and the host. The slot holds the name and
  // the cursor after it, and is never narrower than the longest name the placeholder types, so the line
  // stands still while it types them. A longer name widens it (up to its room; then the name is set
  // smaller), and the line re-centres.
  const face = stage.data.glyphs.block;
  const widthOf = (text: string) => lineMask(face, text)[0].length;
  const host = COPY.surface.host.toUpperCase();
  const HOST = widthOf(host);
  const CURSOR = 7; // (a cell, then the cursor's six)
  const least = Math.max(...COPY.surface.names.map((name) => widthOf(name.toUpperCase()))) + CURSOR;
  let slot = least;
  let left = 0; // where the line begins and ends
  let right = 0;
  const line = new Object3D(); // all of it, to give way to the claim card at once (about the middle of its foot)
  field.add(line);
  const row = new Object3D(); // what is typed, from the slot's left end
  row.position.y = front;
  line.add(row);
  const style = { cell, depth, day: 1, left: 0, line: 0, room };
  const typed = typedLine(ctx, row, { ...style, palette: "ink" });
  const ghost = typedLine(ctx, row, { ...style, palette: GHOST });
  const hostType = stage.makeType([host], { palette: "plum", cell, depth, day: 1 });
  hostType.position.y = front;
  line.add(hostType);
  const dashes = Math.ceil((room + CURSOR) / 6);
  const underline = stage.makeType(["_".repeat(dashes)], { palette: "plum", cell, depth: 2, day: 1, tracking: 0 });
  underline.position.set(0, -2 * cell, -3 * cell);
  line.add(underline);
  const cursor = stage.makeType(["_"], { palette: "gold", cell, depth: 3, day: 1 });
  line.add(cursor);
  /** The line laid out for a slot this many cells wide. */
  const lay = (cells: number) => {
    left = -((cells + 1 + HOST) / 2) * cell;
    right = -left;
    row.position.x = left;
    hostType.position.x = right - (HOST / 2) * cell;
    underline.position.x = left + (cells / 2) * cell;
    underline.scale.x = cells / (dashes * 6);
  };
  lay(slot);

  let focused = false;
  let open = false;
  let verdict: Verdict | "removed" = null;
  let judged = -Infinity;
  let answered = -Infinity; // (when it first answered for the name as it stands)
  let cleared = -Infinity;
  let held: { verdict: Verdict; at: number } | null = null; // what it found, while it says something else first
  let message: Mesh | null = null;
  let shown: Mesh | null = null; // what answers for the name now: the message, or the splash
  let messageFor = "";
  let since = 0; // when the placeholder last started over
  const said = new Vector3();
  const cueBox = new Box3();
  let typedOnce = false;
  const touchOnly = typeof window !== "undefined" && window.matchMedia("(hover: none)").matches;
  let hintCard: Card | null = null;
  let hint = 0;
  let carded = 0; // how far the claim card has come up over the splash
  let given = false; // the line has given way to the claim card
  let gone = 0; // and how far it has gone
  const say =(lines: string[], palette: string | Palette) => {
    if (message) {
      field.remove(message);
      message.geometry.dispose();
      for (const child of message.children as Mesh[]) {
        child.geometry.dispose();
        (child.material as Material).dispose();
      }
    }
    message = stage.makeType(lines, { palette, cell: (tall ? 0.03 : 0.022) * 1.1, depth: 3, day: 1 });
    // On a soft dark card, so it reads over the leaves whatever its colour.
    message.geometry.computeBoundingBox();
    const size = message.geometry.boundingBox!.getSize(new Vector3());
    const card = scrimMesh(size.x * 1.04, size.z * 1.5, size.z * 0.9, 0.62);
    card.position.set(0, 0.03, size.z / 2);
    card.renderOrder = 5;
    message.add(card);
    // Just under the middle of the line (where the wide frame's cue is: it steps aside), in the tall frame nearer the
    // lens, as the splash is.
    message.position.set(0, tall ? -NEARER : front, -3 * cell - 0.05 - size.z);
    field.add(message);
  };
  const SAYS: Record<Exclude<Verdict, "free" | null> | "removed", (name: string) => [string[], string | Palette]> = {
    taken: () => [["TAKEN. TRY ANOTHER."], "ember"],
    mine: () => [["WELCOME BACK."], "lime"],
    short: () => [["AT LEAST 3 CHARACTERS."], "ember"],
    dash: () => [["NO DASH AT THE START OR END."], "ember"],
    removed: () => [[REMOVED], SYSTEM],
  };
  // A free name gets the splash instead, as the game's leans off its logo's corner: from the host's last letter, just
  // grazing it, up to the right. The wide frame has no room under the line; the tall one has no room beside it, and has
  // it under the line, nearer the middle, and a block nearer the lens, over anything the perch has in front of its face
  // there (the island's cloud). On a soft dark card: it crosses the cloud and the leaves.
  const splash = makeSplash(stage, tall ? 0.03 : 0.018, { words: COPY.splash.toUpperCase(), day: 1, scrim: 0.62 }); // (in capitals, as it always was)
  const reach = splash.geometry.boundingBox!.max.x; // (half its length: it is laid out from its middle)
  const splashAt = (end: number) => {
    if (tall) splash.position.set(end - 1.78, -NEARER, -0.7);
    else splash.position.set(end - 3 * cell + reach * Math.cos(TILT), front - 0.1, reach * Math.sin(TILT) - cell);
  };
  splashAt(right);
  field.add(splash);
  // Pointed at, it says what is under the hood (./splash.ts): over its high end, and over the line, which in the tall
  // frame is over the splash.
  const top = new Vector3();
  touchSplash(ctx, splash, {
    name: "sky_splash",
    seen: [0, C.WHITE[1]],
    side: "over",
    keep: () => {
      field.updateWorldMatrix(true, false);
      return (field.localToWorld(top.set(right, front, 10 * cell)).project(stage.camera).y * 0.5 + 0.5) * (ctx.ui?.hud.height ?? 0); // (the top of the field's box)
    },
  });
  /** Answer for the name as it stands (null: nothing is known of it yet). */
  const tell = (next: Verdict | "removed", time: number) => {
    const text = typed.text;
    if (next === null) cleared = time;
    else {
      if (verdict === null) answered = time;
      if (shown) grow(shown, 0); // (what it said first, if it said anything)
      splash.visible = false;
      if (next === "free") shown = splash;
      else {
        const [lines, palette] = SAYS[next](text);
        say(lines, palette);
        shown = message;
      }
      judged = time;
      messageFor = text;
    }
    verdict = next;
  };

  // The placeholder, typing the names the surface shows, one after another.
  const names = COPY.surface.names;
  const per = names.map((name) => name.length * KEY + REST + name.length * BACK + 0.35);
  const cycle = per.reduce((a, b) => a + b, 0);
  const placeholder = (time: number) => {
    let t = (time - since) % cycle;
    let i = 0;
    while (t >= per[i]) t -= per[i++];
    const name = names[i];
    const typedIn = t / KEY;
    if (typedIn <= name.length) return name.slice(0, Math.floor(typedIn));
    const back = (t - name.length * KEY - REST) / BACK;
    return back <= 0 ? name : name.slice(0, Math.max(name.length - Math.floor(back), 0));
  };

  tl.during(0, 1.2, ({ s, time, dt }) => {
    const there = s < C.WHITE[1] + 0.01;
    open = s < C.WAKE_UP - 0.05; // the field takes names while the hero is up
    field.visible = there;
    if (!there) return;

    const empty = !typed.text;
    ghost.type(empty && !focused ? placeholder(time) : "", time);
    const cells = typed.lay(time, true);
    const ghostCells = ghost.lay(time, true);
    const at = empty ? ghostCells : cells;
    // The slot follows the name, and the line re-centres, easing as the surface's address does.
    slot += (Math.max(at + CURSOR, least) - slot) * (1 - Math.exp(-22 * dt));
    lay(slot);
    splashAt(right);
    // The cursor: a gold stretch of the underline where the next letter goes, blinking. (A hair over the
    // underline, so their tops do not fight.)
    cursor.position.set(left + (at + 4) * cell, front, 0.002 - 3 * cell); // (centred on the cell after the last letter, as the underline lies)
    cursor.visible = (focused || empty) && Math.floor(time * 1.8) % 2 === 0;

    const ui = ctx.ui;
    if (ui) {
      const box = open ? frameBox() : null;
      // Until something is typed, a word on how: hung from where the next letter goes.
      const wanted = !!box && empty && !focused && !typedOnce && ui.claimed === null && time > 1.2;
      hint = wanted ? Math.min(hint + dt / 0.25, 1) : Math.max(hint - dt / 0.15, 0);
      if (hint > 0) {
        hintCard ??= ui.draw.tooltip(null, [touchOnly ? COPY.sky.tapHint : COPY.sky.typeHint]);
        // Under the line's left end. Should it reach the cue (a narrow window, where the cue is beside it), the cue steps
        // aside while it shows. (The tall frame's cue is well under it: both stay.)
        field.updateWorldMatrix(true, false);
        const at = field.localToWorld(said.set(left, front, -5 * cell)).project(stage.camera);
        const x = (at.x * 0.5 + 0.5) * ui.hud.width;
        const y = (at.y * 0.5 + 0.5) * ui.hud.height;
        ui.tips.push({ card: hintCard, at: [x, y], anchor: [0, 1], alpha: hint });
        const near = cueBox.setFromObject(cue);
        const [low, high] = [near.min.project(stage.camera), near.max.project(stage.camera)];
        const across = [low.x, high.x].map((v) => (v * 0.5 + 0.5) * ui.hud.width);
        const up = [low.y, high.y].map((v) => (v * 0.5 + 0.5) * ui.hud.height);
        const meets = Math.min(...across) < x + hintCard.width + 4 && Math.max(...across) > x - 4 && Math.min(...up) < y + 4 && Math.max(...up) > y - hintCard.height - 4;
        if (!shown && meets && cue.visible) grow(cue, 1 - hint);
      }
    }

    // What the page says about the name takes the cue's place.
    if (held && time >= held.at) {
      const { verdict: found, at } = held;
      held = null;
      tell(found, at); // (as of when it was due: a field out of sight then shows it as long said)
    }
    const told = verdict !== null;
    const answer = told ? POP((time - judged - 0.05) / 0.28) : UNPOP((time - cleared) / 0.15);
    // The splash has said its piece once the claim card is up with the name (which, here, would lie on it): it goes.
    carded = ctx.ui && ctx.ui.cardTop > 0 ? Math.min(carded + dt / 0.15, 1) : Math.max(carded - dt / 0.15, 0);
    if (shown) grow(shown, answer * (shown === splash ? throb(time) * UNPOP(carded) : 1));
    // So has the line, where the card would lie on it (the wide frame's corner reaches its end as the hero goes by): it
    // gives way to the card, which holds the name now, and is back once the card has gone.
    const card = ctx.ui?.cardBox ?? null;
    if (!card) given = false;
    else if (!given) given = under(card);
    gone = given ? Math.min(gone + dt / 0.15, 1) : Math.max(gone - dt / 0.28, 0);
    grow(line, UNPOP(gone));
    // (After the valley has grown the hero: the cue steps aside for the answer.)
    if (shown) grow(cue, told ? UNPOP((time - answered + 0.1) / 0.15) : POP((time - cleared - 0.1) / 0.28));
  });

  const corner = new Vector3();
  /** Whether the line (its letters, from the underline up) lies under a box of the interface's: [left, bottom, right, top]. */
  const under = ([l, b, r, t]: readonly number[]) => {
    const { width, height } = ctx.ui!.hud;
    field.updateWorldMatrix(true, false);
    const from = field.localToWorld(corner.set(left, front, -3 * cell)).project(stage.camera);
    const [x0, y0] = [(from.x * 0.5 + 0.5) * width, (from.y * 0.5 + 0.5) * height];
    const to = field.localToWorld(corner.set(right, front, face.rows * cell)).project(stage.camera);
    return x0 < r && (to.x * 0.5 + 0.5) * width > l && y0 < t && (to.y * 0.5 + 0.5) * height > b;
  };
  /** Where the field's line is on screen, as shares of it from the top left. */
  const frameBox = () => {
    stage.camera.updateMatrixWorld();
    field.updateWorldMatrix(true, false);
    const a = field.localToWorld(corner.set(left - 3 * cell, front, -5 * cell)).project(stage.camera).clone();
    const b = field.localToWorld(corner.set(right + 3 * cell, front, 10 * cell)).project(stage.camera);
    return { left: a.x * 0.5 + 0.5, top: 0.5 - b.y * 0.5, width: (b.x - a.x) * 0.5, height: (b.y - a.y) * 0.5 };
  };
  return {
    get open() {
      return open;
    },
    get splashing() {
      return verdict === "free" && carded < 1;
    },
    box() {
      return open && field.visible ? frameBox() : null;
    },
    focus(on) {
      focused = on;
      if (!on) since = tl.frame.time; // the placeholder starts over from its first name
    },
    type(next, time) {
      if (next) typedOnce = true;
      typed.type(next, time);
    },
    judge(next, time) {
      const text = typed.text;
      if (next === (held ? held.verdict : verdict) && messageFor === text) return;
      held = null;
      // Herobrine: it says "Removed." first (./eggs.ts), and what it found after.
      if (next !== null && eggOf(text) === "removed") {
        held = { verdict: next, at: time + REMOVED_FOR };
        tell("removed", time);
      } else tell(next, time);
    },
    enter: () => false,
  };
}
