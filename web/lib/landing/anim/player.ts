/**
 * A player, alive: the art's rig (its pivots, and what its hands hold) dressed in limbs
 * that bend as one piece, a face that acts, and motion that has weight.
 *
 * What a scene asks for is a pose: angles per joint, which it may write fresh each frame
 * (`target`). The body gets there through springs, so every move overshoots a little and
 * settles, the way the trailers' characters move; and on top of whatever it is asked it
 * breathes, shifts its weight, blinks, and glances about. A scene can point its head and
 * eyes at something (`look`), and set what its face says (`face`).
 *
 * Angles are degrees about X, Y and Z, applied in the art's order (Blender's XYZ, three's
 * "ZYX"). A bend is how far the elbow (knee) is bent: forward for an arm, back for a leg.
 */

import { AdditiveBlending, CanvasTexture, Euler, Matrix4, Mesh, MeshBasicMaterial, NearestFilter, Object3D, PlaneGeometry, SRGBColorSpace, Vector3, type Material } from "three";
import type { MaterialNote } from "../engine/bundle";
import { modelMaterial } from "../engine/models";
import type { Stage } from "../engine/stage";
import { Limb, type LimbBox } from "./limb";

const RAD = Math.PI / 180;
const DEG = 180 / Math.PI;
/** A pixel of the art, in the world's units (a block is sixteen). */
const PX = 1 / 16;
/** How tall a player stands, to the top of the hat layer. */
const HEIGHT = 32.5 * PX;
/**
 * Blank eyes (see `Player.blank`): their white, and how bright their lights are at full
 * glow (linear light, well past where the bloom begins: engine/post.ts). The lights stand
 * this far off the face, under the hat layer (half a pixel out), which hides them where it
 * covers the eyes.
 */
const BLANK = "#ffffff";
const GLARE = 12;
const OFF_FACE = 0.3 * PX;

export type Joint = "body" | "head" | "armR" | "armL" | "legR" | "legL";
export type Bend = "elbowR" | "elbowL" | "kneeR" | "kneeL";
export const JOINTS: readonly Joint[] = ["body", "head", "armR", "armL", "legR", "legL"];
export const BENDS: readonly Bend[] = ["elbowR", "elbowL", "kneeR", "kneeL"];

/** A whole pose: three angles per joint, a bend per elbow and knee, and how far the body is lifted off its feet (blocks). */
export interface Pose {
  turn: Record<Joint, [number, number, number]>;
  bend: Record<Bend, number>;
  lift: number;
  /** Turn of the whole body about the vertical, on top of where the art stood it (degrees). */
  yaw: number;
}

export type Mouth = "closed" | "open" | "smile";
/** A pumpkin on its head: carved, or lit. */
export type Pumpkin = "carved" | "lit";
export interface Face {
  /** Where the eyes look: -1 to its right (the viewer's left), 0 ahead, 1 to its left. */
  eyes: number;
  /** Eyes wide (surprise). */
  wide: boolean;
  mouth: Mouth;
}

export function clonePose(pose: Pose): Pose {
  return {
    turn: Object.fromEntries(JOINTS.map((j) => [j, [...pose.turn[j]]])) as Pose["turn"],
    bend: { ...pose.bend },
    lift: pose.lift,
    yaw: pose.yaw,
  };
}

/** Add `delta` (degrees, blocks) to `pose`, scaled by `k`. */
export function addPose(pose: Pose, delta: Partial<Delta>, k = 1): Pose {
  for (const j of JOINTS) {
    const d = delta[j];
    if (d) for (let i = 0; i < 3; i += 1) pose.turn[j][i] += (d[i] ?? 0) * k;
  }
  for (const b of BENDS) if (delta[b] !== undefined) pose.bend[b] += delta[b]! * k;
  if (delta.lift !== undefined) pose.lift += delta.lift * k;
  if (delta.yaw !== undefined) pose.yaw += delta.yaw * k;
  return pose;
}

