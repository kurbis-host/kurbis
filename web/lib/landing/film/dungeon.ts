/**
 * The dungeon. In the south bay, the ore wall with its labels and the backups chest. In
 * the north bay, the spawner under the three things you can ask it, and the portal that
 * waits unlit in the wall until the pumpkin jumps into it.
 *
 * Left alone, both bays show themselves: the ores say what they are for one after
 * another, and the three things on the signs are asked in turn, and the room does them.
 * What a visitor points at or clicks has its turn at once.
 */

import { Vector3, type Object3D } from "three";
import { POP, UNPOP, clamp01, flicker, lerp, linear, smooth } from "../anim/easing";
import { setPose } from "../anim/actor";
import { fade, grow } from "../anim/timeline";
import { GREEN, WHITE, type Card } from "../engine/hud";
import { vec, type Ctx } from "./context";

const RAD = Math.PI / 180;
/** Seconds each ore has the word, and each thing asked of the spawner takes. */
const ORE_TURN = 2.3;
const ASK_TURN = 3.4;

/** Whose turn it is and since when; taking turns of `each` seconds while `on`. */
export class Round {
  which = 0;
  since = 0;
  running = false;
  /** Held on the one the visitor points at. */
  held = false;

  constructor(
    private readonly count: number,
    private readonly each: number,
  ) {}

  /** Returns true on the frame a new turn begins. */
  step(on: boolean, time: number): boolean {
    if (!on) {
      this.running = false;
      return false;
    }
    if (!this.running) {
      this.running = true;
      this.which = 0;
      this.since = time;
      return true;
    }
    if (this.held) this.since = Math.max(this.since, time - this.each + 0.5);
    if (time - this.since < this.each) return false;
    this.which = (this.which + 1) % this.count;
    this.since = time;
    return true;
  }

  /** Give the word to `which` now; the round goes on from there. */
  take(which: number, time: number) {
    this.running = true;
    this.which = which;
    this.since = time;
  }

  /** How much `which` shows: it fades in for its turn and out before the next. */
  showing(which: number, time: number): number {
    if (!this.running || this.which !== which) return 0;
    const age = time - this.since;
    return clamp01(age / 0.15) * (1 - clamp01((age - this.each + 0.25) / 0.2));
  }
}

