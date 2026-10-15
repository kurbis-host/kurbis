/**
 * The game's debug keys, which players press without thinking (the page hands them to the
 * film: components/landing/film-canvas.tsx).
 *
 *   F3     The debug screen: what the film knows of where the camera is and of what it
 *          draws, in the game's words and on its axes (Y up). Every value is a real one: the
 *          frames counted, the camera's place, the scroll, the GPU as the browser names it.
 *   F3+B   Hitboxes: a white box round each creature in view, as big as the game makes it,
 *          a red line round it at its eyes and a blue one the way it looks; and a white box
 *          round each thing that can be clicked that is not one of them, so the keys also
 *          show what there is to touch.
 *
 * (F1 and F2, which put the interface away and take a screenshot, are the film's: ./film.ts.)
 *
 * The game's right-hand column names the block under the crosshair. The film cannot: its
 * grid of blocks is built into faces off the page's thread and let go of (../world), and the
 * page never has the blocks' names. It names the creature under the pointer instead.
 */

import { Box3, Matrix4, Ray, Vector2, Vector3, type InterleavedBufferAttribute, type Mesh, type Object3D, type WebGLRenderer } from "three";
import { LineMaterial } from "three/addons/lines/LineMaterial.js";
import { LineSegments2 } from "three/addons/lines/LineSegments2.js";
import { LineSegmentsGeometry } from "three/addons/lines/LineSegmentsGeometry.js";
import { playersOf } from "../anim/player";
import { COPY } from "../copy";
import { WHITE, YELLOW, type Words } from "../engine/hud";
import { BUNDLE } from "../engine/version";
import type { Ctx } from "./context";
import { open } from "./touch";

const SAY = COPY.keys.screen;
const DEG = 180 / Math.PI;

/** Seconds from one writing of the debug screen's lines to the next. (The frames are counted over a whole second, as the game counts them.) */
const EVERY = 0.25;
/** How far off a creature still has its box (blocks): about as far as the game draws them. */
const REACH = 96;
/** How long the blue line the way a creature looks is (blocks), as the game draws it. */
const LOOK = 2;
/** The most lines drawn at once. */
const SEGMENTS = 4096;

const OVERWORLD = "minecraft:overworld";
const NETHER = "minecraft:the_nether";

/**
 * The biome of each part of the journey, by where the scroll is: the one each place would
 * be in, in the game.
 *
 *   forest          The valley: an oak wood round a pond. No biome of the game is autumn;
 *                   the season is in the leaves' colours, and the wood is an oak forest. The
 *                   dig under it is the forest's too: the game gives what lies under a place
 *                   that place's biome, down to a cave with a biome of its own.
 *   lush_caves      The cave, and the cart's tunnel out of it: the moss on its floor and roof,
 *                   and the vines hung with glow berries, are the lush caves'.
 *   plains          The homestead: grass in a shaft of daylight, a fenced yard, a house and a
 *                   field of wheat, which is the biome the game builds its farms in.
 *   nether_wastes   The Nether: netherrack round a lava sea, glowstone in its roof, a strider
 *                   on the lava. The Nether as it first was, and its commonest biome.
 */
function biomeAt(s: number, C: Ctx["C"]): string {
  if (s >= C.HOME || s < C.PART.arrival) return "minecraft:forest";
  if (s >= C.THROUGH) return "minecraft:nether_wastes";
  if (s >= C.PART.dungeon) return "minecraft:plains";
  return "minecraft:lush_caves";
}

// -- the game's axes and words --------------------------------------------------------
/** A place or a way in the film's axes (X east, Y north, Z up), on the game's: X east, Y up, Z south. */
export function gameAxes(v: { x: number; y: number; z: number }): [number, number, number] {
  return [v.x, v.z, -v.y];
}

/** An angle in degrees, brought into -180..180 as the game brings one (180 is -180). */
export function wrapDegrees(degrees: number): number {
  let d = degrees % 360;
  if (d >= 180) d -= 360;
  if (d < -180) d += 360;
  return d;
}