/** A change to a pose, as a scene writes it: only what moves. */
export type Delta = Partial<Record<Joint, readonly number[]>> & Partial<Record<Bend, number>> & { lift?: number; yaw?: number };

/** A spring for one number: it follows its target with a little overshoot, as a limb with weight does. */
class Spring {
  v = 0;
  constructor(
    public x: number,
    private readonly omega: number,
    private readonly zeta: number,
  ) {}

  step(target: number, dt: number) {
    // Small steps: stable however long the frame was.
    let left = dt;
    while (left > 0) {
      const h = Math.min(left, 1 / 240);
      this.v += (this.omega * this.omega * (target - this.x) - 2 * this.zeta * this.omega * this.v) * h;
      this.x += this.v * h;
      left -= h;
    }
  }
}

/** How lively each part is: stiffness (cycles a second) and damping. Arms lag and swing past; the body and legs are steadier. */
const FEEL: Record<Joint | Bend | "lift" | "yaw", [number, number]> = {
  body: [3.2, 0.62],
  head: [3.6, 0.55],
  armR: [3.0, 0.5],
  armL: [3.0, 0.5],
  legR: [4.2, 0.7],
  legL: [4.2, 0.7],
  elbowR: [3.4, 0.5],
  elbowL: [3.4, 0.5],
  kneeR: [4.6, 0.7],
  kneeL: [4.6, 0.7],
  lift: [5.0, 0.6],
  yaw: [2.6, 0.75],
};

interface Eye {
  white: [number, number];
  pupil: [number, number];
}

/** The face, from the skin: where the eyes and mouth are, and its colours (rows of the face counted from the skin's top). */
interface FaceMap {
  eyes: [Eye, Eye];
  row: number;
  mouth: [number, number, number];
  skin: string;
  lip: string;
}

const SKIN = 64;

export class Player {
  readonly root: Object3D;
  readonly base: Pose;
  /** Where the scene wants the body this frame; reset to `base` by `rest()`. */
  target: Pose;
  /** What the face says. */
  face: Face = { eyes: 0, wide: false, mouth: "closed" };
  /** A point to look at (world), or none: then it glances about. */
  look: Vector3 | null = null;
  /** How alive it is when nobody asks anything of it: 1 is fully, 0 is a statue. */
  life = 1;
  /**
   * Eyes with nothing in them, as Herobrine's: white from side to side, without pupils,
   * and they never blink or go wide, whatever `face` says (the mouth still does as it is
   * told). They shine as well: a small light over each eye, which the bloom catches, so
   * that two white points read from far off, in haze and in the dark. The lights are on
   * the head, so they turn and nod with it, and it hides them from behind. Another skin
   * (`wear`) has them too, over its own eyes.
   */
  blank = false;
  /** How brightly blank eyes shine, 0 to 1: a scene may dim them, or put out every other light and leave them. */
  glow = 1;
  /**
   * What it wears on its head, as the game's mobs do on Halloween (film/halloween.ts): a
   * carved pumpkin, a lit one for the dark (a jack o'lantern, its face shining), or nothing.
   * The art made both on its head, a little larger than it, so they turn and nod with it.
   */
  pumpkin: Pumpkin | null = null;
  private readonly pumpkins: Record<Pumpkin, Object3D | null>;
  private readonly springs: Record<string, Spring[]>;
  private readonly parts: Record<Joint, Object3D>;
  private readonly lower: Record<Bend, Object3D>;
  private readonly limbs: Record<Bend, Limb>;
  private readonly canvas: HTMLCanvasElement | null;
  private readonly paint: CanvasRenderingContext2D | null;
  private readonly texture: CanvasTexture | null;
  private map: FaceMap | null;
  private clean: ImageData | null;
  /** The skin the art gave it, for when it is itself again (see `wear`). */
  private readonly own: ImageBitmap | null = null;
  private painted = "";
  /** The lights over blank eyes, made the first time they are wanted, and the face they were laid on. */
  private eyeLights: { look: MeshBasicMaterial; lights: Mesh[]; laid: FaceMap | null } | null = null;
  private blinkAt: number;
  private glance = { at: 0, yaw: 0, pitch: 0 };
  private readonly phase: number;
  private readonly scratch = new Vector3();
  private readonly inverse = new Matrix4();
  private readonly euler = new Euler();
  /** Between the root and the body: what stands it on its head (see `upsideDown`). */
  private readonly over: Object3D;