export function setDungeon(ctx: Ctx) {
  const { tl, C, stage, points } = ctx;
  const { lamps } = stage;
  const d = stage.data.spots.dungeon;
  const wall = d.x[1] + 1; // the east wall's face
  const proud = d.x[1]; // the face of the ores, which stand a block out from it
  const signY = d.y[1] - 0.5;
  // Its lights are on while the dungeon can be seen. A flame wavers; an ore's glow breathes slowly.
  const lit = linear([[3.85, 0], [3.95, 1], [C.THROUGH, 1], [C.THROUGH + 0.05, 0]]);
  const lights: { name: string; energy: number; waver: (time: number) => number }[] = [];
  const steady = (_time: number) => 1;
  for (const lamp of [...lamps.named("Dungeon_"), ...lamps.named("Spawner_Glow"), ...lamps.named("Sign_Light")]) {
    const { name } = lamp;
    if (name.startsWith("Dungeon_PortalGlow") || name === "Dungeon_NightVision") continue;
    const i = Number(name.replace(/^.*?(\d+)$/, "$1")) || 0;
    let waver: (time: number) => number = steady;
    if (name.startsWith("Dungeon_OreLight_")) waver = (time) => 1 + 0.16 * Math.sin(time * 1.3 + 1.9 * i);
    else if (name.startsWith("Dungeon_TorchLight_n")) waver = (time) => flicker(time, 3.1 * i + 0.7);
    else if (name.startsWith("Dungeon_TorchLight_")) waver = (time) => flicker(time, 2.3 * i);
    else if (name === "Spawner_Glow") waver = (time) => flicker(time, 4.4);
    lights.push({ name, energy: lamp.base, waver });
  }

  // Inside the cage the little lantern turns and bobs, as whatever a spawner holds does.
  const core = ctx.node("Spawner_Core");
  // The portal in the east wall: dark until the pumpkin jumps into the frame.
  const surface = ctx.node("Dungeon_Portal");
  const sparks = ctx.node("Dungeon_Sparks");
  const lighted = tl.trigger(C.PORTAL_LIT, 0.2);
  const glows = lamps.named("Dungeon_PortalGlow");

  // -- the ore wall: each ore says what it is for; the chest opens for backups -----------
  const wallOf = stage.layout.ore_wall;
  const [chestY, chestUp, shelf] = wallOf.chest;
  const hang = ctx.pick(-0.58, 1.35); // where a tile's lines hang, from its foot
  const backups = ["Creeper took the house?", "Ctrl+Z for the whole world."];
  const topics = [
    ...wallOf.tiles.map(([, up], k) => ({ title: null as string | null, lines: C.ORE_LINES[k], at: vec(points.tile_spots[k]), half: new Vector3(0.5, 1.1, 1.1), from: new Vector3(proud, points.tile_spots[k][1], d.floor + up + hang), under: true })),
    shelf
      ? { title: null, lines: backups, at: vec(points.tile_spots[wallOf.tiles.length]), half: new Vector3(0.5, 1.1, 1.1), from: new Vector3(proud, chestY, d.floor + chestUp - 1 + hang), under: true }
      : { title: "Backups", lines: backups, at: vec(points.tile_spots[wallOf.tiles.length]), half: new Vector3(0.6, 0.6, 0.6), from: new Vector3(wall - 0.55, chestY - 2.0, d.floor + 0.8), under: false },
  ];
  const frames = topics.map((_, k) => ctx.node(`Dungeon_Pick_${k}`));
  const lids = [0, 1].map((k) => ctx.find(`Dungeon_Chest_${k}_lid`)).filter((lid): lid is Object3D => lid !== null);
  const ores = new Round(topics.length, ORE_TURN);
  const cards: (Card | null)[] = topics.map(() => null);
  topics.forEach((topic, k) => {
    ctx.touch.add({
      name: `topic_${k}`,
      at: topic.at,
      half: topic.half,
      seen: C.ORE_VIEW,
      enter: (time) => {
        if (ores.which !== k || !ores.running) ores.take(k, time);
        ores.held = true;
      },
      leave: () => void (ores.held = false),
    });
  });

  // -- the spawner: the three things on the signs are asked in turn -----------------------
  const asks = new Round(3, ASK_TURN);
  const signFrames = [0, 1, 2].map((k) => ctx.node(`Sign_Pick_${k}`));
  const home = ctx.node("Home_Sign");
  const homeSpot = vec(points.home_spot);
  const signSpot = vec(points.cast.sign_spot);
  const { pumpkin } = ctx.cast;
  const view = C.SPAWNER_VIEW;
  // What the room has done so far: night vision, the /home sign, and where the pumpkin is.
  let vision = -Infinity;
  let written: number | null = null;
  let erased: number | null = null;
  let stay: { t0: number; t1: number } | null = null;
  const ask = (k: number, time: number) => {
    const said = { stay: 5, from: view[0], until: view[1] + 0.02 };
    ctx.ui?.say(`> ${C.ASKED[k][0]}`, WHITE, time + 0.15, said);
    ctx.ui?.say(`[AI] ${C.ASKED[k][1]}`, GREEN, time + 1.0, said);
    if (k === 0) {
      vision = time + 1.0; // night vision lights the room
    } else if (k === 1) {
      if (written !== null && erased === null) return; // it is there already
      written = time + 1.0; // /home is written, and the pumpkin uses it: gone in a blink, and there
      erased = null;
      ctx.ui?.say("<Kurbis> /home", WHITE, time + 1.9, said);
      const go = time + 2.1;
      pumpkin.act(go, go + 0.36, (u, out) => {
        setPose(out, u < 0.5 ? signSpot : homeSpot, -100);
        out.grow = Math.abs(1 - 2 * u);
      });
      stay = pumpkin.act(go + 0.36, Infinity, (_u, out) => setPose(out, homeSpot, -100, 1, true));
    } else if (written !== null && erased === null) {
      erased = time + 1.0; // revert takes it back, and the pumpkin with it
      if (stay) {
        const back = time + 1.3;
        stay.t1 = back;
        stay = null;
        pumpkin.act(back, back + 0.55, (u, out) => {
          setPose(out, homeSpot, -100, 1 + 0.12 * Math.abs(Math.cos(Math.PI * u)));
          out.pos.lerp(signSpot, smooth(u));
          out.pos.z += 4 * 0.9 * u * (1 - u);
        });
      }
    }
  };
  points.sign_spots.forEach((spot, k) => {
    ctx.touch.add({
      name: `sign_${k}`,
      at: vec(spot),
      half: new Vector3(0.4, 1.56, 0.56),
      seen: view,
      click: (time) => {
        asks.take(k, time);
        ask(k, time);
      },
    });
  });
  const nightVision = "Dungeon_NightVision";

  tl.during(3.75, C.THROUGH + 0.15, ({ s, time }) => {
    const on = lit(s);
    for (const { name, energy, waver } of lights) lamps.set(name, on > 0 ? energy * on * waver(time) : 0);
    core.position.set(wall - 2.5, signY, d.floor + 1.46 + 0.035 * Math.sin(time * 2.1));
    core.rotation.set(0, 0, -62 * RAD + time * 1.7);
    grow(surface, POP(lighted.t / 0.2));
    sparks.visible = s >= C.PORTAL_LIT && s < C.THROUGH + 0.1;
    const glow = s < C.THROUGH + 0.02 ? clamp01(lighted.t / 0.1) : 0;
    for (const lamp of glows) lamp.energy = lamp.base * glow;

    // The ore wall, while the camera rests on it.
    ores.step(s >= C.ORE_VIEW[0] && s < C.ORE_VIEW[1], time);
    topics.forEach((topic, k) => {
      const showing = ores.showing(k, time);
      fade(frames[k], 0.9 * showing);
      if (showing <= 0 || !ctx.ui) return;
      const card = (cards[k] ??= ctx.ui.draw.tooltip(topic.title, topic.lines));
      const at = spot.copy(topic.from).project(stage.camera);
      const x = (at.x * 0.5 + 0.5) * ctx.ui.hud.width;
      const y = (at.y * 0.5 + 0.5) * ctx.ui.hud.height;
      const right = x > ctx.ui.hud.width * 0.62;
      // What a tile says hangs under its label; the chest on the floor has a tooltip beside it.
      if (topic.under) ctx.ui.tips.push({ card, at: [x, y], anchor: [0.5, 1], alpha: showing });
      else ctx.ui.tips.push({ card, at: [x + (right ? 20 : -20), y], anchor: [right ? 1 : 0, 0], alpha: showing });
    });
    const open = ores.showing(topics.length - 1, time); // the chest opens for its turn
    for (const lid of lids) lid.rotation.set(lerp(-8, -64, open) * RAD, 0, 0);

    // The spawner, while the camera rests on its bay.
    const asking = s >= view[0] && s < view[1] - 0.04;
    if (asks.step(asking, time)) ask(asks.which, time);
    signFrames.forEach((frame, k) => fade(frame, asking ? 0.95 * asks.showing(k, time) : 0));
    const lit0 = time - vision;
    lamps.set(nightVision, s >= 4.3 && s < C.THROUGH ? 1250 * clamp01(lit0 / 0.4) * (1 - clamp01((lit0 - 1.9) / 0.5)) : 0);
    if (s < view[0] - 0.1 || s >= view[1] + 0.05) {
      // Out of its bay, the room is as it was found.
      written = null;
      erased = null;
      stay = null;
      vision = -Infinity;
      pumpkin.settle();
    }
    const there = written === null ? 0 : erased === null ? POP((time - written) / 0.3) : UNPOP((time - erased) / 0.25);
    grow(home, there);
  });
}

const spot = new Vector3();
