/**
 * The scene as the art pipeline writes it (web/art/landing/scripts/export_web.py):
 * JSON for what things are and where, a glb for everything small, and the world as
 * grids of blocks, which the page builds its faces from (./world.ts).
 */

import { DataArrayTexture, NearestFilter, NearestMipmapLinearFilter, SRGBColorSpace, Texture, type Group } from "three";
import { GLTFLoader } from "three/addons/loaders/GLTFLoader.js";
import type { ShotData } from "../anim/shot";
import type { TrackData } from "../anim/track";
import type { Records } from "../world/build";
import { BASE, bundleUrl } from "./address";
import type { Pack } from "./pack";

export { BASE, bundleUrl };

export type Aspect = "wide" | "tall";
export type Rgb = [number, number, number];
export type Vec3 = [number, number, number];

export interface MaterialNote {
  kind: "voxel" | "flat" | "tex" | "fading" | "scrim" | "crack";
  emit?: number;
  glow?: number;
  rgb?: Rgb;
  alpha?: number;
  emit_rgb?: Rgb;
  dither?: boolean;
  image?: string;
  made?: string;
  clip?: boolean;
  tint?: Rgb | null;
  emit_image?: string | null;
  strength?: number;
  stages?: number;
}

export interface SkyNote {
  sun: Vec3;
  stops: [number, Rgb][];
  glow: Rgb;
  glow_power: number;
  glow_gain: number;
  disc: number;
  disc_power: number;
  stars: number;
  strength: number;
}

export interface Palette {
  front: Rgb;
  top: Rgb;
  side: Rgb;
  bottom: Rgb;
}

export interface Face {
  rows: number;
  glyphs: Record<string, string[]>;
}

export interface SceneData {
  sky: SkyNote;
  atlas: { tiles: number; across: number; tile: number };
  palettes: Record<string, Palette>;
  glyphs: { block: Face; pixel: Face };
  constants: Constants;
  spots: Spots;
  nether: NetherSpots;
  valley_floor: number;
  materials: Record<string, MaterialNote>;
  textures: Record<string, string>;
  /** Pictures the interface shows, by what they are for. */
  hud: Record<string, string>;
}

/** The film's own numbers, read from the art rather than repeated here. */
export interface Constants {
  /** Where each part of the journey begins, in screens of scroll. */
  PART: Record<"sky" | "dive" | "surface" | "slice" | "dig" | "arrival" | "cave" | "cart" | "dungeon" | "portal" | "nether" | "end", number>;
  END: number;
  WAKE_UP: number;
  FALL: [number, number];
  STOMP: number;
  DIG0: number;
  BREAK: number;
  LAND: number;
  WAKE: number;
  ARRIVAL_RATE: number;
  BOARD: number;
  ARRIVE: number;
  PORTAL_LIT: number;
  THROUGH: number;
  WHITE: [number, number, number, number];
  PACE: number;
  POND_LEVEL: number;
  SWELL: number;
  SUN: Vec3;
  SUN_LIGHT: Vec3;
  HERO_EYE: Vec3;
  HERO_PUMPKIN: Vec3;
  LANDING_SPOT: Vec3;
  SLAB_RATE: number;
  CAVE_PUMPKIN: [number, number];
  TORCHES: [number, number][];
  LANTERNS: [number, number][];
  SNAPSHOT: [number, number];
  SNAP_TILT: Vec3;
  LEVER: [number, number];
  NAMES: string[];
  /** The homestead's features, each [from, to] in screens; the blast and its undoing: at the wall, the blast, time turns back, all back. */
  BEATS: [number, number][];
  REWIND: [number, number, number, number];
  HEADS: string[][];
  YARD: { x: [number, number]; y: [number, number]; gate: [number, number] };
  HOUSE: { x: [number, number]; y: [number, number]; door: [number, number]; window: number };
  /** Where the camera rests on the Nether's name field; where it is home again, in the valley; the portal's two swirls. */
  NAME_STOP: number;
  HOME: number;
  SWIRL: [number, number, number, number][];
  /** The closing set's signs. */
  SPECS: string[][];
}