  /**
   * `name` is the art's rig (its parts are `${name}_body` and so on), `slim` whether its
   * arms are three pixels wide.
   */
  constructor(
    stage: Stage,
    readonly name: string,
    slim: boolean,
    seed = 0,
  ) {
    const node = (part: string) => {
      const found = stage.nodes.get(`${name}_${part}`);
      if (!found) throw new Error(`landing: the rig ${name} has no ${part}`);
      return found;
    };
    this.root = stage.nodes.get(name) ?? node("body").parent!;
    this.over = new Object3D();
    this.over.name = `${name}_over`;
    for (const part of [...this.root.children]) this.over.add(part);
    this.root.add(this.over);
    this.parts = { body: node("body"), head: node("head"), armR: node("arm_r"), armL: node("arm_l"), legR: node("leg_r"), legL: node("leg_l") };
    this.lower = { elbowR: node("hand_r"), elbowL: node("hand_l"), kneeR: node("foot_r"), kneeL: node("foot_l") };
    this.phase = seed * 1.618;
    playersOf(stage).set(name, this);
    this.pumpkins = { carved: stage.nodes.get(`${name}_pumpkin_carved`) ?? null, lit: stage.nodes.get(`${name}_pumpkin_lit`) ?? null };
    this.wearPumpkin(); // (none, until it is asked for one)
    this.blinkAt = 1 + ((seed * 0.37) % 2);

    // The pose the art stood it in is where it rests.
    const turnOf = (thing: Object3D): [number, number, number] => {
      this.euler.setFromQuaternion(thing.quaternion, "ZYX");
      return [this.euler.x * DEG, this.euler.y * DEG, this.euler.z * DEG];
    };
    this.base = {
      turn: Object.fromEntries(JOINTS.map((j) => [j, turnOf(this.parts[j])])) as Pose["turn"],
      // The art bends the lower part about X; forward (negative) is an elbow's bend, back a knee's.
      bend: { elbowR: -turnOf(this.lower.elbowR)[0], elbowL: -turnOf(this.lower.elbowL)[0], kneeR: turnOf(this.lower.kneeR)[0], kneeL: turnOf(this.lower.kneeL)[0] },
      lift: 0,
      yaw: 0,
    };
    this.target = clonePose(this.base);
    this.springs = {};
    for (const j of JOINTS) this.springs[j] = this.base.turn[j].map((v) => new Spring(v, 2 * Math.PI * FEEL[j][0], FEEL[j][1]));
    for (const b of BENDS) this.springs[b] = [new Spring(this.base.bend[b], 2 * Math.PI * FEEL[b][0], FEEL[b][1])];
    this.springs.lift = [new Spring(0, 2 * Math.PI * FEEL.lift[0], FEEL.lift[1])];
    this.springs.yaw = [new Spring(0, 2 * Math.PI * FEEL.yaw[0], FEEL.yaw[1])];
    this.rootYaw = this.root.rotation.z;
    this.rootAt = this.root.position.clone();

    // Its own copy of its skin, so the face can act; every part wears it.
    const mesh = this.parts.body as Mesh;
    const noteName = (mesh.material as Material & { name: string }).name;
    const note = stage.data.materials[noteName] as MaterialNote | undefined;
    const picture = note?.image ? stage.pictures.named.get(note.image) : undefined;
    const bitmap = picture?.image as ImageBitmap | undefined;
    let material: Material = mesh.material as Material;
    this.canvas = null;
    this.paint = null;
    this.texture = null;
    this.map = null;
    this.clean = null;
    if (bitmap && note && typeof document !== "undefined") {
      const canvas = document.createElement("canvas");
      canvas.width = SKIN;
      canvas.height = SKIN;
      const paint = canvas.getContext("2d", { willReadFrequently: true });
      if (paint) {
        paint.drawImage(bitmap, 0, 0); // (as loaded: its rows bottom first, which is how the pictures are uploaded)
        this.own = bitmap;
        const texture = new CanvasTexture(canvas);
        texture.flipY = false;
        texture.colorSpace = SRGBColorSpace;
        texture.magFilter = NearestFilter;
        texture.minFilter = NearestFilter;
        texture.generateMipmaps = false;
        const key = `@player:${name}`;
        stage.pictures.named.set(key, texture);
        material = modelMaterial(stage.globals, { ...note, image: key }, stage.pictures);
        this.canvas = canvas;
        this.paint = paint;
        this.texture = texture;
        this.clean = paint.getImageData(0, 0, SKIN, SKIN);
        this.map = findFace(this.clean);
      }
    }
    for (const part of [this.parts.body, this.parts.head]) (part as Mesh).material = material;

    // The limbs: the art's two boxes per limb are hidden (what they hold stays with them),
    // and one bendable piece hangs from the upper one instead.
    const aw = slim ? 3 : 4;
    const limb = (lo: [number, number, number], size: [number, number, number], uv: [number, number], overlay: [number, number], pivot: [number, number, number], joint: number, reach: number) => {
      const rel = (p: readonly number[]): [number, number, number] => [p[0] - pivot[0], p[1] - pivot[1], p[2] - pivot[2]];
      const boxes: LimbBox[] = [
        { lo: rel(lo), size, uv, inflate: 0 },
        { lo: rel(lo), size, uv: overlay, inflate: 0.5 },
      ];
      return new Limb(boxes, joint, reach, PX);
    };
    this.limbs = {
      elbowR: limb([-4 - aw, -2, 12], [aw, 4, 12], [40, 16], [40, 32], [-4 - aw / 2, 0, 22], 4, 2.4),
      elbowL: limb([4, -2, 12], [aw, 4, 12], [32, 48], [48, 48], [4 + aw / 2, 0, 22], 4, 2.4),
      kneeR: limb([-4, -2, 0], [4, 4, 12], [0, 16], [0, 32], [-2, 0, 12], 6, 2.6),
      kneeL: limb([0, -2, 0], [4, 4, 12], [16, 48], [0, 48], [2, 0, 12], 6, 2.6),
    };
    const upper: Record<Bend, Object3D> = { elbowR: this.parts.armR, elbowL: this.parts.armL, kneeR: this.parts.legR, kneeL: this.parts.legL };
    for (const b of BENDS) {
      for (const old of [upper[b], this.lower[b]]) (old as Mesh).geometry?.setDrawRange(0, 0);
      const piece = new Mesh(this.limbs[b].geometry, material);
      piece.name = `${name}_${b}_bendable`;
      piece.frustumCulled = false;
      upper[b].add(piece);
    }
  }

