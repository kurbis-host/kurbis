/**
 * The homestead: a cavern where daylight falls through a shaft onto a house in a fenced
 * yard. Three features play out on it, one after another, as the camera rests on each:
 *
 *   Friends in, everyone else out   the friends come in from the tunnel; the gate shuts
 *                                   on a stranger, who shrugs and goes. He is Herobrine:
 *                                   Steve, but for his blank, shining eyes. Click him and
 *                                   he is let in; and every light goes out, and he goes.
 *   Plugins                         the market's book on the lectern: each plugin changes
 *                                   the world (lanterns, fireflies, fireworks). Dusk falls.
 *   Turn back time                  night. A creeper blows the house apart, and scrolling
 *                                   on takes it all back, block by block, to the dawn.
 *
 * The first two run on real time while their stop is in sight; the blast and its undoing
 * are scrubbed by the scroll, so they play backwards as well as forwards. The friends are
 * the same two throughout, and walk from wherever one feature left them to wherever the
 * next wants them; Herobrine is only ever at the gate. Their portal waits in the east
 * wall, as before.
 */

import { AdditiveBlending, BoxGeometry, Color, Mesh, MeshBasicMaterial, PlaneGeometry, ShaderMaterial, Vector3, type IUniform, type Object3D } from "three";
import { POP, UNPOP, clamp01, easeOut, flicker, lerp, linear, seeded, smooth } from "../anim/easing";
import { Player, addPose, walk, type Delta } from "../anim/player";
import { fade, grow } from "../anim/timeline";
import type { Constants } from "../engine/bundle";
import { GREEN, GREY, WHITE, YELLOW, type Card, type Rgb } from "../engine/hud";
import { COPY } from "../copy";
import { vec, type Ctx, type Friend } from "./context";
import { thisYear } from "./dates";
import type { After } from "./speech";

const RAD = Math.PI / 180;
const RED: Rgb = [1, 0.4, 0.35];
/** Blocks a second, walking; how far one stride (both feet) takes them. */
const SPEED = 2.1;
const STRIDE = 1.45;
/** How long the stranger takes for one try at the gate, from the tunnel and back. */
const TRY = 7.6;
/** When each plugin installs itself, if nobody has clicked it first (seconds into the stop). */
const AUTO = [1.4, 3.6, 5.8];
/**
 * Herobrine let in, in seconds from the click: he walks in, and then every light flickers
 * out (over FLICKER); in the dark only his eyes shine (DARK), and as he leaves the game
 * the lights flicker on again.
 */
const OUT = 2.0;
const FLICKER = 0.3;
const DARK = 1.5;
const LEAVES = OUT + FLICKER + DARK;
/** How much of the light there is through each flicker, a twentieth of a second at a time: going out, and coming back. */
const GOING = [0.25, 0.9, 0.1, 0.6, 0.15, 0];
const COMING = [0, 0.55, 0.1, 0.85, 0.4, 1];
/** How much light there is in the homestead, `u` seconds after Herobrine was let in. */
const lightsAt = (u: number) => {
  const step = (steps: readonly number[], k: number) => steps[Math.min(Math.floor(k * steps.length), steps.length - 1)];
  if (u < OUT) return 1;
  if (u < OUT + FLICKER) return step(GOING, (u - OUT) / FLICKER);
  if (u < LEAVES) return 0;
  if (u < LEAVES + FLICKER) return step(COMING, (u - LEAVES) / FLICKER);
  return 1;
};
/** The picture on the toast for turning him away (a key of the art's HUD): the game's shield. */
const SHIELD = "shield";

/** Someone who walks to where they are wanted, and faces where they are told. */
class Walker {
  readonly at = new Vector3();
  yaw = 0;
  goal = new Vector3();
  goalYaw: number | null = null;
  pace = 0;
  phase = 0;
  shown = 0;
  /** Seen at all (out of the tunnel). */
  out = false;

  constructor(
    readonly player: Player,
    at: Vector3,
    yaw: number,
  ) {
    this.put(at, yaw);
  }

  /** Be there now, facing so (radians), as when the scene starts over. */
  put(at: Vector3, yaw: number) {
    this.at.copy(at);
    this.goal.copy(at);
    this.yaw = yaw;
    this.goalYaw = yaw;
    this.pace = 0;
  }

  go(to: Vector3, yaw: number | null = null) {
    this.goal.copy(to);
    this.goalYaw = yaw;
  }

  get arrived(): boolean {
    return Math.hypot(this.goal.x - this.at.x, this.goal.y - this.at.y) < 0.06;
  }

  /** A frame: step toward the goal, turning to the way they walk, and pose the body. `extra` is laid on top. */
  update(time: number, dt: number, extra: Delta | null = null) {
    const dx = this.goal.x - this.at.x;
    const dy = this.goal.y - this.at.y;
    const far = Math.hypot(dx, dy);
    const moving = far > 0.06;
    this.pace += ((moving ? 1 : 0) - this.pace) * Math.min(dt * 7, 1);
    let face = this.goalYaw ?? this.yaw;
    if (moving) {
      const step = Math.min(far, SPEED * dt * Math.max(this.pace, 0.25));
      this.at.x += (dx / far) * step;
      this.at.y += (dy / far) * step;
      this.phase += step / STRIDE;
      face = Math.atan2(dx, -dy);
    }
    let turn = face - this.yaw;
    turn = ((turn + Math.PI) % (2 * Math.PI) + 2 * Math.PI) % (2 * Math.PI) - Math.PI;
    this.yaw += turn * Math.min(dt * (moving ? 9 : 5), 1);
    this.player.place(this.at, this.yaw);
    addPose(this.player.rest(), walk(this.phase % 1, this.pace));
    if (extra) addPose(this.player.target, extra);
    this.player.update(time, dt);
    this.shown += ((this.out ? 1 : 0) - this.shown) * Math.min(dt * 10, 1);
    grow(this.player.root, this.shown);
  }
}

/** Where a thing is in the world now (a copy). */
const worldOf = (thing: Object3D, out = new Vector3()) => {
  thing.updateWorldMatrix(true, false);
  return out.setFromMatrixPosition(thing.matrixWorld);
};

/** Whether it is night in the homestead at a scroll position: from the creeper's feature until the world is whole again. */
export function nightAt(C: Constants, s: number): boolean {
  return s >= C.BEATS[2][0] - 0.01 && s < C.REWIND[3];
}