/** The yaw and pitch of a way to look (in the film's axes), in the game's degrees: yaw 0 looks south and 90 west; pitch is positive looking down. */
export function yawPitch(forward: { x: number; y: number; z: number }): [number, number] {
  const [x, y, z] = gameAxes(forward);
  const yaw = Math.atan2(-x, z) * DEG;
  const pitch = Math.asin(Math.min(Math.max(-y / (Math.hypot(x, y, z) || 1), -1), 1)) * DEG;
  return [wrapDegrees(yaw), wrapDegrees(pitch)];
}

export type Direction = keyof typeof SAY.towards;
const AROUND: readonly Direction[] = ["south", "west", "north", "east"];

/** Which way a yaw faces, rounded to the nearest of the four as the game rounds it. */
export function facing(yaw: number): Direction {
  return AROUND[Math.floor(yaw / 90 + 0.5) & 3];
}

/** A line of the debug screen with its values filled in. */
export function fill(template: string, values: Record<string, string | number>): string {
  return template.replace(/\{(\w+)\}/g, (all, key: string) => (key in values ? String(values[key]) : all));
}

/**
 * Where the camera is and which way it looks, as the game's lines say it: XYZ, Block, Chunk
 * and Facing (`short`: the facing without which way along the axes it is, for a narrow frame).
 */
export function whereLines(eye: { x: number; y: number; z: number }, forward: { x: number; y: number; z: number }, short = false): string[] {
  const [x, y, z] = gameAxes(eye);
  const [bx, by, bz] = [x, y, z].map(Math.floor);
  const [cx, cy, cz] = [bx >> 4, by >> 4, bz >> 4];
  const [yaw, pitch] = yawPitch(forward);
  const [direction, towards] = SAY.towards[facing(yaw)];
  return [
    fill(SAY.xyz, { x: x.toFixed(3), y: y.toFixed(5), z: z.toFixed(3) }),
    fill(SAY.block, { x: bx, y: by, z: bz, inX: bx & 15, inY: by & 15, inZ: bz & 15 }),
    fill(SAY.chunk, { x: cx, y: cy, z: cz, inX: cx & 31, inZ: cz & 31, regionX: cx >> 5, regionZ: cz >> 5 }),
    fill(short ? SAY.facingShort : SAY.facing, { direction, towards, yaw: yaw.toFixed(1), pitch: pitch.toFixed(1) }),
  ];
}
/**
 * The debug screen's columns for a frame `room` interface pixels wide, given how wide a line
 * is. Each line of the right-hand column stands on the same row as the left's, and is left
 * out where the two would meet. In a narrow frame (`narrow`) there is one column: the left's
 * lines, and then `more`. A line too wide for the frame is left out.
 */
export function arrange(left: readonly Words[], right: readonly Words[], more: readonly Words[], room: number, width: (line: Words) => number, narrow: boolean): [Words[], Words[]] {
  const strip = (line: Words) => (width(line) > 0 ? width(line) + 3 : 0); // (as Draw.debug lays it)
  const fits = (line: Words) => strip(line) + 2 <= room;
  if (narrow) return [[...left, ...more].filter(fits), []];
  const kept = left.filter(fits);
  const beside = right.map((line, i) => (strip(kept[i] ?? "") + strip(line) + 6 <= room ? line : ""));
  return [kept, beside.some((line) => width(line) > 0) ? beside : []];
}

// -- the creatures, as the game sizes them --------------------------------------------
/** How wide and tall the game makes a creature, and how high its eyes are (blocks). */
type Size = readonly [number, number, number];
const SIZES = {
  player: [0.6, 1.8, 1.62],
  creeper: [0.6, 1.7, 1.445],
  chicken: [0.4, 0.7, 0.644],
  /** A baby, as the jockey's rider is: half a zombie, its eyes where the game puts a baby's. */
  zombie: [0.3, 0.975, 0.93],
  strider: [0.9, 1.7, 1.445],
  minecart: [0.98, 0.7, 0.595],
  salmon: [0.7, 0.4, 0.26],
  fishing_bobber: [0.25, 0.25, 0.2125],
} as const satisfies Record<string, Size>;
/** The game draws a player at 15/16 of its model; the art draws the model whole, so a player's box is that much bigger. */
const PLAYER_SCALE = 16 / 15;
/** Where the game puts the eyes of a creature that does not say: this share of its height up. */
const EYES = 0.85;