  private rootYaw: number;
  /** How much it is following what it looks at (eases in and out). */
  private follow = 0;
  private readonly rootAt: Vector3;

  /** Start this frame's pose from the resting one (scenes then add what they want). */
  rest(): Pose {
    this.target = clonePose(this.base);
    return this.target;
  }

  /** Move to `at` (world), facing along `yaw` (radians, as the art's Z turn); null keeps the art's place. */
  place(at: Vector3 | null, yaw: number | null = null) {
    if (at) this.rootAt.copy(at);
    if (yaw !== null) this.rootYaw = yaw;
  }

  /** Make it jump to its target at once (no springs): after it was hidden, or moved far. */
  settle() {
    for (const j of JOINTS) this.springs[j].forEach((s, i) => ((s.x = this.target.turn[j][i]), (s.v = 0)));
    for (const b of BENDS) ((this.springs[b][0].x = this.target.bend[b]), (this.springs[b][0].v = 0));
    this.springs.lift[0].x = this.target.lift;
    this.springs.yaw[0].x = this.target.yaw;
  }

  update(time: number, dt: number) {
    const t = this.target;
    const life = this.life;
    const p = this.phase;
    // Breath: the chest rises, the shoulders with it, the head stays where it looks.
    const breath = Math.sin(time * 1.75 + p);
    // Weight shifts from foot to foot, slowly.
    const shift = Math.sin(time * 0.83 + p * 2.1);
    // (About X a part pitches, about Y it leans to the side, about Z it turns.)
    const live: Delta = {
      body: [1.1 * breath, 1.6 * shift, 0],
      head: [-0.9 * breath, -1.2 * shift, 0],
      armR: [0.6 * breath, 1.0 * breath - 1.2 * shift, 0],
      armL: [0.6 * breath, -1.0 * breath - 1.2 * shift, 0],
      legR: [0, -1.6 * shift, 0],
      legL: [0, -1.6 * shift, 0],
      elbowR: 2 + 2 * breath,
      elbowL: 2 + 2 * breath,
    };
    const goal = addPose(clonePose(t), live, life);

    // The head: toward what it looks at, through the body; else it glances about now and then.
    let eyes = this.face.eyes;
    // Only what is in front can be followed: a thing behind is let go of, as a person would.
    let following = 0;
    if (this.look) {
      this.parts.body.updateWorldMatrix(true, false);
      this.inverse.copy(this.parts.body.matrixWorld).invert();
      const local = this.scratch.copy(this.look).applyMatrix4(this.inverse);
      const yaw = Math.abs(Math.atan2(local.x, -local.y) * DEG);
      following = 1 - Math.min(Math.max((yaw - 85) / 25, 0), 1);
    }
    this.follow += (following - this.follow) * Math.min(dt * 6, 1);
    if (this.look && this.follow > 0.01) {
      this.parts.body.updateWorldMatrix(true, false);
      this.inverse.copy(this.parts.body.matrixWorld).invert();
      const local = this.scratch.copy(this.look).applyMatrix4(this.inverse);
      // From the neck: up the body 12 pixels. The face looks along -Y.
      local.z -= 12 * PX;
      const yaw = Math.atan2(local.x, -local.y) * DEG;
      const pitch = Math.atan2(local.z, Math.hypot(local.x, local.y)) * DEG;
      const f = this.follow;
      const turn = Math.max(-55, Math.min(55, yaw));
      goal.turn.head[2] += (turn - goal.turn.head[2]) * f;
      // A head pitches down about +X. The neck only goes so far (further, and the head sinks
      // into the shoulders); for the rest the body leans, as people do.
      const neck = Math.max(-20, Math.min(22, pitch));
      goal.turn.head[0] += (-neck - goal.turn.head[0]) * f;
      goal.turn.body[0] -= (pitch - neck) * 0.45 * f;
      // Far round, the body comes along a little (never so far it turns its back).
      if (Math.abs(yaw) > 45) goal.turn.body[2] += Math.max(-18, Math.min(18, (yaw - 45 * Math.sign(yaw)) * 0.35)) * f;
      // The eyes go first, and further than the head.
      if (f > 0.5) eyes = Math.abs(yaw - turn) > 6 || Math.abs(yaw) > 12 ? Math.sign(yaw) : eyes;
    } else if (life > 0) {
      if (time > this.glance.at) {
        const r = Math.abs(Math.sin(time * 12.9898 + p) * 43758.5453) % 1;
        this.glance = { at: time + 1.6 + 3.2 * r, yaw: (r - 0.5) * 22, pitch: (((r * 7.3) % 1) - 0.5) * 8 };
      }
      goal.turn.head[2] += this.glance.yaw * life;
      goal.turn.head[0] += this.glance.pitch * life;
      if (Math.abs(this.glance.yaw) > 7 && eyes === 0) eyes = Math.sign(this.glance.yaw);
    }

    // Springs.
    for (const j of JOINTS) this.springs[j].forEach((s, i) => s.step(goal.turn[j][i], dt));
    for (const b of BENDS) this.springs[b][0].step(goal.bend[b], dt);
    this.springs.lift[0].step(goal.lift, dt);
    this.springs.yaw[0].step(goal.yaw, dt);

    // Onto the rig.
    for (const j of JOINTS) {
      const [x, y, z] = this.springs[j].map((s) => s.x * RAD);
      this.parts[j].rotation.set(x, y, z, "ZYX");
    }
    for (const b of BENDS) {
      // An elbow bends forward, a knee back; neither past straight by more than a little.
      const knee = b.startsWith("knee");
      const amount = Math.max(-4, this.springs[b][0].x) * RAD;
      const angle = knee ? -amount : amount;
      const limb = this.limbs[b];
      limb.bend(angle);
      const frame = limb.lower(angle);
      const holder = this.lower[b];
      holder.position.set(holder.position.x, frame.y, frame.z);
      holder.rotation.set(frame.turn, 0, 0, "ZYX");
    }
    this.root.position.set(this.rootAt.x, this.rootAt.y, this.rootAt.z + this.springs.lift[0].x);
    this.root.rotation.z = this.rootYaw + this.springs.yaw[0].x * RAD;

    // The face.
    let blink = false;
    if (time > this.blinkAt) {
      blink = time < this.blinkAt + 0.11;
      if (time > this.blinkAt + 0.11) {
        const r = Math.abs(Math.sin(time * 78.233 + p) * 43758.5453) % 1;
        // Now and then twice in a row, as people do.
        this.blinkAt = time + (r < 0.18 ? 0.22 : 2.0 + 3.5 * r);
      }
    }
    this.draw(eyes, blink && !this.face.wide, this.face.wide, this.face.mouth);
    this.shine();
    this.wearPumpkin();
  }