export function setHomestead(ctx: Ctx) {
  const { tl, C, stage, tall, points } = ctx;
  const { lamps } = stage;
  const [gateBeat, bookBeat, rewindBeat] = C.BEATS as [number, number][];
  const [AT_WALL, BLAST, BACK, WHOLE] = C.REWIND;
  const yard = C.YARD;
  const house = C.HOUSE;
  const spots = points.home_spots;
  const floor = stage.data.spots.dungeon.floor;
  const say = COPY.homestead;
  const random = seeded(23);

  // -- light: the cavern's lamps, and the daylight down the shaft through the day -------------
  const lit = linear([[3.85, 0], [3.95, 1], [C.THROUGH, 1], [C.THROUGH + 0.05, 0]]);
  const steady = ["Home_Bounce", "Home_Fill"].map((name) => ({ name, base: lamps.base(name) }));
  const torch = { name: "Dungeon_TorchLight_0", base: lamps.base("Dungeon_TorchLight_0") };
  const sky = lamps.byName.get("Home_Sky") ?? null;
  const skyBase = sky?.base ?? 0;
  // The day, as [scroll, colour, strength]: noon, dusk over the plugins, night for the
  // creeper; and as time turns back, the gold of the morning before it all.
  const DAY: [number, readonly number[], number][] = [
    [0, [1.0, 0.93, 0.8], 1.0],
    [bookBeat[0] - 0.03, [1.0, 0.93, 0.8], 1.0],
    [bookBeat[0] + 0.07, [1.0, 0.56, 0.34], 0.62],
    [rewindBeat[0] - 0.04, [1.0, 0.5, 0.32], 0.5],
    [rewindBeat[0] + 0.01, [0.3, 0.4, 0.92], 0.16],
    [BACK, [0.3, 0.4, 0.92], 0.16],
    [WHOLE, [1.0, 0.76, 0.5], 0.86],
  ];
  const tint = new Color();
  const dayAt = (s: number, out: Color): number => {
    let i = 0;
    while (i < DAY.length - 2 && DAY[i + 1][0] <= s) i += 1;
    const [s0, c0, l0] = DAY[i];
    const [s1, c1, l1] = DAY[i + 1];
    const k = smooth(clamp01((s - s0) / Math.max(s1 - s0, 1e-6)));
    out.setRGB(lerp(c0[0], c1[0], k), lerp(c0[1], c1[1], k), lerp(c0[2], c1[2], k));
    return lerp(l0, l1, k);
  };
  // The daylight itself: the opening at the top of the shaft, and a soft beam down it.
  const [[sx0, sx1], [sy0, sy1]] = stage.data.spots.dungeon.shaft;
  const shaftMid = new Vector3((sx0 + sx1 + 1) / 2, (sy0 + sy1 + 1) / 2, floor);
  const height = stage.data.spots.dungeon.height;
  const opening = new Mesh(new PlaneGeometry(sx1 + 1 - sx0, sy1 + 1 - sy0), new MeshBasicMaterial({ color: 0xffffff }));
  opening.position.set(shaftMid.x, shaftMid.y, floor + height + 7.8);
  opening.rotation.x = Math.PI; // facing down
  const beamLook = new MeshBasicMaterial({ color: 0xffffff, transparent: true, opacity: 0.1, blending: AdditiveBlending, depthWrite: false });
  const beams = [0, 1, 2].map((k) => {
    const beam = new Mesh(new BoxGeometry(sx1 + 1 - sx0 - k * 0.9, sy1 + 1 - sy0 - k * 0.9, height + 7.5), beamLook);
    beam.position.set(shaftMid.x + 0.25 * k, shaftMid.y - 0.15 * k, floor + (height + 7.5) / 2);
    beam.renderOrder = 7;
    return beam;
  });
  for (const thing of [opening, ...beams]) stage.scene.add(thing);

  // -- the headlines, one for each feature, while its stop is in sight ------------------------------
  const heads = [0, 1, 2].map((k) => ctx.find(`Home_Head_${k}`));
  const headsIn = (C.BEATS as [number, number][]).map(([s0]) => tl.trigger(s0 + 0.02, 0.28));
  // After the sunbeam (its light is drawn over what is behind it, not over the headline in front).
  for (const head of heads) head?.traverse((part) => void (part.renderOrder = 8));

  // -- the portal in the east wall: dark until the pumpkin jumps into the frame ---------------
  const surface = ctx.node("Dungeon_Portal");
  const sparks = ctx.node("Dungeon_Sparks");
  const lighted = tl.trigger(C.PORTAL_LIT, 0.2);
  const glows = lamps.named("Dungeon_PortalGlow");

  // -- the people ---------------------------------------------------------------------------------------
  const tunnel = vec(spots.tunnel);
  const gateAt = vec(spots.gate);
  const outside = new Vector3(gateAt.x, gateAt.y - 1.4, floor); // where one waits at the gate
  const steve = new Walker(new Player(stage, "Home_Steve", false, 4), tunnel, 0);
  const alex = new Walker(new Player(stage, "Home_Alex", true, 5), tunnel, 0);
  const stranger = new Walker(new Player(stage, "Home_Stranger", false, 6), tunnel, 0);
  // The stranger is Herobrine: Steve's skin (the art gave him that), and eyes blank and
  // shining, as at the treeline (./sighting.ts). Steve himself stands beside him.
  stranger.player.blank = true;
  const people = [steve, alex, stranger];
  // Where they stand for each feature: in the yard; at the lectern and by the fence; along the north fence, watching the house.
  const P = (x: number, y: number, z = floor) => new Vector3(x, y, z);
  const lectern = vec(spots.lectern);
  const yawTo = (from: Vector3, to: Vector3) => Math.atan2(to.x - from.x, -(to.y - from.y));
  const place = {
    // Either side of the path from the gate, so the camera sees past them to it, and them in profile.
    yard: { steve: P(24.0, 15.3), alex: P(20.9, 15.0) },
    // Steve behind the lectern, reading toward the camera (out of the sunbeam, which would
    // burn his shirt white); Alex to his left by the fence, watching what the plugins do.
    book: { steve: P(lectern.x, lectern.y + 0.85), alex: P(22.95, 21.0) },
    watch: { steve: P(22.4, 20.2), alex: P(23.8, 19.5) },
  };
  // Click a friend and they say something (./speech.ts): what they have to say of whichever
  // feature is in sight, gone by the next. Some of it only once something has happened
  // there, at the next clicks after: Herobrine let in and gone, the house blown up, the
  // house back. (Clicking the stranger lets him in instead.)
  const talks = COPY.talk;
  const features = [talks.gate, talks.market, talks.night] as const;
  const happened = (k: number, friend: Friend): After[] =>
    k === 0
      ? [{ since: () => (left ? letAt + LEAVES : null), lines: talks.gate.left[friend] }]
      : k === 2
        ? [
            { since: () => (rebuilt === null ? blown : null), lines: talks.night.blown[friend] },
            { since: () => rebuilt, lines: talks.night.restored[friend] },
          ]
        : [];
  for (const [one, friend] of [[steve, "Steve"], [alex, "Alex"]] as const) {
    const middle = new Vector3();
    const here = () => one.shown > 0.5;
    (C.BEATS as [number, number][]).forEach((beat, k) => {
      const talk = ctx.speech.speaker({ rig: one.player.name, seen: beat, here, headline: [heads[k]], lines: features[k][friend], after: happened(k, friend) });
      ctx.touch.add({ name: `home_${friend}_${k}`, at: () => worldOf(one.player.root, middle).add(new Vector3(0, 0, 1)), half: new Vector3(0.45, 0.45, 1.05), seen: beat, here, click: talk });
    });
  }

  // The gate's two leaves: they swing into the yard.
  const leaves = [ctx.node("Home_Gate_0"), ctx.node("Home_Gate_1")];
  const leafYaw = leaves.map((leaf) => leaf.rotation.z);
  let gateOpen = 0; // 0 shut, 1 open (eased)
  let gateWant = 0;
  let rattle = -Infinity;

  // -- the gate: who gets in ------------------------------------------------------------------------
  let gateStart: number | null = null;
  let tries = 0; // which of the stranger's tries this is
  let letIn = false;
  let letAt = -Infinity;
  let turnedAt = -Infinity;
  /** Herobrine, let in, has left the game; and where he stood, for the friends to stare at. */
  let left = false;
  const stood = new Vector3();
  const strangerAt = new Vector3();
  let strangerTip: Card | null = null;
  ctx.touch.add({
    name: "stranger",
    at: () => worldOf(stranger.player.root, strangerAt).add(new Vector3(0, 0, 1)),
    half: new Vector3(0.45, 0.45, 1.05),
    seen: gateBeat,
    here: () => stranger.shown > 0.5 && !letIn, // (let in, he is nothing to click)
    hang: "under",
    tip: () => {
      if (letIn || !stranger.out || !ctx.ui) return null;
      return (strangerTip ??= ctx.ui.draw.tooltip(say.whitelist.stranger, [say.whitelist.kept, say.whitelist.letIn]));
    },
    click: (time) => {
      if (letIn || !stranger.out) return;
      letIn = true;
      letAt = time;
      ctx.ui?.say(say.whitelist.added, GREEN, time + 0.1, { stay: 5, from: gateBeat[0] - 0.05, until: bookBeat[1] });
    },
  });

  const playGate = (time: number) => {
    if (gateStart === null) {
      gateStart = time;
      tries = 0;
      letIn = false;
      left = false;
      stranger.player.life = 1;
      for (const one of people) {
        one.put(tunnel, 0);
        one.out = false;
      }
      gateWant = 0;
    }
    const u = time - gateStart;
    // The friends: out of the tunnel, up to the gate, in.
    for (const [one, start, spot, side] of [[steve, 0.2, place.yard.steve, -0.35], [alex, 0.75, place.yard.alex, 0.35]] as const) {
      if (u < start) continue;
      one.out = true;
      const waiting = new Vector3(outside.x + side, outside.y, floor);
      if (one.at.y < outside.y - 0.05 && one.goal.y < outside.y) one.go(waiting);
      if (one.at.y >= outside.y - 0.06 && gateOpen > 0.7) one.go(spot, one === steve ? yawTo(spot, gateAt) : yawTo(spot, gateAt));
      if (one.at.y >= outside.y - 0.06 && gateOpen < 0.7 && one.goal.y < spot.y) gateWant = 1; // the gate opens for a friend
    }
    const friendsIn = steve.at.y > gateAt.y + 1.2 && alex.at.y > gateAt.y + 1.2;
    if (friendsIn && !letIn && u > 3) gateWant = 0;
    // The stranger: a try at the gate every so often, until someone lets him in.
    const v = u - 3.4 - tries * TRY;
    if (letIn) {
      // In he comes, between the friends. As the lights go he stops where he is and turns
      // to the lens, as still as a statue, so that in the dark his eyes look out of it; and
      // he leaves the game at once, from where he stood. (He does not come back while the
      // gate's feature is in sight.)
      const since = time - letAt;
      const spot = P(22.1, 15.3);
      if (since < OUT) {
        stranger.out = true;
        if (stranger.at.y < outside.y - 0.3) stranger.go(outside);
        else stranger.go(spot, yawTo(spot, place.yard.steve));
      } else stranger.go(stranger.at, yawTo(stranger.at, stage.camera.position));
      stranger.player.life = since < OUT ? 1 : 0;
      if (since >= LEAVES && !left) {
        left = true;
        stranger.out = false;
        stranger.shown = 0; // (no fade: simply not there)
        worldOf(stranger.player.root, stood).add(new Vector3(0, 0, 1.5));
        ctx.ui?.say(say.whitelist.left, YELLOW, time, { stay: 5, from: gateBeat[0] - 0.05, until: bookBeat[1] });
      }
      gateWant = left ? 0 : 1;
      if (stranger.at.y > gateAt.y + 1.0 && since > 2.5) gateWant = 0;
    } else if (v >= 0) {
      stranger.out = v < TRY - 0.4;
      // Up to the gate, facing it; he tries it (it rattles, and the chat says no), and only then turns away.
      if (v < 2.4) stranger.go(outside, yawTo(outside, gateAt));
      else if (v < 4.6) {
        stranger.go(outside, v < 3.1 ? yawTo(outside, gateAt) : 0);
        if (turnedAt < gateStart + 3.4 + tries * TRY) {
          turnedAt = time;
          rattle = time;
          ctx.ui?.say(say.whitelist.turned, RED, time, { stay: 5, from: gateBeat[0] - 0.05, until: gateBeat[1] + 0.05 });
          // The first time a visit sees him turned away, the game's advancement for turning a
          // projectile with a shield: clear of the gate's headline, gone with it, and out of
          // the way of the market's.
          ctx.ui?.toast(COPY.toasts.events.shield, SHIELD, time, { from: gateBeat[0] - 0.05, until: gateBeat[1], under: [heads[0], heads[1]] });
        }
      } else stranger.go(tunnel);
      if (v >= TRY) tries += 1;
    }
  };

  // -- plugins: the market's book ----------------------------------------------------------------------
  const lanterns = ctx.stage.data.constants ? [...stage.nodes.keys()].filter((name) => /^Home_Lantern_\d+$/.test(name)).sort().map((name) => ctx.node(name)) : [];
  const lanternLights = lamps.named("Home_LanternLight_");
  const installed: (number | null)[] = [null, null, null]; // when each plugin went in
  let bookStart: number | null = null;
  const install = (k: number, time: number) => {
    if (installed[k] !== null) return;
    installed[k] = time;
    ctx.ui?.say(`[Plugins] ${say.market.plugins[k]} ${say.market.said}`, GREEN, time + 0.05, { stay: 4.5, from: bookBeat[0] - 0.05, until: rewindBeat[1] });
  };
  say.market.plugins.forEach((_, k) => ctx.touch.button(`plugin-${k}`, (time) => install(k, time)));
  // The lectern itself: a click installs the next one.
  ctx.touch.add({
    name: "lectern",
    at: lectern,
    half: new Vector3(0.55, 0.55, 0.6),
    seen: bookBeat,
    tip: () => ctx.ui?.draw.tooltip(say.market.title, [`${say.market.plugins.length} plugins`]) ?? null,
    click: (time) => {
      const next = installed.findIndex((at) => at === null);
      if (next >= 0) install(next, time);
    },
  });
  // The book lies open on it, and Steve turns its pages: two leaves, and one that turns.
  // (In the book's own frame: its spine along Y at X = 0, its top 3 pixels above the pivot.)
  const pageLook = new MeshBasicMaterial({ color: 0xd8ccab });
  const book = ctx.find("Home_Lectern_book");
  const pageAt = (side: number) => {
    // A hair off the spine, so the two halves read as two, with the spine's dark between.
    const page = new Mesh(new BoxGeometry(0.31, 0.54, 0.025).translate(0.175 * side, 0, 0), pageLook);
    page.position.set(0, 0, 3.4 / 16);
    page.rotation.y = -side * 9 * RAD; // the two halves rise from the spine, as an open book's do
    book?.add(page);
    return page;
  };
  pageAt(-1);
  pageAt(1);
  const turning = pageAt(1);
  /** Seconds between page turns, and how long one takes. */
  const TURN = 3.4;
  const FLIP = 0.75;
  // Fireflies, and fireworks, made here.
  const glowLook = new MeshBasicMaterial({ color: 0xd8ff7a, transparent: true, blending: AdditiveBlending, depthWrite: false });
  const fly = new BoxGeometry(0.07, 0.07, 0.07);
  const flies = Array.from({ length: 46 }, () => {
    const mesh = new Mesh(fly, glowLook);
    mesh.userData = { home: new Vector3(lerp(19.5, 25, random()), lerp(13.5, 26.5, random()), floor + lerp(0.4, 2.6, random())), phase: random() * 6.28, speed: lerp(0.4, 0.9, random()) };
    mesh.visible = false;
    stage.scene.add(mesh);
    return mesh;
  });
  // A firework star as the game's: one colour bursting into a ball, fading to a second as it falls.
  const STARS = [
    ["#ff4f5e", "#ffc36b"],
    ["#5fd8ff", "#e9f8ff"],
    ["#ffe14a", "#ff8a3d"],
  ].map(([from, to]) => ({ from: new Color(from), to: new Color(to) }));
  const ember = new BoxGeometry(0.1, 0.1, 0.1);
  const SPARKS = 48;
  const rockets = [0, 1, 2].map((k) => {
    const trail = new MeshBasicMaterial({ color: 0xfff1c8, transparent: true, blending: AdditiveBlending, depthWrite: false });
    const look = new MeshBasicMaterial({ color: STARS[k].from, transparent: true, blending: AdditiveBlending, depthWrite: false });
    const rocket = new Mesh(ember, trail);
    const sparksOf = Array.from({ length: SPARKS }, (_, j) => {
      const spark = new Mesh(ember, look);
      // Spread evenly over a ball (a Fibonacci spiral), each a little faster or slower.
      const z = 1 - (2 * (j + 0.5)) / SPARKS;
      const r = Math.sqrt(1 - z * z);
      const a = j * 2.39996;
      spark.userData.out = new Vector3(r * Math.cos(a), r * Math.sin(a), z).multiplyScalar(lerp(2.2, 2.7, random()));
      spark.userData.twinkle = random() * 6.28;
      return spark;
    });
    for (const thing of [rocket, ...sparksOf]) {
      thing.visible = false;
      stage.scene.add(thing);
    }
    const lamp = lamps.byName.get(`Home_Burst_${k}`) ?? null;
    return { rocket, sparks: sparksOf, look, trail, lamp, star: STARS[k], from: new Vector3(), side: [-0.62, 0.34, 0.8][k], burst: new Vector3(), cycle: -1, every: 2.9 + 0.45 * k, offset: 0.8 * k };
  });

  // -- the blast, and time turned back ------------------------------------------------------------
  const blocks = [...stage.nodes].filter(([name]) => /^Home_Block_\d+$/.test(name)).map(([, node]) => node);
  const homes = new Map(blocks.map((block) => [block, block.position.clone()]));
  const blast = vec(spots.blast).add(new Vector3(0, 0, 1.0));
  const flights = new Map(
    blocks.map((block) => {
      const home = homes.get(block)!;
      const away = new Vector3(home.x - blast.x, home.y - blast.y, 0);
      const near = Math.max(away.length(), 0.6);
      away.normalize().multiplyScalar(lerp(3.5, 7.5, random()) / Math.sqrt(near) * 1.6);
      return [block, { out: away, up: lerp(3.0, 7.0, random()) / Math.sqrt(near), spin: new Vector3(random() - 0.5, random() - 0.5, random() - 0.5).multiplyScalar(14), lag: random() * 0.05 }];
    }),
  );
  const window = ctx.find("Home_Window_Lit");
  const windowBlock = blocks.find((block) => {
    const home = homes.get(block)!;
    return Math.abs(home.x - (house.x[0] + 0.5)) < 0.01 && Math.abs(home.y - (house.window + 0.5)) < 0.01 && Math.abs(home.z - (floor + 1.5)) < 0.01;
  });
  // Smoke: puffs that billow out of the blast and rise.
  // (Shades of the game's explosion smoke: pale at the heart, darker and greyer at the edge.)
  const smokeLook = [0.78, 0.62, 0.5, 0.4].map((v) => new MeshBasicMaterial({ color: new Color(v, v * 0.98, v * 0.95), transparent: true, opacity: 0.9, depthWrite: false }));
  const puffs = Array.from({ length: 34 }, (_, k) => {
    const puff = new Mesh(new BoxGeometry(1, 1, 1), smokeLook[k % smokeLook.length].clone());
    puff.userData = { dir: new Vector3(random() - 0.5, random() - 0.5, lerp(0.1, 1.0, random())).normalize().multiplyScalar(lerp(1.2, 3.6, random())), size: lerp(0.25, 0.8, random()), spin: random() * 3 };
    puff.visible = false;
    stage.scene.add(puff);
    return puff;
  });
  // The creeper: from the dark to the north, to the house's wall.
  const creeper = {
    root: ctx.node("Home_Creeper"),
    head: ctx.find("Home_Creeper_head"),
    legs: ["leg_fr", "leg_bl", "leg_fl", "leg_br"].map((key) => ctx.find(`Home_Creeper_${key}`)),
  };
  const creeperFrom = creeper.root.position.clone();
  const creeperTo = new Vector3(blast.x, blast.y + 0.3, floor);
  const flashLook = new MeshBasicMaterial({ color: 0xffffff, transparent: true, opacity: 0, depthWrite: false });
  const flash = new Mesh(new BoxGeometry(0.62, 0.62, 1.7), flashLook);
  flash.position.set(0, 0, 0.85);
  creeper.root.add(flash);
  const creeperAt = new Vector3();
  let poked = -Infinity;
  ctx.touch.add({
    name: "creeper",
    at: () => worldOf(creeper.root, creeperAt).add(new Vector3(0, 0, 0.8)),
    half: new Vector3(0.45, 0.45, 0.85),
    seen: [rewindBeat[0], AT_WALL + 0.02],
    tip: () => ctx.ui?.draw.tooltip("Creeper", ["Up to no good."]) ?? null,
    click: (time) => void (poked = time),
  });
  /** Seconds of the blast at a scroll position: running on to its end, then back to its start as time turns back. */
  const BLAST_TIME = 1.25;
  const blastAt = (s: number) => {
    if (s < BLAST) return -1;
    if (s < BACK) return BLAST_TIME * clamp01((s - BLAST) / (BACK - BLAST));
    const rewound = BACK + 0.6 * (WHOLE - BACK); // the house is whole again here; then the creeper goes back
    return BLAST_TIME * (1 - clamp01((s - BACK) / (rewound - BACK)));
  };
  const reforms = BACK + 0.6 * (WHOLE - BACK);
  /** When the house was blown up, and when it was whole again after, as the scroll last went: for what the friends say of it. */
  let blown: number | null = null;
  let rebuilt: number | null = null;
  /** The year it is: time turned back counts from here back to the backup's (COPY.homestead.rewind.from). */
  const yearNow = Math.max(thisYear(), COPY.homestead.rewind.from);
  let aimed = false;
  const camera = stage.camera;
  const ahead = new Vector3();
  const side = new Vector3();
  const UP = new Vector3(0, 0, 1);
  const lift = new Vector3();
  /** The latest burst: where the friends look. */
  const seen = new Vector3();

  // -- the interface: the market's panel, and the rewind's clock ------------------------------------
  const cards = new Map<string, Card>();
  const card = (key: string, make: () => Card) => {
    let made = cards.get(key);
    if (!made) cards.set(key, (made = make()));
    return made;
  };

  // -- the blackout: Herobrine let in, and every light of the set out at once -----------------------
  // Each lamp (engine/lamps.ts, `level`), the sky's light down the shaft, and whatever has
  // light of its own, as the art made it: the glowing blocks of the cavern and of the rock
  // beyond the gate, the torch's flame, the pumpkin's face, the headlines' letters, the dust
  // in the sunbeam. His eyes are not among them (anim/player.ts). (The blocks' glow is the
  // world's terrain material's, which the valley shares: it is not drawn while the gate is
  // in sight, film.ts.)
  const own: [IUniform, number | number[]][] = [];
  const looked = new Set<ShaderMaterial>();
  for (const [name, node] of stage.nodes) {
    if (!/^(Home_|Dungeon_|Kurbis)/.test(name) && name !== `dungeon.${stage.aspect}` && name !== "under.terrain") continue;
    node.traverse((part) => {
      for (const material of [(part as Mesh).material ?? []].flat() as ShaderMaterial[]) {
        if (!material.uniforms || looked.has(material)) continue;
        looked.add(material);
        for (const key of ["uEmit", "uGlow", "uSelf"]) {
          const uniform = material.uniforms[key];
          const value: unknown = uniform?.value;
          if (typeof value === "number" && value > 0) own.push([uniform, value]);
          else if (Array.isArray(value) && value.some((v) => v > 0)) own.push([uniform, [...(value as number[])]]);
        }
      }
    });
  }
  let darkened = 1;
  /** How much light the set has this frame (1: as the scene wants it). Once a frame, after the light is set. */
  const darken = (k: number) => {
    stage.lamps.level = k;
    if (k !== darkened) {
      darkened = k;
      for (const [uniform, base] of own) {
        if (typeof base === "number") uniform.value = base * k;
        else for (let i = 0; i < base.length; i += 1) (uniform.value as number[])[i] = base[i] * k;
      }
    }
    if (k === 1) return;
    const { globals } = stage;
    for (const sky of [globals.uSkyUp, globals.uSkySide, globals.uSkyDown]) sky.value.multiplyScalar(k);
    stage.skyStrength.value *= k;
    (opening.material as MeshBasicMaterial).color.multiplyScalar(k);
    beamLook.color.multiplyScalar(k);
  };

  let last = 0;
  let lastBeat = -1;
  tl.during(3.75, C.THROUGH + 0.15, ({ s, time }) => {
    const dt = Math.min(Math.max(time - last, 0), 0.1);
    last = time;
    const on = lit(s);
    for (const { name, base } of steady) lamps.set(name, base * on);
    lamps.set(torch.name, torch.base * on * flicker(time, 2.3));
    const level = dayAt(s, tint);
    if (sky) {
      sky.color.copy(tint);
      sky.energy = skyBase * on * level;
    }
    (opening.material as MeshBasicMaterial).color.copy(tint).multiplyScalar(0.6 + 2.2 * level);
    beamLook.color.copy(tint).multiplyScalar(0.55 * level + 0.05);
    for (const thing of [opening, ...beams]) thing.visible = on > 0;
    heads.forEach((head, k) => {
      if (!head) return;
      const [s0, s1] = (C.BEATS as [number, number][])[k];
      grow(head, POP(headsIn[k].t / 0.28) * (s < s1 ? 1 : UNPOP((s - s1) / 0.04)) * (s >= s0 ? 1 : 0));
    });

    // The portal.
    grow(surface, POP(lighted.t / 0.2));
    sparks.visible = s >= C.PORTAL_LIT && s < C.THROUGH + 0.1;
    const glow = s < C.THROUGH + 0.02 ? clamp01(lighted.t / 0.1) : 0;
    for (const lamp of glows) lamp.energy = lamp.base * glow;

    const near = s > gateBeat[0] - 0.12 && s < rewindBeat[1] + 0.12;
    for (const one of people) one.player.root.visible = near && one.shown > 0.01;
    if (!near) {
      gateStart = null;
      bookStart = null;
      lastBeat = -1;
      blown = rebuilt = null;
      darken(1);
      return;
    }

    // Who is where, by the feature in sight.
    const ui = ctx.ui;
    const inGate = s < bookBeat[0] - 0.015;
    const inBook = !inGate && s < rewindBeat[0] - 0.02;
    if (inGate) {
      playGate(time);
    } else {
      gateStart = null;
      for (const one of [steve, alex]) one.out = true;
      stranger.out = false; // (Herobrine is only ever at the gate, let in or not)
      if (letIn) stranger.shown = 0; // (and once let in, he is gone at once with the dark it ends)
    }
    // A new feature in sight: everyone is already where it needs them, rather than
    // walking there through the lens while the camera moves.
    const beat = inGate ? 0 : inBook ? 1 : 2;
    if (beat !== lastBeat && lastBeat >= 0 && beat > 0) {
      const spots = beat === 1 ? place.book : place.watch;
      const aim = beat === 1 ? lectern : blast;
      steve.put(spots.steve, yawTo(spots.steve, aim));
      alex.put(spots.alex, yawTo(spots.alex, beat === 1 ? P(shaftMid.x, shaftMid.y, floor) : aim));
    }
    lastBeat = beat;
    if (inBook) {
      bookStart ??= time;
      steve.go(place.book.steve, yawTo(place.book.steve, lectern));
      alex.go(place.book.alex, yawTo(place.book.alex, P(shaftMid.x, shaftMid.y, floor)));
      for (let k = 0; k < 3; k += 1) if (installed[k] === null && time - bookStart > AUTO[k]) install(k, time);
    } else if (!inGate) {
      bookStart = null;
      steve.go(place.watch.steve, yawTo(place.watch.steve, blast));
      alex.go(place.watch.alex, yawTo(place.watch.alex, blast));
    }
    if (s < bookBeat[0] - 0.05) {
      installed.fill(null); // scrolled back before the market: nothing is installed yet
      seen.set(0, 0, 0);
    }
    // Herobrine let in puts the lights out, while the gate's feature is in sight: scrolled
    // on, they are as the scene wants them at once.
    const light = inGate && letIn ? lightsAt(time - letAt) : 1;
    darken(light);

    // The gate's leaves, swinging (and rattled when someone is turned away).
    gateOpen += (gateWant - gateOpen) * Math.min(dt * (gateWant > gateOpen ? 5 : 9), 1);
    // (Only for half a second after a rattle: before the first one, `time - rattle` is infinite and its sine is not a number.)
    const rattled = time - rattle;
    const shake = rattled < 0.5 ? (1 - rattled / 0.5) * Math.sin(rattled * 46) * 7 : 0;
    leaves.forEach((leaf, k) => (leaf.rotation.z = leafYaw[k] + (k ? -1 : 1) * (smooth(gateOpen) * 88 + shake) * RAD));

    // What each of them does, on top of walking.
    const blastTime = blastAt(s);
    // The light back after Herobrine has gone: the friends start, a little, and stare at where he stood.
    const after = inGate && left ? time - letAt - LEAVES : -1;
    const shaken = after > 0.05 && after < 1.9;
    const flinch = shaken ? clamp01((after - 0.05) / 0.12) * (1 - clamp01((after - 0.25) / 1.1)) : 0;
    const startled = Math.max(s >= BLAST && s < BACK ? Math.max(1 - Math.abs(blastTime - 0.12) / 0.5, 0) : 0, 0.4 * flinch);
    const hiss = s >= AT_WALL && s < BLAST ? clamp01((s - AT_WALL) / (BLAST - AT_WALL)) : 0;
    const restored = s >= WHOLE - 0.005 ? 1 : 0;
    // (Scrolled back before the blast, neither has happened; whole again only after it was blown up.)
    if (s < BLAST) blown = rebuilt = null;
    else if (!restored) [blown, rebuilt] = [blown ?? time, null];
    else if (blown !== null) rebuilt ??= time;
    const readNow = inBook ? time - (bookStart ?? time) : 0;
    // A page turns every few seconds: his hand goes to it, and it sweeps over.
    const leaf = inBook ? (readNow % TURN) - (TURN - FLIP) : -1;
    const flip = leaf > 0 ? smooth(leaf / FLIP) : 0;
    const reach = leaf > -0.35 && leaf < FLIP ? Math.sin(clamp01((leaf + 0.35) / (FLIP + 0.35)) * Math.PI) : 0;
    turning.rotation.y = (-9 - 162 * flip) * RAD; // from lying on the right to lying on the left
    turning.position.z = (3.4 + 0.6 * Math.sin(flip * Math.PI)) / 16; // it lifts off the others as it goes over
    pageLook.color.setRGB(0.74, 0.68, 0.55).multiplyScalar((0.35 + 0.65 * lit(s)) * light);
    const latest = Math.max(...installed.map((at) => at ?? -Infinity));
    const cheer = time - latest < 1.4 ? Math.sin(clamp01((time - latest) / 1.4) * Math.PI) : 0;
    // Once the fireworks are in, Steve looks up from the book at each burst, now and then.
    const watching = cheer > 0 || (installed[2] !== null && seen.lengthSq() > 0 && Math.sin(time * 0.9) > 0.1);
    // Steve: reading (a page turns now and then), cheering what a plugin does; at night, shielding himself from the blast.
    const steveDo: Delta = inBook
      ? { head: [10 + 4 * reach, -8 * reach, 0], armR: [-38 - 8 * Math.sin(readNow * 1.7) - 14 * reach, 0, 6 + 18 * reach], elbowR: 48 - 18 * reach, armL: [-30 + cheer * -110, cheer * -30, 0], elbowL: 40 - 30 * cheer }
      : { armR: [-70 * startled, 0, 0], armL: [-70 * startled, 0, 0], elbowR: 80 * startled, elbowL: 80 * startled, body: [-14 * startled, 0, 0], lift: 0.1 * startled };
    steve.player.look = inGate ? (shaken ? stood : stranger.out ? worldOf(stranger.player.root, new Vector3()).add(new Vector3(0, 0, 1.4)) : null) : inBook ? (watching ? seen : lectern) : s < BLAST ? worldOf(creeper.root, new Vector3()).add(new Vector3(0, 0, 1)) : blast;
    steve.player.face.wide = hiss > 0.3 || startled > 0.2 || shaken;
    steve.player.face.mouth = startled > 0.2 ? "open" : cheer > 0.2 || restored ? "smile" : "closed";
    // Alex: waves the stranger off when he is turned away; looks up at fireworks; jumps for joy when the house is back.
    const turned = time - turnedAt;
    const bye = inGate && turned > 0.6 && turned < 2.8 ? Math.sin(clamp01((turned - 0.6) / 2.2) * Math.PI) : 0;
    const joy = restored ? Math.max(0, Math.sin(time * 7.5)) : 0;
    const alexDo: Delta = {
      armL: [0, -150 * bye + 20 * bye * Math.sin(time * 13), 0],
      armR: [-80 * startled - 120 * restored, 0, 0],
      elbowR: 70 * startled,
      kneeR: 30 * joy,
      kneeL: 30 * joy,
      lift: 0.28 * joy,
      body: [-10 * startled, 0, 0],
    };
    // Alex, at the market, watches the sky for each burst (Steve's book before there are any).
    alex.player.look = inBook && installed[2] !== null && seen.lengthSq() > 0 ? seen : steve.player.look;
    alex.player.face.mouth = startled > 0.2 ? "open" : bye > 0.1 || restored || cheer > 0.2 ? "smile" : "closed";
    alex.player.face.wide = steve.player.face.wide;
    // The stranger: at the gate he finds it shut, and shrugs; let in, he waves, until the lights go.
    const v = time - turnedAt;
    const push = !letIn && v > 0 && v < 0.7 ? Math.sin(clamp01(v / 0.7) * Math.PI) : 0; // both hands on the gate
    const shrug = !letIn && v > 0.8 && v < 2.2 ? Math.sin(clamp01((v - 0.8) / 1.4) * Math.PI) : 0; // turned from it
    const thanks = letIn && time - letAt < OUT ? Math.sin(clamp01((time - letAt) / OUT) * Math.PI) : 0;
    const strangerDo: Delta = {
      armR: [-75 * push, 70 * shrug, 0],
      armL: [-75 * push, -70 * shrug + (-150 + 20 * Math.sin(time * 13)) * thanks, 0],
      elbowR: 70 * shrug + 15 * push,
      elbowL: 70 * shrug + 15 * push,
      head: [-6 * shrug, 14 * shrug, 0],
      body: [0, 0, 0],
    };
    stranger.player.face.mouth = thanks > 0.1 ? "smile" : shrug > 0.1 ? "open" : "closed";
    stranger.player.look = letIn ? (time - letAt < OUT ? P(place.yard.steve.x, place.yard.steve.y, floor + 1.5) : stage.camera.position) : push > 0 || shrug > 0 ? P(gateAt.x, gateAt.y, floor + 0.8) : null;
    steve.update(time, dt, steveDo);
    alex.update(time, dt, alexDo);
    stranger.update(time, dt, strangerDo);

    // -- plugins: what each one does ------------------------------------------------------------
    const dusk = clamp01((s - bookBeat[0]) / 0.08);
    lanterns.forEach((lantern, k) => {
      const at = installed[0];
      const k0 = at === null ? 0 : POP((time - at - 0.08 * k) / 0.3);
      grow(lantern, k0);
    });
    lanternLights.forEach((lamp, k) => {
      const at = installed[0];
      lamp.energy = at === null ? 0 : 45 * clamp01((time - at - 0.08 * k) / 0.2) * (0.35 + 0.65 * dusk) * flicker(time, 1.3 * k);
    });
    const fliesOn = installed[1] === null ? 0 : clamp01((time - installed[1]) / 1.2);
    flies.forEach((mesh) => {
      const { home, phase, speed } = mesh.userData as { home: Vector3; phase: number; speed: number };
      mesh.visible = fliesOn > 0;
      if (!mesh.visible) return;
      const t = time * speed + phase;
      mesh.position.set(home.x + 0.8 * Math.sin(t * 0.9), home.y + 0.8 * Math.sin(t * 0.7 + 1.3), home.z + 0.4 * Math.sin(t * 1.3));
      const near = clamp01((mesh.position.distanceTo(camera.position) - 1.2) / 1.3); // none drifts into the lens
      mesh.scale.setScalar((0.4 + 0.6 * Math.max(0, Math.sin(t * 3.1))) * fliesOn * near);
    });
    glowLook.color.setRGB(0.85, 1, 0.48).multiplyScalar(0.4 + 0.8 * dusk);
    for (const rocket of rockets) {
      const start = installed[2];
      const since = start === null ? -1 : time - start - rocket.offset;
      const cycle = since < 0 ? -1 : Math.floor(since / rocket.every);
      const u = since < 0 ? -1 : since % rocket.every;
      if (cycle !== rocket.cycle && cycle >= 0) {
        const k = rockets.indexOf(rocket);
        // Each launch from where the camera looks now: out past the friends, to one side,
        // bursting under the dome in the frame's middle (the headline has the top).
        rocket.cycle = cycle;
        camera.getWorldDirection(ahead);
        side.crossVectors(ahead, UP).normalize();
        lift.crossVectors(side, ahead).normalize();
        const far = 6.2 + 0.6 * (cycle % 2); // in front of the house, behind the friends
        const spread = Math.min(camera.aspect / 1.6, 1) * 3.6;
        // The burst's heart: a little above the frame's middle, clear of the headline.
        rocket.burst
          .copy(camera.position)
          .addScaledVector(ahead, far)
          .addScaledVector(side, (rocket.side + 0.12 * Math.sin(cycle * 2.3)) * spread)
          .addScaledVector(lift, far * (0.14 + 0.03 * k));
        rocket.from.set(rocket.burst.x, rocket.burst.y, floor + 0.3);
      }
      const { from } = rocket;
      const showing = start !== null && s < rewindBeat[1];
      // Up it goes, wobbling, a spark trailing it...
      const up = showing && u >= 0 && u < 0.7;
      rocket.rocket.visible = up;
      if (up) {
        rocket.rocket.position.set(from.x + 0.05 * Math.sin(u * 47), from.y + 0.05 * Math.cos(u * 39), lerp(from.z, rocket.burst.z, easeOut(u / 0.7, 2)));
        rocket.rocket.scale.set(0.7, 0.7, 2.2); // drawn out by its speed
      }
      // ...and bursts: fast out, the air holds the stars, they fall and fade to their second colour, twinkling out.
      const t = u - 0.7;
      const bursting = showing && t >= 0 && t < 1.8;
      const fading = clamp01((t - 0.5) / 0.9);
      rocket.look.color.copy(rocket.star.from).lerp(rocket.star.to, fading).multiplyScalar(1.6 - 0.9 * fading);
      for (const bit of rocket.sparks) {
        bit.visible = bursting;
        if (!bursting) continue;
        const out = bit.userData.out as Vector3;
        const go = ((1 - Math.exp(-3.6 * t)) / 3.6) * 2.4;
        bit.position.set(from.x + out.x * go, from.y + out.y * go, rocket.burst.z + out.z * go - 0.9 * t * t);
        const blink = t > 1.0 && Math.sin(time * 38 + (bit.userData.twinkle as number)) < -0.2 ? 0.25 : 1;
        bit.scale.setScalar((1 - clamp01((t - 1.2) / 0.6)) * blink * (t < 0.06 ? 1.8 : 1));
      }
      // The burst's light, on the cave and the faces: a flash, dying away.
      if (rocket.lamp) {
        rocket.lamp.at.copy(rocket.burst);
        rocket.lamp.color.copy(rocket.star.from);
        rocket.lamp.energy = bursting ? 380 * Math.exp(-4 * t) : 0;
      }
      if (bursting && t < 1.2) seen.copy(rocket.burst);
    }

    // -- the blast, and time turned back ----------------------------------------------------------------
    // The creeper: out of the dark to the wall, hissing, gone; back again, walking backwards into the dark.
    const walkIn = clamp01((s - rewindBeat[0]) / (AT_WALL - rewindBeat[0]));
    const walkOut = clamp01((s - reforms) / (WHOLE - reforms));
    const creeperHere = (s >= rewindBeat[0] - 0.02 && s < BLAST) || (s >= reforms && s < WHOLE);
    const spot = s < BLAST ? walkIn : 1 - walkOut;
    creeper.root.position.lerpVectors(creeperFrom, creeperTo, smooth(spot));
    creeper.root.visible = creeperHere && on > 0;
    const startle = Math.max(1 - (time - poked) / 0.6, 0);
    const swell = s < BLAST ? hiss : walkOut < 0.15 ? 1 - walkOut / 0.15 : 0;
    creeper.root.scale.setScalar((1 + 0.22 * swell * (0.7 + 0.3 * Math.sin(time * 38)) + 0.12 * startle) * (creeperHere ? 1 : 0.001));
    // It flashes white as it hisses, as the game's does: quick blinks that come faster.
    flashLook.opacity = swell > 0.2 && Math.sin(time * 14 * (0.6 + swell)) > 0.55 ? 0.42 * swell : 0;
    const stepping = (s < BLAST ? walkIn < 1 : walkOut > 0.15 && walkOut < 1) ? Math.sin(spot * 34) : 0;
    creeper.legs.forEach((leg, k) => leg?.rotation.set((k % 2 ? -1 : 1) * 28 * stepping * RAD, 0, 0));
    if (creeper.head) creeper.head.rotation.set(0, 0, 0.25 * Math.sin(time * 1.3) * (1 - swell));
    // The house: blown apart, every block on its own path; and back.
    if (!aimed && blastTime > 0) {
      // None flies at the lens (a plank filling the frame): those heading for it fall short.
      aimed = true;
      const lens = new Vector3(camera.position.x - blast.x, camera.position.y - blast.y, 0).normalize();
      for (const flight of flights.values()) {
        const toward = flight.out.clone().normalize().dot(lens);
        if (toward > 0.3) flight.out.multiplyScalar(lerp(1, 0.3, clamp01((toward - 0.3) / 0.5)));
      }
    }
    for (const block of blocks) {
      const home = homes.get(block)!;
      const { out, up, spin, lag } = flights.get(block)!;
      const t = blastTime - lag;
      if (t <= 0) {
        block.position.copy(home);
        block.rotation.set(0, 0, 0);
        block.visible = true;
        continue;
      }
      const z = home.z + up * t - 9.8 * t * t * 0.5;
      const ground = floor + 0.5;
      const flying = z > ground;
      // Once down, it lies where it fell.
      const landed = flying ? t : (up + Math.sqrt(up * up + 2 * 9.8 * (home.z - ground))) / 9.8;
      block.position.set(home.x + out.x * landed, home.y + out.y * landed, Math.max(z, ground));
      block.rotation.set(spin.x * landed, spin.y * landed, spin.z * landed);
      block.visible = true;
    }
    if (window && windowBlock) {
      // The lit window (by night) goes where its block goes.
      const night = nightAt(C, s);
      window.visible = night;
      windowBlock.visible = !night;
      window.position.copy(windowBlock.position);
      window.rotation.copy(windowBlock.rotation);
    }
    lamps.set("Home_Window", nightAt(C, s) && blastTime < 0.05 ? 160 * on : 0);
    // The flash and the shake: the blast's first moment, going forwards (time turned back is quiet).
    const bang = s >= BLAST && s < BACK && blastTime > 0 ? Math.max(1 - blastTime / 0.45, 0) : 0;
    lamps.set("Home_Blast", 9000 * bang ** 2 * on);
    puffs.forEach((puff) => {
      const { dir, size, spin } = puff.userData as { dir: Vector3; size: number; spin: number };
      puff.visible = blastTime > 0.01;
      if (!puff.visible) return;
      const t = blastTime;
      const grown = easeOut(clamp01(t / 0.9), 3);
      puff.position.set(blast.x + dir.x * grown, blast.y + dir.y * grown, blast.z + dir.z * grown + 0.6 * t);
      puff.scale.setScalar(size * (0.3 + grown) * (1 - clamp01((t - 0.9) / 0.35) * 0.6));
      puff.rotation.set(spin * t, spin * 0.7 * t, 0);
      (puff.material as MeshBasicMaterial).opacity = 0.8 * (1 - clamp01((t - 0.35) / 0.9)) * (s < WHOLE ? 1 : 0);
      // In the blast's first instant the smoke is lit by it.
      (puff.material as MeshBasicMaterial).color.setScalar(0.4 + 0.5 * bang + 0.25 * ((puff.id % 4) / 4));
    });
    // The camera shakes with the blast.
    if (bang > 0) {
      camera.position.x += 0.12 * bang * Math.sin(time * 61);
      camera.position.z += 0.1 * bang * Math.sin(time * 47 + 1.3);
      camera.updateMatrixWorld();
    }

    // -- the interface ----------------------------------------------------------------------------------
    if (!ui) return;
    // The market, by the lectern.
    if (inBook && s > bookBeat[0] + 0.01) {
      const width = 128;
      const rows = say.market.plugins.length;
      const panel = card("market", () => ui.draw.panel(width, 22 + rows * 22));
      const left = tall ? (ui.hud.width - width) / 2 : 12;
      const bottom = tall ? 30 : ui.hud.height * 0.42 - panel.height / 2;
      const alpha = clamp01((s - bookBeat[0] - 0.01) / 0.03) * (1 - clamp01((s - bookBeat[1] + 0.03) / 0.03));
      // (A narrow frame has no room for it and the claim card both: the card is put away while it shows.)
      if (tall) ui.aside.hidden = Math.max(ui.aside.hidden, alpha);
      ui.extras.push({ card: panel, x: left, y: bottom, anchor: [0, 0], alpha });
      const top = bottom + panel.height;
      ui.extras.push({ card: card("market-title", () => ui.draw.words(say.market.title, YELLOW)), x: left + 6, y: top - 18, anchor: [0, 0], alpha });
      say.market.plugins.forEach((name, k) => {
        const y = top - 22 - (k + 1) * 22 + 4;
        ui.extras.push({ card: card(`plugin-name-${k}`, () => ui.draw.words(name, WHITE)), x: left + 6, y: y - 1, anchor: [0, 0], alpha });
        const done = installed[k] !== null;
        const hover = ctx.touch.pressing === `plugin-${k}`;
        const button = done
          ? card(`plugin-done-${k}`, () => ui.draw.words(say.market.installed, GREEN))
          : card(`plugin-${k}-${hover}`, () => ui.draw.button(null, say.market.install, "green", hover));
        ui.extras.push({ card: button, x: left + width - 6, y: done ? y - 1 : y - 7, anchor: [1, 0], alpha, press: done ? undefined : `plugin-${k}` });
      });
    }
    // Time turned back: a card that counts the years back to the backup's as the house comes
    // together, holds on it while the creeper backs off, and then says the world is restored.
    if (s >= BACK - 0.01 && s < WHOLE + 0.03) {
      const alpha = clamp01((s - BACK + 0.01) / 0.02) * (1 - clamp01((s - WHOLE - 0.01) / 0.02));
      // Low in the frame, over the yard (the headline has the top): over the chat, and in a
      // narrow frame over the claim card too, where the chat goes up over it.
      const bottom = tall ? Math.max(52, ui.cardTop + 50) : 26;
      if (s >= WHOLE - 0.005) {
        ui.extras.push({ card: card("rewind-done", () => ui.draw.tooltip(null, [say.rewind.restored])), x: ui.hud.width / 2, y: bottom, anchor: [0.5, 0], alpha });
      } else {
        // The tooltip's card, its line laid on it by hand: each figure of the year in a cell
        // as wide as the widest (a 1 is narrower), so neither the card nor the year jitters.
        const year = String(Math.round(lerp(yearNow, say.rewind.from, clamp01((s - BACK) / (reforms - BACK)))));
        const words = `<< ${say.rewind.restoring} `;
        const cell = Math.max(...[..."0123456789"].map((figure) => ui.draw.measure(figure))) + 1;
        const at = ui.draw.measure(words) + 1; // where the year begins in the line
        const width = at + year.length * cell + 8;
        const left = Math.round((ui.hud.width - width) / 2);
        ui.extras.push({ card: card(`rewind-${width}`, () => ui.draw.panel(width, 16)), x: left, y: bottom, anchor: [0, 0], alpha });
        ui.extras.push({ card: card("rewind-words", () => ui.draw.words(words, WHITE)), x: left + 4, y: bottom - 3, anchor: [0, 0], alpha });
        [...year].forEach((figure, k) => {
          const x = left + 4 + at + k * cell + Math.floor((cell - 1 - ui.draw.measure(figure)) / 2);
          ui.extras.push({ card: card(`figure-${figure}`, () => ui.draw.words(figure, WHITE)), x, y: bottom - 3, anchor: [0, 0], alpha });
        });
      }
    }
    void GREY;
  });

  void UNPOP;
  void fade;
}