interface Creature {
  /** The game's id for it. The film's own creatures go by the film's name, as a mod's do. */
  id: string;
  root: Object3D;
  /** What it looks with, if not the whole of it. */
  head: Object3D | null;
  /** Which way along its own Y it faces: the art's creatures look along -Y, a flier along +Y. */
  ahead: 1 | -1;
  /**
   * The game's size for it, at the scale the art set it, which its box keeps whatever the
   * creature does (as the game's does: a creeper swells, its box does not). One the game does
   * not have is boxed as it is (`own`), and its box follows it as it squashes and stretches.
   */
  size: Size | null;
  scale: number;
  own: Box3 | null;
  /** Its middle is where it stands (a fish, a bobber), not its feet. */
  centred: boolean;
  /** How high its eyes are, where the game's share of its height would not have them (blocks, as it stands unsquashed). */
  eyeHeight?: number;
  /** It is alive: the game rings only the living at their eyes. */
  living: boolean;
  // This frame: whether it is there to be seen, its box, its eyes and the way it looks.
  shown: boolean;
  box: Box3;
  eyes: Vector3;
  look: Vector3;
}

export interface Debug {
  /** Whether the debug screen shows (F3), and the hitboxes (F3+B). */
  readonly screen: boolean;
  readonly hitboxes: boolean;
  toggleScreen(): void;
  /** The hitboxes, shown or hidden; the chat says which, as the game's does. */
  toggleHitboxes(time: number): void;
}