  private wearPumpkin() {
    for (const face of ["carved", "lit"] as const) {
      const worn = this.pumpkins[face];
      if (worn) worn.visible = this.pumpkin === face;
    }
  }

  /** Paint the face as it is now (only when it changed). */
  private draw(eyes: number, shut: boolean, wide: boolean, mouth: Mouth) {
    const blank = this.blank;
    const key = blank ? `blank|${mouth}` : `${eyes}|${shut}|${wide}|${mouth}`;
    if (key === this.painted || !this.paint || !this.clean || !this.map || !this.texture) return;
    this.painted = key;
    const paint = this.paint;
    paint.putImageData(this.clean, 0, 0);
    const { eyes: pair, row, mouth: lips, skin, lip } = this.map;
    // The canvas has the skin's rows bottom first: row y of the skin is row 63 - y here.
    const dot = (x: number, y: number, color: string) => {
      paint.fillStyle = color;
      paint.fillRect(x, SKIN - 1 - y, 1, 1);
    };
    const [left, right] = pair;
    const white = "#f6f6f6";
    const pupilColor = this.pixel(left.pupil[0], left.pupil[1]);
    if (blank) {
      for (const eye of pair) for (const [x] of [eye.white, eye.pupil]) dot(x, row, BLANK);
    } else if (shut) {
      for (const eye of pair) for (const [x] of [eye.white, eye.pupil]) dot(x, row, skin);
    } else {
      // Pupils toward where it looks: both eyes the same way.
      const toward = (eye: Eye, outer: boolean) => {
        // An eye is [white, pupil] from its outer side in; looking out swaps them.
        const swap = eyes !== 0 && ((eyes < 0) === (eye.white[0] < eye.pupil[0])) === outer;
        dot(eye.white[0], row, swap ? pupilColor : white);
        dot(eye.pupil[0], row, swap ? white : pupilColor);
      };
      toward(left, true);
      toward(right, true);
      if (wide) for (const eye of pair) for (const [x] of [eye.white, eye.pupil]) dot(x, row - 1, white);
    }
    const [mx, my, width] = lips;
    if (mouth === "open") {
      for (let x = mx; x < mx + width; x += 1) {
        dot(x, my, "#5a2121");
        dot(x, my + 1, "#7a2b2b");
      }
    } else if (mouth === "smile") {
      for (let x = mx; x < mx + width; x += 1) dot(x, my + 1, lip);
      dot(mx - 1, my, lip);
      dot(mx + width, my, lip);
      for (let x = mx; x < mx + width; x += 1) dot(x, my, skin);
    }
    this.texture.needsUpdate = true;
  }

