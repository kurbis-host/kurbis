/**
 * The Nether: out of the portal on its ledge, a short bridge, and a second frame that
 * waits unlit under the name field. The end of the page: a name is typed, it is free,
 * the frame lights, and the pumpkin leaps through.
 */

import { Mesh, Vector3 } from "three";
import { setPose } from "../anim/actor";
import { POP, UNPOP, clamp01, lerp, smooth, wrap } from "../anim/easing";
import { grow } from "../anim/timeline";
import type { Palette } from "../engine/bundle";
import type { Card } from "../engine/hud";
import { COPY } from "../copy";
import { vec, type Ctx } from "./context";
import { REMOVED, REMOVED_FOR, SYSTEM, eggOf } from "./eggs";
import { typedLine } from "./name-line";
import { makeSplash, throb, touchSplash } from "./splash";

const RAD = Math.PI / 180;
/** Cells the name has room for before it is set smaller, and the host's width, as the art laid the line out. */
const NAME_CELLS = 58;
const HOST_CELLS = 71;

/** What the page has found out about the name that stands in the field. */
export type Verdict = "free" | "taken" | "mine" | "short" | "dash" | null;

export interface NameField {
  /** Where the field is on the canvas, as shares of its width and height from the top left; null while it cannot be typed in. */
  box(): { left: number; top: number; width: number; height: number } | null;
  focus(on: boolean): void;
  /** The whole name as it now stands. */
  type(text: string, time: number): void;
  judge(verdict: Verdict, time: number): void;
  /** Step through. True if the name was free and the field is in sight: the pumpkin leaps, and the page may follow it. */
  enter(time: number): boolean;
}