export function setDebug(ctx: Ctx, film: { renderer: WebGLRenderer; end: number }): Debug {
  const { stage, tl, C } = ctx;
  const { camera } = stage;

  // Every creature of the film, measured as it stands when the film is made.
  stage.scene.updateMatrixWorld(true);
  const creatures: Creature[] = [];
  const inverse = new Matrix4();
  const relative = new Matrix4();
  const piece = new Box3();
  /** A thing's own bounds about its root (without the root's own place, turn and size). */
  const ownBox = (root: Object3D) => {
    const box = new Box3();
    inverse.copy(root.matrixWorld).invert();
    root.traverse((object) => {
      const geometry = (object as Mesh).isMesh ? (object as Mesh).geometry : null;
      if (!geometry) return;
      if (!geometry.boundingBox) geometry.computeBoundingBox();
      box.union(piece.copy(geometry.boundingBox!).applyMatrix4(relative.multiplyMatrices(inverse, object.matrixWorld)));
    });
    return box;
  };
  const add = (root: Object3D | null, id: string, how: { size?: Size; scale?: number; head?: Object3D | null; ahead?: 1 | -1; centred?: boolean; living?: boolean; eyeHeight?: number } = {}) => {
    if (!root) return;
    creatures.push({
      id,
      root,
      head: how.head ?? null,
      ahead: how.ahead ?? -1,
      size: how.size ?? null,
      scale: how.size ? root.getWorldScale(new Vector3()).x * (how.scale ?? 1) : 1,
      own: how.size ? null : ownBox(root),
      centred: how.centred ?? false,
      living: how.living ?? true,
      eyeHeight: how.eyeHeight,
      shown: false,
      box: new Box3(),
      eyes: new Vector3(),
      look: new Vector3(),
    });
  };
  const named = (pattern: RegExp) => [...stage.nodes].filter(([name]) => pattern.test(name)).map(([, node]) => node);
  for (const player of playersOf(stage).values()) add(player.root, "minecraft:player", { size: SIZES.player, scale: PLAYER_SCALE, head: ctx.find(`${player.name}_head`) });
  // (Its eyes are carved a little over halfway up its block: the stem would put the game's share above them.)
  for (const pumpkin of [ctx.cast.pumpkin, ctx.cast.nether]) add(pumpkin.root, "kurbis:pumpkin", { eyeHeight: 0.62 });
  add(ctx.find("Jockey"), "minecraft:chicken", { size: SIZES.chicken, head: ctx.find("Jockey_head") });
  add(ctx.find("Jockey_Rider"), "minecraft:zombie", { size: SIZES.zombie, head: ctx.find("Jockey_Rider_head") });
  add(ctx.find("Home_Creeper"), "minecraft:creeper", { size: SIZES.creeper, head: ctx.find("Home_Creeper_head") });
  add(ctx.find("Strider"), "minecraft:strider", { size: SIZES.strider });
  add(ctx.find("Cart"), "minecraft:minecart", { size: SIZES.minecart, living: false });
  // (The film's bats are its own, much smaller than the game's: they are boxed as they are.)
  for (const bat of named(/^Cave_Bat_\d+$/)) add(bat, "minecraft:bat", { ahead: 1 });
  for (const butterfly of named(/^Butterfly_\d+$/)) add(butterfly, "kurbis:butterfly", { ahead: 1 });
  for (const fish of named(/^(Leaper|\w+_Fish)$/)) add(fish, "minecraft:salmon", { size: SIZES.salmon, centred: true });
  for (const bobber of named(/^\w+_Bobber$/)) add(bobber, "minecraft:fishing_bobber", { size: SIZES.fishing_bobber, centred: true, living: false });

  const at = new Vector3();
  const scale = new Vector3();
  /** Whether it is drawn at all: it and all it hangs from are, and something of it is. */
  const drawn = (root: Object3D) => {
    for (let object: Object3D | null = root; object; object = object.parent) if (!object.visible) return false;
    let mesh = false;
    root.traverseVisible((object) => void (mesh ||= (object as Mesh).isMesh === true));
    return mesh;
  };
  /** Where each creature is this frame, and whether it is there to be seen. */
  const place = () => {
    for (const one of creatures) {
      one.shown = drawn(one.root);
      if (!one.shown) continue;
      one.root.updateWorldMatrix(true, false);
      one.root.getWorldPosition(at);
      if (at.distanceTo(camera.position) > REACH) {
        one.shown = false;
        continue;
      }
      let eyes: number;
      if (one.size) {
        const [width, height, eye] = one.size.map((v) => v * one.scale);
        const bottom = one.centred ? at.z - height / 2 : at.z;
        one.box.min.set(at.x - width / 2, at.y - width / 2, bottom);
        one.box.max.set(at.x + width / 2, at.y + width / 2, bottom + height);
        eyes = eye;
      } else {
        // The film's own: its bounds as it is now, on a square foot as the game's boxes are.
        one.root.getWorldScale(scale);
        const own = one.own!;
        const half = (Math.max(own.max.x - own.min.x, own.max.y - own.min.y) * Math.max(scale.x, scale.y)) / 2;
        one.box.min.set(at.x - half, at.y - half, at.z + own.min.z * scale.z);
        one.box.max.set(at.x + half, at.y + half, at.z + own.max.z * scale.z);
        eyes = one.eyeHeight !== undefined ? one.eyeHeight * scale.z : EYES * (one.box.max.z - one.box.min.z);
      }
      one.eyes.set(at.x, at.y, one.box.min.z + eyes);
      const looks = one.head ?? one.root;
      looks.updateWorldMatrix(true, false);
      one.look.set(0, one.ahead, 0).transformDirection(looks.matrixWorld);
    }
  };

  // -- the hitboxes: thin lines in the world, drawn after all of it ------------------------
  // (A pixel of the page wide: a line of the GPU's own is one of the screen's, a hair on a fine screen.)
  const ends = new Float32Array(SEGMENTS * 6);
  const tones = new Float32Array(SEGMENTS * 6);
  const geometry = new LineSegmentsGeometry().setPositions(ends).setColors(tones);
  // (Seen through nothing: the world in front hides them, as it hides what they ring.)
  const material = new LineMaterial({ vertexColors: true, transparent: true, depthWrite: false, toneMapped: false });
  const lines = new LineSegments2(geometry, material);
  lines.name = "Debug_Hitboxes";
  lines.frustumCulled = false; // (it is written afresh each frame)
  lines.renderOrder = 1000; // after everything, the water too
  lines.visible = false;
  stage.scene.add(lines);
  const pixels = new Vector2();
  let count = 0;
  const segment = (a: Vector3, b: Vector3, color: readonly number[]) => {
    if (count >= SEGMENTS) return;
    const k = count * 6;
    a.toArray(ends, k);
    b.toArray(ends, k + 3);
    for (let i = 0; i < 6; i += 1) tones[k + i] = color[i % 3];
    count += 1;
  };
  const BOX = [1, 1, 1] as const;
  const EYE = [1, 0, 0] as const;
  const AHEAD = [0, 0, 1] as const;
  const a = new Vector3();
  const b = new Vector3();
  const corner = (box: Box3, i: number, z: number, out: Vector3) => out.set(i & 1 ? box.max.x : box.min.x, i & 2 ? box.max.y : box.min.y, z);
  /** A box's edges: round its foot and its top, and up its four corners. Given one height, just round it there. */
  const outline = (box: Box3, color: readonly number[], z0 = box.min.z, z1 = box.max.z) => {
    for (const z of z0 === z1 ? [z0] : [z0, z1]) for (const [i, j] of [[0, 1], [1, 3], [3, 2], [2, 0]]) segment(corner(box, i, z, a), corner(box, j, z, b), color);
    if (z0 !== z1) for (const i of [0, 1, 2, 3]) segment(corner(box, i, z0, a), corner(box, i, z1, b), color);
  };
  const middle = new Vector3();
  const target = new Box3();
  const draw = (s: number) => {
    count = 0;
    for (const one of creatures) {
      if (!one.shown) continue;
      outline(one.box, BOX);
      if (one.living) outline(one.box, EYE, one.eyes.z, one.eyes.z);
      segment(one.eyes, b.copy(one.look).multiplyScalar(LOOK).add(one.eyes), AHEAD);
    }
    // What can be clicked, where it is not one of them already (whose own it is: its middle is in their box).
    for (const thing of ctx.touch.targets) {
      if (!open(thing, s)) continue;
      middle.copy(typeof thing.at === "function" ? thing.at() : thing.at);
      if (creatures.some((one) => one.shown && one.box.containsPoint(middle))) continue;
      target.min.copy(middle).sub(thing.half);
      target.max.copy(middle).add(thing.half);
      outline(target, BOX);
    }
    geometry.instanceCount = count;
    (geometry.attributes.instanceStart as InterleavedBufferAttribute).data.needsUpdate = true;
    (geometry.attributes.instanceColorStart as InterleavedBufferAttribute).data.needsUpdate = true;
    material.resolution.copy(film.renderer.getDrawingBufferSize(pixels));
    material.linewidth = Math.max(film.renderer.getPixelRatio(), 1.5);
  };

  // -- the debug screen's lines ---------------------------------------------------------------
  const gl = film.renderer.getContext();
  let gpu: { vendor: string; renderer: string; version: string } | null = null;
  /** What the browser says the GPU is: as it says it, or unmasked where it gives only its own name. */
  const askGpu = () => {
    let vendor = String(gl.getParameter(gl.VENDOR));
    let renderer = String(gl.getParameter(gl.RENDERER));
    if (/webkit|mozilla/i.test(renderer)) {
      const info = gl.getExtension("WEBGL_debug_renderer_info");
      if (info) {
        vendor = String(gl.getParameter(info.UNMASKED_VENDOR_WEBGL));
        renderer = String(gl.getParameter(info.UNMASKED_RENDERER_WEBGL));
      }
    }
    return { vendor, renderer, version: String(gl.getParameter(gl.VERSION)) };
  };
  const ray = new Ray();
  const hit = new Vector3();
  /** The creature under the pointer, if any: the nearest whose box the pointer's line meets. */
  const pointedAt = (): Creature | null => {
    const pointer = ctx.touch.pointer;
    if (!pointer) return null;
    ray.origin.setFromMatrixPosition(camera.matrixWorld);
    ray.direction.set(pointer.x * 2 - 1, 1 - pointer.y * 2, 0.5).unproject(camera).sub(ray.origin).normalize();
    let best: Creature | null = null;
    let nearest = Infinity;
    for (const one of creatures) {
      if (!one.shown || !ray.intersectBox(one.box, hit)) continue;
      const far = hit.distanceTo(ray.origin);
      if (far < nearest) {
        nearest = far;
        best = one;
      }
    }
    return best;
  };
  const forward = new Vector3();
  const megabytes = (bytes: number) => String(Math.floor(bytes / 1024 / 1024)).padStart(3, "0");
  /** The left-hand column, the right-hand one, and what of the right a narrow frame shows under the left. */
  const columns = (s: number, short: boolean): [Words[], Words[], Words[]] => {
    camera.getWorldDirection(forward);
    const left: Words[] = [
      fill(SAY.film, { version: BUNDLE }),
      fill(SAY.fps, { fps }),
      s >= C.THROUGH && s < C.HOME ? NETHER : OVERWORLD,
      "",
      ...whereLines(camera.position, forward, short),
      fill(SAY.biome, { biome: biomeAt(s, C) }),
      "",
      fill(SAY.scroll, { at: s.toFixed(2), end: film.end.toFixed(2) }),
    ];
    gpu ??= askGpu();
    const right: Words[] = [];
    // (How much memory the page's scripts take: only some browsers say.)
    const memory = (performance as Performance & { memory?: { usedJSHeapSize: number; totalJSHeapSize: number; jsHeapSizeLimit: number } }).memory;
    if (memory?.jsHeapSizeLimit) {
      const { usedJSHeapSize: used, totalJSHeapSize: size, jsHeapSizeLimit: max } = memory;
      right.push(fill(SAY.mem, { share: Math.floor((used * 100) / max), used: megabytes(used), max: megabytes(max) }));
      right.push(fill(SAY.allocated, { share: Math.floor((size * 100) / max), size: megabytes(size) }));
    }
    if (navigator.hardwareConcurrency) right.push(fill(SAY.cpu, { count: navigator.hardwareConcurrency }));
    right.push("", fill(SAY.display, { width: gl.drawingBufferWidth, height: gl.drawingBufferHeight, vendor: gpu.vendor }), gpu.renderer, gpu.version);
    right.push(fill(SAY.ratio, { ratio: Math.round((window.devicePixelRatio || 1) * 100) / 100 }));
    const one = pointedAt();
    const more: Words[] = [...(one ? ["", [{ text: SAY.entity, underline: true }], one.id] : []), "", SAY.server];
    return [left, [...right, ...more], more];
  };

  let screen = false;
  let hitboxes = false;
  // The frames, counted over each second of real time (the film's own clock is held back on a slow frame).
  let frames = 0;
  let counted = performance.now();
  let fps = 0;
  let written = -Infinity;
  let room = 0;
  let short = false;
  tl.always(({ s }) => {
    frames += 1;
    const now = performance.now();
    if (now - counted >= 1000) {
      fps = Math.round((frames * 1000) / (now - counted));
      frames = 0;
      counted = now;
    }
    const { ui } = ctx;
    lines.visible = hitboxes;
    if (!screen && ui) ui.debug.left = ui.debug.right = null;
    if (!screen && !hitboxes) return;
    place();
    if (hitboxes) draw(s);
    // The lines, a few times a second (and at once, for a frame of another width).
    if (!screen || !ui || (now - written < EVERY * 1000 && room === ui.hud.width)) return;
    const width = (line: Words) => ui.draw.widthOf(line);
    if (room !== ui.hud.width) {
      // A frame too narrow for the facing line at its widest has it short, whatever it says now
      // (or it would come and go as the numbers do).
      const widest = Math.max(...Object.values(SAY.towards).map(([direction, towards]) => width(fill(SAY.facing, { direction, towards, yaw: "-180.0", pitch: "-90.0" }))));
      short = ctx.tall && widest + 5 > ui.hud.width;
    }
    written = now;
    room = ui.hud.width;
    const [left, right, more] = columns(s, short);
    const [one, two] = arrange(left, right, more, room, width, ctx.tall);
    ui.debug.left = ui.draw.debug(one);
    ui.debug.right = two.length ? ui.draw.debug(two, true) : null;
  });

  return {
    get screen() {
      return screen;
    },
    get hitboxes() {
      return hitboxes;
    },
    toggleScreen() {
      screen = !screen;
      written = -Infinity;
    },
    toggleHitboxes(time) {
      hitboxes = !hitboxes;
      const said = hitboxes ? COPY.keys.hitboxes.shown : COPY.keys.hitboxes.hidden;
      ctx.ui?.say([{ text: COPY.keys.prefix, color: YELLOW, bold: true }, { text: ` ${said}` }], WHITE, time, {});
    },
  };
}