  private pixel(x: number, y: number): string {
    const data = this.clean!.data;
    const i = ((SKIN - 1 - y) * SKIN + x) * 4;
    return `rgb(${data[i]}, ${data[i + 1]}, ${data[i + 2]})`;
  }

  /** The lights over blank eyes: on while they are blank, over the eyes of the skin it wears. */
  private shine() {
    const on = this.blank && this.glow > 0 && this.map !== null;
    if (!on && !this.eyeLights) return;
    if (!this.eyeLights) {
      const look = new MeshBasicMaterial({ color: 0xffffff, transparent: true, blending: AdditiveBlending, depthWrite: false });
      const shape = new PlaneGeometry(2 * PX, PX).rotateX(Math.PI / 2); // (facing out of the face, along -Y; seen from in front only)
      const lights = [0, 1].map((k) => {
        const light = new Mesh(shape, look);
        light.name = `${this.name}_eye_${k}`;
        this.parts.head.add(light);
        return light;
      });
      this.eyeLights = { look, lights, laid: null };
    }
    const { look, lights } = this.eyeLights;
    for (const light of lights) light.visible = on;
    if (!on) return;
    look.color.setScalar(GLARE * Math.min(this.glow, 1));
    const map = this.map!;
    if (this.eyeLights.laid !== map) {
      this.eyeLights.laid = map;
      // Pixel (x, y) of the face (x 8..15, y 8..15 from the skin's top) is the square of the
      // head's front from x - 12 to x - 11 pixels across from its middle (toward its left,
      // which is the viewer's right) and from 15 - y to 16 - y up from the neck.
      map.eyes.forEach((eye, k) => {
        const across = Math.min(eye.white[0], eye.pupil[0]) + 1 - 12;
        lights[k].position.set(across * PX, -4 * PX - OFF_FACE, (15.5 - map.row) * PX);
      });
    }
  }

