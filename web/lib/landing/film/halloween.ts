/**
 * Halloween (./dates.ts), as the game keeps it. Its zombies and skeletons come out with
 * pumpkins on their heads; here every friend wears one, wherever they are (and so does
 * the visitor, in a friend's skin): carved, and lit where it is dark, so that its face
 * shines. And the hero's title has the game's splash for the day at its lower right
 * corner, as the title screen has it at its logo's.
 */

import { Vector3 } from "three";
import { POP, UNPOP } from "../anim/easing";
import { playersOf } from "../anim/player";
import { grow } from "../anim/timeline";
import { lineMask } from "../engine/type";
import { COPY } from "../copy";
import type { Ctx } from "./context";
import { nightAt } from "./homestead";
import type { SkyField } from "./sky-field";
import { makeSplash, throb } from "./splash";
import { RIGS } from "./visitor";

/** How far from the lens the title's splash is drawn (blocks): nearer than the sleeper and its Zs. */
const NEAR = 4;

export function setHalloween(ctx: Ctx, sky: SkyField) {
  const { tl, C, stage } = ctx;

  // -- the friends' pumpkins: lit in the cave, and in the homestead by night -------------------------
  const players = playersOf(stage);
  const friends = Object.values(RIGS)
    .flat()
    .map((rig) => ({ player: players.get(rig), dark: (s: number) => rig.startsWith("Cave_") || (rig.startsWith("Home_") && nightAt(C, s)) }));
  tl.always(({ s }) => {
    for (const { player, dark } of friends) if (player) player.pumpkin = dark(s) ? "lit" : "carved";
  });

  // -- the title's splash -----------------------------------------------------------------------------
  // From under the title's last letter out past its lower right corner, which it only nips, so that every
  // letter of the title reads; above the sleeper's Zs and inside the frame wherever the hero rests. (Past the
  // last letter there is too little room left for it to start there; in the tall frame, whose last line is
  // short, it starts under that line.) It floats with the title, and is drawn nearer the lens than anything
  // in the hero, at the size that keeps it there in the picture, so that it stands over all of it, as the
  // game's splash does over its title screen.
  const title = ctx.node("Hero_Title");
  const note = stage.layout.nodes.find((node) => node.name === "Hero_Title")?.note;
  const last = note?.lines?.[note.lines.length - 1] ?? "";
  const right = (lineMask(stage.data.glyphs.block, last, note?.layout?.tracking)[0].length / 2) * (note?.cell ?? 0.5);
  const corner = new Vector3(right + ctx.pick(1.25, -0.3), 0, ctx.pick(0.2, -0.3)); // (its middle, in the title's terms: from the foot of the last letter's right side)
  const splash = makeSplash(stage, ctx.pick(0.1, 0.155), { words: COPY.spooky, day: 1 });
  splash.name = "Hero_Splash";
  stage.scene.add(splash);
  stage.nodes.set(splash.name, splash);
  const at = new Vector3();

  // It pops in with the title, and stays while the hero is up. One splash at a time: while a free
  // name has its own under the field, this one steps aside, and it is back once the name changes
  // (or once the claim card has come up, and the name's has gone: ./sky-field.ts).
  let up: number | null = null; // (when the hero came up)
  let free = false;
  let turned = -Infinity; // (when a free name's splash came, or went)
  tl.during(0, 1.2, ({ s, time }) => {
    if (s >= C.WHITE[1] + 0.01) {
      up = null;
      splash.visible = false;
      return;
    }
    up ??= time;
    if (sky.splashing !== free) {
      free = sky.splashing;
      turned = time;
    }
    const shown = free ? UNPOP((time - turned) / 0.15) : POP((time - Math.max(up + 0.3, turned + 0.15)) / 0.28);
    title.updateWorldMatrix(true, false);
    const eye = stage.camera.position;
    const near = NEAR / eye.distanceTo(title.localToWorld(at.copy(corner)));
    splash.position.lerpVectors(eye, at, near);
    grow(splash, shown * throb(time) * near);
  });
}