export interface Spots {
  landing: [number, number];
  pond: { center: [number, number]; radius: [number, number] };
  cave: { center: Vec3; radius: Vec3; floor: number };
  rail: Vec3[];
  /** The homestead's cavern (the art still calls it the dungeon). */
  dungeon: { x: [number, number]; y: [number, number]; floor: number; height: number; portal: [number, number]; shaft: [[number, number], [number, number]]; tunnel: [number, number]; field: [[number, number], [number, number]] };
  front: { x: [number, number]; depth: number; bottom: number };
}

export interface NetherSpots {
  origin: Vec3;
  ledge: number;
  portal_row: number;
  bridge: [number, number];
  gate_row: number;
}

/** The places of the pumpkin's journey. */
export interface CastPoints {
  fall_from: Vec3;
  dig_start: Vec3;
  hole: Vec3;
  landing: Vec3;
  mid: Vec3;
  home: Vec3;
  seat: Vec3;
  board: Vec3;
  alight: Vec3;
  path_spot: Vec3;
  book_spot: Vec3;
  watch_spot: Vec3;
  /** Which way it faces at each of its homestead spots. */
  facing: [number, number, number];
  before: Vec3;
  inside: Vec3;
  nether_spots: Vec3[];
  nether_inside: Vec3;
}

/** Places and numbers the sets worked out while they were built. */
export interface Points {
  /** The closing set: where the camera comes and rests, the signs' boards, the lectern's book. */
  ending: { sky: Vec3; sky_aim: Vec3; eye: Vec3; aim: Vec3; spots: Vec3[]; lectern: Vec3 };
  gate: Vec3;
  lever_side: Vec3;
  /** The homestead: where its lectern's book is, the tunnel the friends come by, its gate, and the creeper's blast. */
  home_spots: { lectern: Vec3; tunnel: Vec3; gate: Vec3; blast: Vec3 };
  /** The fence's posts, as [x, y] block corners. */
  fence_posts: [number, number][];
  toys: Record<string, Vec3>;
  /** The sign where the pumpkin starts to dig: the middle of its board, and the column of the slab it stands on, if it does (it goes down with it). */
  rule: { at: Vec3; slab?: string };
  nether_click: Vec3;
  /** The other one of the dig's lost sock, on the Nether's ground by the second frame: the middle of its tile, on the ground. */
  nether_sock: Vec3;
  /** The dig as [x, z] steps, and the step at which it comes through the cave's roof. */
  steps: [number, number][];
  through: number;
  cave_folded: number;
  /** Points along the rails, and the distance along them at each. */
  rail: [Vec3[], number[]];
  cast: CastPoints;
  /** The friends at the pond: the rod's tip in the rod's own space, where the bobber floats, where the fish ends up. */
  fishing: { tag: string; tip: Vec3; float: Vec3; away: Vec3; keeps: boolean; phase: number; size: number }[];
  /** Drops from the cave's roof: x, y, where it falls from, the floor, seconds between drops, phase. */
  drips: [number, number, number, number, number, number][];
  /**
   * A chicken jockey's run behind the pond (film/jockey.ts): its line as [x, y] every `step`
   * blocks along it, the ground's height under it wherever that changes ([blocks along it,
   * height]), and the stretch of it the lens sees (blocks along it).
   */
  jockey: { path: [number, number][]; step: number; ground: [number, number][]; seen: [number, number] };
}

export interface TypeNote {
  kind: "type" | "rim" | "scrim" | "flakes";
  lines?: string[];
  palette?: string;
  cell?: number;
  depth?: number;
  width?: number;
  height?: number;
  feather?: number;
  strength?: number;
  layout?: { leading?: number; tracking?: number; align?: "left" | "right" | "center" };
  material?: string;
  lo?: Vec3;
  hi?: Vec3;
  count?: number;
  size?: [number, number];
  seed?: number;
  streaks?: boolean;
  drift?: { velocity: Vec3; sway: number };
}