  /**
   * Wear another skin (a picture loaded as the bundle's are: rows bottom first), or its
   * own again (null). Its face is found afresh, so it still blinks and talks.
   */
  wear(picture: ImageBitmap | null) {
    const skin = picture ?? this.own;
    if (!skin || !this.paint || !this.texture) return;
    this.paint.clearRect(0, 0, SKIN, SKIN);
    this.paint.drawImage(skin, 0, 0);
    this.clean = this.paint.getImageData(0, 0, SKIN, SKIN);
    this.map = findFace(this.clean);
    this.painted = "";
    this.texture.needsUpdate = true;
  }

  /**
   * On its head, as the game draws anything called Dinnerbone or Grumm (film/eggs.ts): turned
   * half round about the way it faces, and lifted by its height, so it stands on its head
   * where it stood. (Whoever moves the root this frame moves it as ever: this is below that.)
   */
  upsideDown(on: boolean) {
    this.over.rotation.set(0, on ? Math.PI : 0, 0); // (it faces along -Y)
    this.over.position.set(0, 0, on ? HEIGHT : 0);
  }

  dispose() {
    for (const b of BENDS) this.limbs[b].geometry.dispose();
    this.texture?.dispose();
    this.eyeLights?.lights[0].geometry.dispose();
    this.eyeLights?.look.dispose();
  }
}

/** Each stage's players, by the rig they move: whoever must reach one (to dress it, say) finds it here. */
const PLAYERS = new WeakMap<Stage, Map<string, Player>>();
export function playersOf(stage: Stage): Map<string, Player> {
  let players = PLAYERS.get(stage);
  if (!players) PLAYERS.set(stage, (players = new Map()));
  return players;
}

