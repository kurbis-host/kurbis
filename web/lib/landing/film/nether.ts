/**
 * The Nether: out of the portal on its ledge, a short bridge, and a second frame that
 * waits unlit under the name field. The end of the page: a name is typed, it is free,
 * the frame lights, and the pumpkin leaps through.
 */

import { Mesh, Vector3, type BufferGeometry, type Material } from "three";
import { setPose } from "../anim/actor";
import { POP, UNPOP, clamp01, lerp, smooth, wrap } from "../anim/easing";
import { grow } from "../anim/timeline";
import type { Card } from "../engine/hud";
import { vec, type Ctx } from "./context";

const RAD = Math.PI / 180;
/** Cells the name has room for before it is set smaller, and the host's width, as the art laid the line out. */
const NAME_CELLS = 58;
const HOST_CELLS = 71;

/** What the page has found out about the name that stands in the field. */
export type Verdict = "free" | "taken" | "short" | "dash" | null;

export interface NameField {
  /** Where the field is on the canvas, as shares of its width and height from the top left; null while it cannot be typed in. */
  box(): { left: number; top: number; width: number; height: number } | null;
  focus(on: boolean): void;
  /** The whole name as it now stands. */
  type(text: string, time: number): void;
  judge(verdict: Verdict, time: number): void;
  /** Enter. True if the name was free: the pumpkin leaps, and the page may follow it. */
  enter(time: number): boolean;
}