export interface NodeData {
  name: string;
  at: Vec3;
  turn: Vec3;
  size: Vec3;
  parent?: string;
  color?: [number, number, number, number];
  note?: TypeNote;
  material?: string;
}

export interface LightData {
  name: string;
  kind: "point" | "sun" | "area" | "spot";
  at: Vec3;
  color: Rgb;
  energy: number;
  radius?: number;
  toward?: Vec3;
  size?: number;
  parent?: string;
}

export interface FogData {
  name: string;
  lo: Vec3;
  hi: Vec3;
  density: number;
  color: Rgb;
  anisotropy: number;
  fade: [number, number] | null;
}

export interface LayoutData {
  aspect: Aspect;
  shots: ShotData[];
  nodes: NodeData[];
  /** For the glb's meshes, by object name: what the art calls its material, and its own colour where it has one. */
  materials: Record<string, string>;
  colors: Record<string, [number, number, number, number]>;
  lights: LightData[];
  fog: FogData[];
  points: Points;
  tracks: { dig: TrackData; ride: TrackData };
  /** Facts on the cut face: [lines, palettes, cell, [centre x, bottom z]]. */
  bands: [string[], string[], number, [number, number]][];
}

export type { Pack, Records };

/** A picture's pixels, top row first. */
export async function pixelsOf(picture: Blob): Promise<ImageData> {
  const bitmap = await createImageBitmap(picture, { premultiplyAlpha: "none", colorSpaceConversion: "none" });
  const canvas = document.createElement("canvas");
  canvas.width = bitmap.width;
  canvas.height = bitmap.height;
  const context = canvas.getContext("2d", { willReadFrequently: true });
  if (!context) throw new Error("landing: no 2D canvas");
  context.drawImage(bitmap, 0, 0);
  bitmap.close();
  return context.getImageData(0, 0, canvas.width, canvas.height);
}

/**
 * The block atlas as an array texture, one layer a tile. Mipmaps then never bleed from
 * one tile into its neighbour, which they do in an atlas.
 */
export async function loadTiles(picture: Blob, atlas: SceneData["atlas"]): Promise<DataArrayTexture> {
  const image = await pixelsOf(picture);
  const { tiles, across, tile } = atlas;
  const data = new Uint8Array(tiles * tile * tile * 4);
  for (let index = 0; index < tiles; index += 1) {
    const column = index % across;
    const row = Math.floor(index / across); // counted from the bottom of the picture
    for (let y = 0; y < tile; y += 1) {
      const from = ((image.height - 1 - (row * tile + y)) * image.width + column * tile) * 4;
      data.set(image.data.subarray(from, from + tile * 4), (index * tile + y) * tile * 4);
    }
  }
  const texture = new DataArrayTexture(data, tile, tile, tiles);
  texture.colorSpace = SRGBColorSpace;
  texture.magFilter = NearestFilter;
  texture.minFilter = NearestMipmapLinearFilter;
  texture.generateMipmaps = true;
  texture.needsUpdate = true;
  return texture;
}

/** A pixel-art picture: nearest filtering, no mipmaps. */
export async function loadPicture(picture: Blob): Promise<Texture> {
  const bitmap = await createImageBitmap(picture, { premultiplyAlpha: "none", colorSpaceConversion: "none", imageOrientation: "flipY" });
  const texture = new Texture(bitmap);
  texture.colorSpace = SRGBColorSpace;
  texture.magFilter = NearestFilter;
  texture.minFilter = NearestFilter;
  texture.generateMipmaps = false;
  texture.flipY = false;
  texture.needsUpdate = true;
  return texture;
}

/** The small models, out of the glb's bytes. */
export async function readModels(glb: ArrayBuffer): Promise<Group> {
  const gltf = await new GLTFLoader().parseAsync(glb, "");
  return gltf.scene;
}
