/**
 * The cut face: the facts beside the dig, the blocks the pumpkin has yet to dig, their
 * cracks, and the crumbs it throws.
 */

import { Vector3 } from "three";
import { lerp, seeded, smooth } from "../anim/easing";
import { grow } from "../anim/timeline";
import type { Card } from "../engine/hud";
import { vec, type Ctx } from "./context";

/** Things buried in the cut face, to find on the way down: what each says when the pointer finds it. */
const TOYS: [string, string, string][] = [
  ["fossil", "Fossil", "Older than your last world."],
  ["chest", "Chest", "Someone's coal. Finders keepers."],
  ["sign", "Old sign", "It says: Back in 5 minutes."],
  ["sock", "Lost sock", "The other one is in the Nether."],
];

export function setDig(ctx: Ctx) {
  const { tl, C, points, dig, tall } = ctx;
  const { steps, through } = points;

  // The tall frame turns away from each band before it is out of sight: it steps aside
  // before the frame's edge would cut it.
  const bands = [0, 1].map((i) => ctx.node(`Band_${i}`));
  const gone = [2.04, 2.52];

  // What the pumpkin has not dug yet stays in place until it gets there. Its face cracks
  // as the pumpkin comes: ten stages by the scroll, then it is gone.
  const plugs: { plug: ReturnType<Ctx["find"]>; cracks: ReturnType<Ctx["find"]>; i: number; begun: number; when: number }[] = [];
  // A crumb thrown toward the camera at every step, so the digging has some bite.
  const crumbs = Array.from({ length: 12 }, (_, i) => ctx.node(`Crumb_${i}`));
  for (const crumb of crumbs) crumb.rotation.order = "ZYX";
  const random = seeded(11);
  const throws: { crumb: number; clock: ReturnType<typeof tl.trigger>; from: Vector3; fling: Vector3; spin: Vector3 }[] = [];
  for (let i = 0; i <= through; i += 1) {
    const when = i ? dig.when(i) : C.DIG0;
    const tag = String(i).padStart(2, "0");
    const plug = ctx.find(`Plug_${tag}`);
    if (plug) plugs.push({ plug, cracks: ctx.find(`Crack_${tag}`), i, begun: i > 1 ? dig.when(i - 1) : C.DIG0, when });
    throws.push({
      crumb: i % crumbs.length,
      clock: tl.trigger(when, 0.42),
      from: new Vector3(steps[i][0], 0.8, steps[i][1] + 0.5),
      fling: new Vector3(lerp(-1.6, 1.6, random()), lerp(-3.4, -1.4, random()), lerp(1.0, 3.2, random())),
      spin: new Vector3(lerp(-9, 9, random()), lerp(-9, 9, random()), lerp(-9, 9, random())),
    });
  }

  for (const [key, title, line] of TOYS) {
    if (!points.toys[key]) continue;
    let card: Card | null = null;
    ctx.touch.add({
      name: key,
      at: vec(points.toys[key]),
      half: new Vector3(0.75, 0.6, 0.7),
      seen: [1.72, C.BREAK + 0.04],
      pick: true,
      tip: () => (card ??= ctx.ui?.draw.tooltip(title, [line]) ?? null),
    });
  }

  tl.during(C.STOMP - 0.1, C.LAND + 0.3, ({ s }) => {
    if (tall) bands.forEach((band, i) => grow(band, 1 - smooth((s - gone[i]) / 0.05)));
    const far = dig.value(s);
    for (const { plug, cracks, i, begun, when } of plugs) {
      if (plug) plug.visible = s < when - 0.004;
      if (!cracks) continue;
      cracks.visible = s >= begun && s < when - 0.004;
      cracks.userData.tint = Math.min(Math.max(far - (i - 1), 0), 0.999);
    }
    // Each crumb flies for the step that threw it last.
    for (const crumb of crumbs) crumb.visible = false;
    for (const { crumb, clock, from, fling, spin } of throws) {
      const t = clock.t;
      if (t <= 0) continue;
      const thing = crumbs[crumb];
      const size = Math.max(1 - t / 0.42, 0);
      thing.visible = size > 1e-3;
      if (!thing.visible) continue;
      thing.scale.setScalar(size);
      thing.position.set(from.x + fling.x * t, from.y + fling.y * t, from.z + fling.z * t - 9 * t * t);
      thing.rotation.set(spin.x * t, spin.y * t, spin.z * t);
    }
  });
}