/** Find the eyes on a skin's face: on one row, a white pixel at each side with the pupil next to it, inward. */
function findFace(skin: ImageData): FaceMap | null {
  const at = (x: number, y: number) => {
    const i = ((SKIN - 1 - y) * SKIN + x) * 4;
    return [skin.data[i], skin.data[i + 1], skin.data[i + 2]];
  };
  const hex = (c: number[]) => `rgb(${c[0]}, ${c[1]}, ${c[2]})`;
  const whiteish = (c: number[]) => Math.min(...c) > 205 && Math.max(...c) - Math.min(...c) < 30;
  for (let y = 10; y <= 13; y += 1) {
    for (let x = 8; x <= 10; x += 1) {
      const mirror = 23 - x; // the face is x 8..15
      if (!whiteish(at(x, y)) || !whiteish(at(mirror, y))) continue;
      const eyes: [Eye, Eye] = [
        { white: [x, y], pupil: [x + 1, y] },
        { white: [mirror, y], pupil: [mirror - 1, y] },
      ];
      const skinColor = hex(at(x, y + 1));
      // The mouth: two rows under the eyes, the middle two pixels; its colour if it has one.
      const my = y + 2;
      const lipColor = at(11, my);
      const plain = at(x, y + 1);
      const hasLip = Math.abs(lipColor[0] - plain[0]) + Math.abs(lipColor[1] - plain[1]) + Math.abs(lipColor[2] - plain[2]) > 40;
      return { eyes, row: y, mouth: [11, my, 2], skin: skinColor, lip: hasLip ? hex(lipColor) : "#b85a4e" };
    }
  }
  return null;
}


/**
 * An action as keys: at each moment (seconds), what changes (a Delta). Between keys it
 * eases; a joint a key leaves out is back at rest there. `u` is the moment in the action.
 */
export function keyed(keys: readonly (readonly [number, Delta])[], u: number): Delta {
  if (u <= keys[0][0]) return keys[0][1];
  let i = 0;
  while (i < keys.length - 1 && keys[i + 1][0] <= u) i += 1;
  if (i >= keys.length - 1) return keys[keys.length - 1][1];
  const [t0, a] = keys[i];
  const [t1, b] = keys[i + 1];
  const x = Math.min(Math.max((u - t0) / (t1 - t0), 0), 1);
  const k = x * x * (3 - 2 * x);
  const out: Delta = {};
  const mix = (p = 0, q = 0) => p + (q - p) * k;
  for (const j of JOINTS) {
    const p = a[j];
    const q = b[j];
    if (p || q) (out as Record<string, number[]>)[j] = [0, 1, 2].map((c) => mix(p?.[c], q?.[c]));
  }
  for (const key of [...BENDS, "lift", "yaw"] as const) {
    const p = a[key];
    const q = b[key];
    if (p !== undefined || q !== undefined) (out as Record<string, number>)[key] = mix(p, q);
  }
  return out;
}

/**
 * A walk, at `phase` (one stride is 0..1) and `pace` (0 standing, 1 a stroll, 2 a run):
 * legs swing from the hip and fold at the knee as they come through, the arms swing
 * against them with soft elbows, the body bobs twice a stride and turns with the hips.
 */
export function walk(phase: number, pace: number): Delta {
  const a = phase * 2 * Math.PI;
  const swing = 32 * pace;
  const s = Math.sin(a);
  // A leg folds as it comes forward through the air (the half of the stride it is behind and rising).
  const fold = (x: number) => Math.max(0, Math.sin(x)) ** 1.5;
  return {
    legR: [-swing * s, 0, 0],
    legL: [swing * s, 0, 0],
    kneeR: 55 * pace * fold(a + Math.PI * 0.15),
    kneeL: 55 * pace * fold(a + Math.PI * 1.15),
    armR: [swing * 0.75 * s, 0, 0],
    armL: [-swing * 0.75 * s, 0, 0],
    elbowR: 10 * pace + 14 * pace * Math.max(0, s),
    elbowL: 10 * pace + 14 * pace * Math.max(0, -s),
    body: [4 * pace, 0, 5 * pace * s],
    head: [-3 * pace, 0, -4 * pace * s],
    lift: 0.045 * pace * Math.abs(Math.cos(a)),
  };
}
