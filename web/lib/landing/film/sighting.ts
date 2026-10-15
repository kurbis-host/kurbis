/**
 * Something at the treeline. A visitor who stays with the hero a long while, the pumpkin
 * asleep on its cloud, may find far below, at the edge of a wood, someone who looks just
 * like Steve: standing still, looking up at them, his eyes white and shining. Nothing on
 * the page says who he is (Herobrine, whom the patch notes have removed for years).
 *
 * Point at him, and he is not there: no fade, nothing, as in the stories. Leave him, and
 * he steps back behind the trees. Either way he does not come again that visit. He has
 * no say in anything else: the scroll, the camera and the story are as they were.
 */

import { Vector3 } from "three";
import { clamp01, smooth } from "../anim/easing";
import { Player, addPose, walk } from "../anim/player";
import type { Ctx } from "./context";

/** Seconds with the hero up, without a break, before he is there; how long he stands, left alone; his steps back. */
const WAIT = 15;
const STAND = 8;
const BACK = 2.2;
/** How far one stride takes him (both feet), in blocks. */
const STRIDE = 1.3;

export function setSighting(ctx: Ctx) {
  const { tl, C, stage } = ctx;
  // The art stood him where the frame sees him, facing the lens; the marker is where the trees hide him.
  const him = new Player(stage, "Sky_Herobrine", false, 7);
  him.blank = true;
  him.life = 0; // (not a breath)
  him.look = stage.camera.position;
  const { root } = him;
  const from = root.position.clone();
  const yaw = root.rotation.z;
  const to = ctx.node("Sky_Herobrine_Back").position.clone();
  const far = from.distanceTo(to);
  root.visible = false;

  let stayed = 0;
  let came: number | null = null;
  let gone = false;
  const go = () => {
    gone = true;
    root.visible = false;
  };
  const at = new Vector3();
  const chest = new Vector3();
  // Tiny on screen, so the box is generous. No tooltip: nothing says who he was.
  ctx.touch.add({
    name: "treeline",
    at: () => chest.set(at.x, at.y, at.z + 1),
    half: new Vector3(1.2, 1.2, 1.6),
    seen: [0, C.WAKE_UP - 0.02],
    here: () => came !== null && !gone,
    enter: go,
  });

  tl.during(0, C.WAKE_UP, ({ s, time, dt }) => {
    if (gone) return;
    const up = s < C.WAKE_UP - 0.02; // the pumpkin asleep, the hero up
    if (came === null) {
      stayed = up ? stayed + dt : 0;
      if (stayed < WAIT) return;
      came = time;
      him.place(from, yaw);
      him.rest();
      him.settle();
      root.visible = true; // simply there
    }
    // He does not wait for the visitor to come back.
    if (!up) return go();
    // Standing, then a few steps back, still facing the lens.
    const k = clamp01((time - came - STAND) / BACK);
    const u = smooth(k);
    at.lerpVectors(from, to, u);
    him.place(at, yaw);
    addPose(him.rest(), walk(-(u * far) / STRIDE, 0.6 * Math.sin(Math.PI * k)));
    him.update(time, dt);
    if (k >= 1) go(); // (behind the trees)
  });
}