export function setNether(ctx: Ctx): NameField {
  const { tl, C, stage, tall } = ctx;
  const { lamps } = stage;
  const origin = vec(stage.data.nether.origin);
  const sIn = C.THROUGH - 0.03;
  const lights = lamps.named("Nether_Lamp_");
  const face = stage.data.glyphs.block;

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

  // -- the name, as it is typed -------------------------------------------------------------
  const cell = ctx.pick(0.06, 0.09);
  const left = -(ctx.pick(NAME_CELLS + 1 + HOST_CELLS, NAME_CELLS) / 2) * cell; // where the name begins
  const line = ctx.pick(0.72, 2.55); // the name's line, above the foot of the block
  const said = ctx.pick(0.042, 0.06); // the cell of what is said under it
  const glyphs = new Map<string, BufferGeometry>();
  let look: Material | null = null;
  const letters: { mesh: Mesh; ch: string; born: number }[] = [];
  let text = "";
  let focused = false;
  let emptied = 0; // how far the placeholder has gone, 0..1
  let verdict: Verdict = null;
  let judged = -Infinity;
  let cleared = -Infinity;
  let message: Mesh | null = null;
  let messageFor = "";
  let entered: number | null = null;
  let hint: Card | null = null;

  const widthOf = (ch: string) => (face.glyphs[ch] ?? face.glyphs["?"])[0].length;
  const letter = (ch: string) => {
    const geometry = glyphs.get(ch);
    if (geometry && look) return new Mesh(geometry, look);
    const made = stage.makeType([ch], { palette: "cream", cell, depth: 4 });
    look = made.material as Material;
    glyphs.set(ch, made.geometry);
    return made;
  };
  const say = (lines: string[], palette: string) => {
    if (message) {
      field.remove(message);
      message.geometry.dispose();
    }
    message = stage.makeType(lines, { palette, cell: said, depth: 3 });
    field.add(message);
  };
  const SAYS: Record<Exclude<Verdict, null>, (name: string) => [string[], string]> = {
    free: (name) => [tall ? [`${name}.KURBIS.HOST`, "IS FREE."] : [`${name}.KURBIS.HOST IS FREE.`], "lime"],
    taken: (name) => [tall ? [`${name}.KURBIS.HOST`, "IS TAKEN."] : [`${name}.KURBIS.HOST IS TAKEN.`], "ember"],
    short: () => [tall ? ["AT LEAST", "3 CHARACTERS."] : ["AT LEAST 3 CHARACTERS."], "ember"],
    dash: () => [tall ? ["NO DASH AT THE", "START OR END."] : ["NO DASH AT THE START OR END."], "ember"],
  };

  // -- the pumpkin's last leap ----------------------------------------------------------------
  const { nether } = ctx.cast;
  const spots = ctx.points.cast.nether_spots.map(vec);
  const waits = spots[spots.length - 1];
  const inside = vec(ctx.points.cast.nether_inside);

  const corner = new Vector3();

  tl.during(sIn - 0.1, C.END, ({ s, time, dt }) => {
    const there = s >= sIn;
    for (const lamp of lights) lamp.energy = there ? lamp.base : 0;
    for (const thing of stale) if (thing) thing.visible = false;
    card.visible = s >= 5.6;
    grow(headline, POP(popped.headline.t / 0.28));
    grow(host, POP(popped.host.t / 0.28));
    // Until a name is typed the field shows a placeholder.
    emptied = focused || text ? Math.min(emptied + dt / 0.15, 1) : Math.max(emptied - dt / 0.15, 0);
    grow(placeholder, POP(popped.name.t / 0.28) * UNPOP(emptied));
    grow(bar, POP(popped.name.t / 0.28));
    bar.scale.z *= focused ? 1.5 + 0.5 * Math.sin(time * 5) : 1; // it stirs while the field has the keyboard

    // The letters, each popping in as it is typed. A long name is set smaller, to fit.
    let cells = 0;
    for (const { ch } of letters) cells += widthOf(ch) + 1;
    const fit = Math.min(NAME_CELLS / Math.max(cells - 1, 1), 1);
    let x = 0;
    for (const { mesh, ch, born } of letters) {
      const w = widthOf(ch);
      mesh.position.set(left + (x + w / 2) * cell * fit, 0, line);
      mesh.scale.setScalar(fit * (1 + 0.2 * Math.sin(Math.PI * Math.min((time - born) / 0.12, 1))));
      mesh.visible = popped.name.t > 0;
      x += w + 1;
    }

    // What the page says about the name takes the caption's place.
    const told = verdict !== null;
    const answer = told ? POP((time - judged - 0.05) / 0.28) : UNPOP((time - cleared) / 0.15);
    if (message) grow(message, answer);
    const captioned = told ? UNPOP((time - judged + 0.1) / 0.15) : POP((time - cleared - 0.1) / 0.28);
    grow(caption, POP(popped.caption.t / 0.28) * (message ? captioned : 1));

    // A name that is free lights the frame under it.
    const free = verdict === "free" ? time - judged : -1;
    grow(gate, free >= 0 ? POP((free - 0.25) / 0.3) : 0);
    sparks.visible = free >= 0.3;
    sparks.userData.grow = clamp01((free - 0.3) / 0.2);
    glows.forEach((lamp, i) => void (lamp.energy = free >= 0 ? [800, 260][i] * clamp01((free - 0.25) / 0.25) : 0));
    if (ctx.ui && s > 6.0 && free >= 0 && entered === null) {
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
      if (s < 6.0 || popped.name.t <= 0) return null;
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
      const upper = next.toUpperCase();
      if (upper === text) return;
      // Keep the letters that still stand; the new ones pop.
      let same = 0;
      while (same < text.length && same < upper.length && text[same] === upper[same]) same += 1;
      for (const gone of letters.splice(same)) field.remove(gone.mesh);
      [...upper.slice(same)].forEach((ch, i) => {
        const mesh = letter(ch);
        field.add(mesh);
        letters.push({ mesh, ch, born: time + 0.02 * i });
      });
      text = upper;
    },
    judge(next, time) {
      if (next === verdict && messageFor === text) return;
      if (next === null) cleared = time;
      else {
        const [lines, palette] = SAYS[next](text);
        say(lines, palette);
        judged = time;
        messageFor = text;
      }
      verdict = next;
    },
    enter(time) {
      if (verdict !== "free" || entered !== null) return false;
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
