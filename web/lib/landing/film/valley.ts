/**
 * The sky and the valley: the clouds, the hero's title and the pumpkin's perch, and the
 * slab of ground that hides the cut face until the stomp.
 */

import { Box3, Vector3, type Object3D } from "three";
import { lerp, seeded, smooth } from "../anim/easing";
import { grow } from "../anim/timeline";
import { vec, type Ctx } from "./context";

export function setValley(ctx: Ctx) {
  const { tl, C, stage } = ctx;
  const eye = vec(C.HERO_EYE);

  // The clouds drift with the wind.
  const clouds: [Object3D, Vector3][] = [];
  for (const [name, cloud] of stage.nodes) if (name.startsWith("Cloud_") && name !== "Cloud_dive") clouds.push([cloud, cloud.position.clone()]);

  // The hero: gone under the white-out, so nothing of it hangs over the landing.
  const title = ctx.node("Hero_Title");
  const cue = ctx.node("Hero_Cue");
  const hero = [title, ctx.node("Hero_Perch"), ctx.node("Hero_Caption"), cue];
  // The layer the dive goes through. Once the pumpkin is down it is out of the designed
  // frame; a taller screen would see its flat underside, so it goes.
  const layer = ctx.node("Cloud_dive");
  const titleZ = title.position.z;
  const cueZ = cue.position.z;

  // The Zs rise from the sleeper one after another, until it wakes.
  const cells = [0.034, 0.046, 0.062];
  const zs = cells.map((_, i) => ctx.node(`Hero_Zzz_${i}`));
  const from = vec(ctx.pick([2.2, 18.4, 31.05], [1.9, 18.4, 31.4]));
  const to = vec(ctx.pick([4.6, 18.4, 32.2], [3.3, 18.4, 33.3]));
  const woken = tl.trigger(C.WAKE_UP - 0.01, 0.15);

  tl.during(0, 1.2, ({ s, time, dt }) => {
    for (const [cloud, home] of clouds) cloud.position.set(home.x + 0.22 * time, home.y + 0.04 * time, home.z);
    rain.visible = time >= rained && time - rained < 3.2;
    const asleep = s < C.WAKE_UP - 0.01;
    pumpkin.mask = asleep && (watched || time - poked < 1.4) ? "peek" : null;
    let toward = 0;
    if (asleep && watched && ctx.touch.pointer) {
      // It turns to where the pointer is, a little.
      onScreen.copy(sleeper).project(stage.camera);
      toward = Math.max(Math.min(0.45 * 480 * (ctx.touch.pointer.x - (onScreen.x * 0.5 + 0.5)), 40), -40);
    }
    glance += (toward - glance) * (1 - Math.exp(-dt / 0.1));
    pumpkin.glance = Math.abs(glance) > 0.01 ? glance : 0;
    const there = s < C.WHITE[1] + 0.01 ? 1 : 0;
    for (const thing of hero) grow(thing, there);
    layer.visible = s < C.FALL[1] + 0.04;
    title.position.z = titleZ + 0.28 * Math.sin((time * 2 * Math.PI) / 6.5); // it floats
    cue.position.z = cueZ + 0.012 * Math.sin((time * 2 * Math.PI) / 1.1); // and the cue nudges downward
    const left = Math.max(1 - woken.t / 0.15, 0);
    zs.forEach((z, i) => {
      const u = (time / 2.7 + i / 3) % 1;
      const size = ((0.5 * lerp(0.03, 0.066, u)) / cells[i]) * smooth(u / 0.12) * smooth((1 - u) / 0.18) * left * there;
      z.visible = size > 1e-4;
      if (!z.visible) return;
      z.scale.setScalar(size);
      // As the art's `toward` places it: half way to the eye, at half the size.
      z.position.lerpVectors(from, to, u);
      z.position.z += 0.1 * Math.sin(u * 9);
      z.position.multiplyScalar(0.5).addScaledVector(eye, 0.5);
    });
  });

  // Rain waits for somebody to click a cloud: then it falls from that one for a while.
  const rain = ctx.node("Rain");
  const rainFrom = new Vector3(-16, 70, 52); // the foot of the cloud the art hung it under
  let rained = -Infinity;
  for (const [cloud, home] of clouds) {
    const box = new Box3().setFromObject(cloud);
    const middle = box.getCenter(new Vector3());
    const at = new Vector3();
    const drifted = () => at.copy(middle).add(cloud.position).sub(home);
    ctx.touch.add({
      name: cloud.name,
      at: drifted,
      half: box.getSize(new Vector3()).multiplyScalar(0.5),
      seen: [0, C.WAKE_UP - 0.01],
      click: (time) => {
        rained = time + 0.1;
        drifted();
        rain.position.set(at.x - rainFrom.x, at.y - rainFrom.y, box.min.z - rainFrom.z);
      },
    });
  }

  // The sleeper: point at it and it opens an eye and follows the pointer; click it and it jumps.
  const { pumpkin } = ctx.cast;
  const sleeper = vec(C.HERO_PUMPKIN).add(new Vector3(0, 0, 0.5));
  const onScreen = new Vector3();
  let watched = false;
  let poked = -Infinity;
  let glance = 0;
  ctx.touch.add({
    name: "sleeper",
    at: sleeper,
    half: new Vector3(0.62, 0.62, 0.62),
    seen: [0, C.WAKE_UP - 0.02],
    enter: () => void (watched = true),
    leave: () => void (watched = false),
    click: (time) => {
      poked = time;
      pumpkin.jump(time, 0.42); // (a small one: the title is right above its head)
    },
  });

  // The slab in front of the cut face, in columns that fall away in a wave from the stomp.
  // The fall is scrubbed, so the camera, which drops through where the slab was, can
  // never catch up with it however fast the page is scrolled.
  const random = seeded(3);
  const columns: { slab: Object3D; home: Vector3; delay: number; tip: number; twist: number; drift: number }[] = [];
  for (const name of [...stage.nodes.keys()].filter((key) => key.startsWith("Slab_")).sort()) {
    const slab = ctx.node(name);
    const back = name.endsWith("_1");
    columns.push({
      slab,
      home: slab.position.clone(),
      delay: 0.04 + 0.03 * Math.abs(slab.position.x + 1) + (back ? 0 : 0.16),
      tip: lerp(0.7, 1.5, random()),
      twist: lerp(-0.25, 0.25, random()),
      drift: lerp(0.6, 1.6, random()),
    });
    slab.rotation.order = "ZYX";
  }
  tl.during(C.STOMP - 0.1, C.STOMP + 0.6, ({ s }) => {
    for (const { slab, home, delay, tip, twist, drift } of columns) {
      const fallen = Math.max((s - C.STOMP) * C.SLAB_RATE - delay, 0);
      slab.visible = fallen <= 1.55;
      if (!slab.visible) continue;
      slab.position.set(home.x, home.y - drift * fallen, home.z - 13 * fallen * fallen);
      slab.rotation.set(-tip * fallen, 0, twist * fallen);
    }
  });
}