export function setNether(ctx: Ctx): NameField {
  const { tl, C, stage, tall } = ctx;
  const { lamps } = stage;
  const origin = vec(stage.data.nether.origin);
  const sIn = C.THROUGH - 0.03;
  const lights = lamps.named("Nether_Lamp_");

  // The name field over the second frame, facing back along the bridge.
  const field = ctx.node("Nether_Field");
  const headline = ctx.node("Nether_Headline");
  const placeholder = ctx.node("Nether_Name");
  const host = ctx.node("Nether_Host");
  const bar = ctx.node("Nether_Underline");
  const caption = ctx.node("Nether_Caption");
  const card = ctx.node("Nether_Field_Scrim");
  const popped = { headline: tl.trigger(5.66, 0.28), name: tl.trigger(5.7, 0.28), host: tl.trigger(5.72, 0.28), caption: tl.trigger(5.76, 0.28) };
  // What the art typed for its own film is not ours to show.
  const stale = [ctx.node("Nether_Free"), ...[0, 1, 2, 3, 4].map((i) => ctx.find(`Nether_Typed_${i}`))];
  // The second frame's own portal, dark until a free name lights it.
  const gate = ctx.node("Nether_Gate");
  const sparks = ctx.node("Nether_GateSparks");
  const glows = lamps.named("Nether_GateGlow_");
  // A strider walks the sea far out.
  const walker = ctx.node("Strider");
  const legs = [ctx.node("Strider_leg_r"), ctx.node("Strider_leg_l")];
  walker.rotation.set(0, 0, 90 * RAD); // walking east
  // The other one of the dig's lost sock lies by the second frame, for a visitor who
  // remembers what that one said. It is small: the pointer finds it from a little way off.
  const [sockTitle, sockLine] = COPY.nether.sock;
  let sockTip: Card | null = null;
  ctx.touch.add({
    name: "nether_sock",
    at: vec(ctx.points.nether_sock).add(new Vector3(0, 0, 0.1)),
    half: new Vector3(0.8, 0.8, 0.35),
    seen: [5.9, C.HOME - 0.05],
    pick: true,
    // Under it, on the bare deck: clear of the frame (lit or not) and of the field over it.
    hang: { offset: [0, -18], anchor: [0.5, 1] },
    tip: () => (sockTip ??= ctx.ui?.draw.tooltip(sockTitle, [sockLine]) ?? null),
  });

  // -- the name, as it is typed -------------------------------------------------------------
  const cell = ctx.pick(0.06, 0.09);
  const left = -(ctx.pick(NAME_CELLS + 1 + HOST_CELLS, NAME_CELLS) / 2) * cell; // where the name begins
  const line = ctx.pick(0.72, 2.55); // the name's line, above the foot of the block
  const said = ctx.pick(0.042, 0.06); // the cell of what is said under it
  const typed = typedLine(ctx, field, { palette: "cream", cell, depth: 4, day: 0, left, line, room: NAME_CELLS });
  let focused = false;
  let emptied = 0; // how far the placeholder has gone, 0..1
  let verdict: Verdict | "removed" = null;
  let judged = -Infinity;
  let answered = -Infinity; // (when it first answered for the name as it stands)
  let cleared = -Infinity;
  let held: { verdict: Verdict; at: number } | null = null; // what it found, while it says something else first
  let message: Mesh | null = null;
  let shown: Mesh | null = null; // what answers for the name now: the message, or the splash
  let messageFor = "";
  let entered: number | null = null;
  let hint: Card | null = null;

  const say = (lines: string[], palette: string | Palette) => {
    if (message) {
      field.remove(message);
      message.geometry.dispose();
    }
    message = stage.makeType(lines, { palette, cell: said, depth: 3 });
    field.add(message);
  };
  // (The name stands right above: the answer does not repeat it, so a long one never runs off the frame.)
  const SAYS: Record<Exclude<Verdict, "free" | null> | "removed", (name: string) => [string[], string | Palette]> = {
    taken: () => [["TAKEN. TRY ANOTHER."], "ember"],
    mine: () => [["WELCOME BACK."], "lime"],
    short: () => [tall ? ["AT LEAST", "3 CHARACTERS."] : ["AT LEAST 3 CHARACTERS."], "ember"],
    dash: () => [tall ? ["NO DASH AT THE", "START OR END."] : ["NO DASH AT THE START OR END."], "ember"],
    removed: () => [[REMOVED], SYSTEM],
  };
  // A free name gets the splash instead: on the host's lower right corner (3.9 along, in the wide frame), as the
  // game's is on its logo's, just grazing its last letter (nearer the middle in the tall frame, to stay inside it).
  const splash = makeSplash(stage, ctx.pick(0.03, 0.04), { words: COPY.splash.toUpperCase() }); // (in capitals, as it always was)
  splash.position.set(ctx.pick(3.9, 0.6), -0.15, ctx.pick(0.66, 0.5));
  field.add(splash);
  // Pointed at, it says what is under the hood (./splash.ts): under it, where the frame below has room.
  touchSplash(ctx, splash, { name: "nether_splash", seen: [6.0, C.HOME - 0.05], side: "under" });
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

  // -- the pumpkin's last leap ----------------------------------------------------------------
  const { nether } = ctx.cast;
  const spots = ctx.points.cast.nether_spots.map(vec);
  const waits = spots[spots.length - 1];
  const inside = vec(ctx.points.cast.nether_inside);

  const corner = new Vector3();

  tl.during(sIn - 0.1, C.END, ({ s, time, dt }) => {
    const there = s >= sIn && s < C.HOME; // (home, the Nether is behind us)
    for (const lamp of lights) lamp.energy = there ? lamp.base : 0;
    for (const thing of stale) if (thing) thing.visible = false;
    card.visible = s >= 5.6;
    grow(headline, POP(popped.headline.t / 0.28));
    grow(host, POP(popped.host.t / 0.28));
    // Until a name is typed the field shows a placeholder.
    emptied = focused || typed.text ? Math.min(emptied + dt / 0.15, 1) : Math.max(emptied - dt / 0.15, 0);
    grow(placeholder, POP(popped.name.t / 0.28) * UNPOP(emptied));
    grow(bar, POP(popped.name.t / 0.28));
    bar.scale.z *= focused ? 1.5 + 0.5 * Math.sin(time * 5) : 1; // it stirs while the field has the keyboard

    // The letters, each popping in as it is typed. A long name is set smaller, to fit.
    typed.lay(time, popped.name.t > 0);

    // What the page says about the name takes the caption's place.
    if (held && time >= held.at) {
      const { verdict: found, at } = held;
      held = null;
      tell(found, at); // (as of when it was due: a field out of sight then shows it as long said)
    }
    const told = verdict !== null;
    const answer = told ? POP((time - judged - 0.05) / 0.28) : UNPOP((time - cleared) / 0.15);
    if (shown) grow(shown, answer * (shown === splash ? throb(time) : 1));
    const captioned = told ? UNPOP((time - answered + 0.1) / 0.15) : POP((time - cleared - 0.1) / 0.28);
    grow(caption, POP(popped.caption.t / 0.28) * (shown ? captioned : 1));

    // A name that is free lights the frame under it.
    const free = verdict === "free" || verdict === "mine" ? time - judged : -1; // (their own world opens the frame too)
    grow(gate, free >= 0 ? POP((free - 0.25) / 0.3) : 0);
    sparks.visible = free >= 0.3;
    sparks.userData.grow = clamp01((free - 0.3) / 0.2);
    glows.forEach((lamp, i) => void (lamp.energy = free >= 0 ? [800, 260][i] * clamp01((free - 0.25) / 0.25) : 0));
    if (ctx.ui && s > 6.0 && s < C.HOME - 0.05 && free >= 0 && entered === null && ctx.ui.claimed === null) {
      // And the key to press shows, for a hand that has no keyboard.
      hint ??= ctx.ui.draw.button(null, "Enter");
      ctx.ui.extras.push({ card: hint, x: ctx.ui.hud.width / 2, y: 22, anchor: [0.5, 0], alpha: clamp01((free - 0.5) / 0.15), press: "enter" });
    }
    if (entered !== null && time - entered > 6) entered = null; // (the page has long gone on; if it has not, the field is as it was)

    walker.position.set(origin.x - 34 + wrap(1.1 * time, 86), origin.y + 78, origin.z + 0.8 + 0.12 * Math.abs(Math.sin(time * 5)));
    legs.forEach((leg, i) => leg.rotation.set(26 * (i ? -1 : 1) * Math.sin(time * 5) * RAD, 0, 0));
  });

  const name: NameField = {
    box() {
      const { s } = tl.frame;
      if (s < 6.0 || s >= C.HOME - 0.05 || popped.name.t <= 0) return null;
      // The name's line: from where it begins to where the host does, projected.
      stage.camera.updateMatrixWorld();
      field.updateWorldMatrix(true, false);
      const a = field.localToWorld(corner.set(left - 2 * cell, 0, line - 3 * cell)).project(stage.camera).clone();
      const b = field.localToWorld(corner.set(left + (NAME_CELLS + 2) * cell, 0, line + 11 * cell)).project(stage.camera);
      return { left: a.x * 0.5 + 0.5, top: 0.5 - b.y * 0.5, width: (b.x - a.x) * 0.5, height: (b.y - a.y) * 0.5 };
    },
    focus(on) {
      focused = on;
    },
    type(next, time) {
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
    enter(time) {
      // (Only where it can be seen: at the field, by the lit frame.)
      if (verdict !== "free" || entered !== null || tl.frame.s < 6.0) return false;
      entered = time;
      // It leaps through the lit frame, and is gone.
      nether.act(time + 0.15, time + 0.8, (u, out) => {
        setPose(out, waits, lerp(12, 180, smooth(u)));
        out.pos.lerp(inside, smooth(u));
        out.pos.z += 4 * 1.1 * u * (1 - u);
        out.tilt[0] = 360 * smooth(u);
        out.grow = 1 - smooth((u - 0.8) / 0.2);
      });
      nether.act(time + 0.8, time + 6, (_u, out) => {
        setPose(out, inside, 180);
        out.grow = 0;
      });
      return true;
    },
  };
  return name;
}
