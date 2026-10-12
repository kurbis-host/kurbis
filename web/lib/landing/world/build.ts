/**
 * From a grid of blocks to what the page draws: every visible block face, with its
 * light, sorted into cubes of the world. What the art pipeline used to send ready-made.
 */

import { cornerLight, levels, solids, sunlight, type Levels } from "./light";
import type { BakedSun, BlockTable, Grid, Part } from "./grid";

/** Faces are grouped into cubes this many blocks across, to cull by. */
const CHUNK = 32;

/** One mesh's worth of block faces, as the renderer takes them. `chunks` are runs of faces that share a cube of the world. */
export interface Records {
  count: number;
  /** [x0, y0, z0, x1, y1, z1, first, count] */
  chunks: number[][];
  /** x, y, z, direction per face */
  cells: Int16Array;
  /** tile, 0 per face */
  tiles: Uint16Array;
  /** daylight x4, glow x4, sun x4, key suns x4 per face: one of each per corner */
  light: Uint8Array;
}

interface Faces {
  count: number;
  /** The cell each face belongs to, in grid coordinates, and the side of it. */
  x: Int16Array;
  y: Int16Array;
  z: Int16Array;
  side: Uint8Array;
  tile: Uint16Array;
  /** The block's group, as an index into the groups asked for. */
  group: Uint8Array;
}

/**
 * The visible faces of the blocks in `groups`: those whose neighbour neither hides them
 * nor is the same block. A side of the box that is `closed` shows none.
 */
function findFaces(grid: Grid, groups: readonly string[]): Faces {
  const [nx, ny, nz] = grid.shape;
  const { cells, blocks } = grid;
  const opaque = Uint8Array.from(blocks.opaque, Number);
  const groupOf = Uint8Array.from(blocks.group, (group) => groups.indexOf(group) + 1);
  const tiles = Uint16Array.from(blocks.tiles.flat());
  const closed = [0, 1, 2, 3, 4, 5].map((side) => grid.closed.includes(side));
  const sx = ny * nz;
  const sy = nz;
  const strides = [sx, -sx, sy, -sy, 1, -1];
  let room = 1 << 16;
  let faces: Faces = { count: 0, x: new Int16Array(room), y: new Int16Array(room), z: new Int16Array(room), side: new Uint8Array(room), tile: new Uint16Array(room), group: new Uint8Array(room) };
  const grow = () => {
    room *= 2;
    const wider: Faces = { count: faces.count, x: new Int16Array(room), y: new Int16Array(room), z: new Int16Array(room), side: new Uint8Array(room), tile: new Uint16Array(room), group: new Uint8Array(room) };
    wider.x.set(faces.x);
    wider.y.set(faces.y);
    wider.z.set(faces.z);
    wider.side.set(faces.side);
    wider.tile.set(faces.tile);
    wider.group.set(faces.group);
    faces = wider;
  };
  const add = (x: number, y: number, z: number, side: number, id: number, group: number) => {
    if (faces.count === room) grow();
    const at = faces.count;
    faces.x[at] = x;
    faces.y[at] = y;
    faces.z[at] = z;
    faces.side[at] = side;
    faces.tile[at] = tiles[id * 6 + side];
    faces.group[at] = group - 1;
    faces.count += 1;
  };
  let i = 0;
  for (let x = 0; x < nx; x += 1) {
    for (let y = 0; y < ny; y += 1) {
      const inner = x > 0 && x < nx - 1 && y > 0 && y < ny - 1;
      for (let z = 0; z < nz; z += 1, i += 1) {
        const id = cells[i];
        const group = groupOf[id];
        if (!group) continue;
        if (inner && z > 0 && z < nz - 1) {
          // Away from the box's sides, where nearly every block is: all six neighbours are there, and a buried block (most are) is done with at once.
          const east = cells[i + sx];
          const west = cells[i - sx];
          const north = cells[i + sy];
          const south = cells[i - sy];
          const above = cells[i + 1];
          const below = cells[i - 1];
          if (opaque[east] & opaque[west] & opaque[north] & opaque[south] & opaque[above] & opaque[below]) continue;
          if (!opaque[east] && east !== id) add(x, y, z, 0, id, group);
          if (!opaque[west] && west !== id) add(x, y, z, 1, id, group);
          if (!opaque[north] && north !== id) add(x, y, z, 2, id, group);
          if (!opaque[south] && south !== id) add(x, y, z, 3, id, group);
          if (!opaque[above] && above !== id) add(x, y, z, 4, id, group);
          if (!opaque[below] && below !== id) add(x, y, z, 5, id, group);
          continue;
        }
        for (let side = 0; side < 6; side += 1) {
          const edge = side === 0 ? x === nx - 1 : side === 1 ? x === 0 : side === 2 ? y === ny - 1 : side === 3 ? y === 0 : side === 4 ? z === nz - 1 : z === 0;
          if (edge) {
            if (closed[side]) continue;
          } else {
            const next = cells[i + strides[side]];
            if (opaque[next] || next === id) continue;
          }
          add(x, y, z, side, id, group);
        }
      }
    }
  }
  return faces;
}

/** Whether a cell is in any of some boxes (six numbers each, one after the other). */
function within(boxes: Int32Array, x: number, y: number, z: number): boolean {
  for (let b = 0; b < boxes.length; b += 6) {
    if (x >= boxes[b] && y >= boxes[b + 1] && z >= boxes[b + 2] && x < boxes[b + 3] && y < boxes[b + 4] && z < boxes[b + 5]) return true;
  }
  return false;
}

/** The faces of one part, sorted into cubes: what the renderer draws as one mesh. */
function gather(grid: Grid, faces: Faces, light: Uint8Array, part: Part, group: number): Records {
  const [ox, oy, oz] = grid.origin;
  const [nx, ny, nz] = grid.shape;
  // Cubes of the world, counted from the one the grid's first cell is in: in order of z, then y, then x.
  const bx = Math.floor(ox / CHUNK);
  const by = Math.floor(oy / CHUNK);
  const bz = Math.floor(oz / CHUNK);
  const cx = Math.floor((ox + nx - 1) / CHUNK) - bx + 1;
  const cy = Math.floor((oy + ny - 1) / CHUNK) - by + 1;
  const cz = Math.floor((oz + nz - 1) / CHUNK) - bz + 1;
  const cube = new Int32Array(faces.count).fill(-1);
  const filled = new Int32Array(cx * cy * cz + 1);
  const inside = part.inside ? Int32Array.from(part.inside.flat()) : null;
  const outside = part.outside ? Int32Array.from(part.outside.flat()) : null;
  let count = 0;
  for (let i = 0; i < faces.count; i += 1) {
    if (faces.group[i] !== group) continue;
    const x = faces.x[i] + ox;
    const y = faces.y[i] + oy;
    const z = faces.z[i] + oz;
    if (inside && !within(inside, x, y, z)) continue;
    if (outside && within(outside, x, y, z)) continue;
    const key = ((Math.floor(z / CHUNK) - bz) * cy + Math.floor(y / CHUNK) - by) * cx + Math.floor(x / CHUNK) - bx;
    cube[i] = key;
    filled[key + 1] += 1;
    count += 1;
  }
  // Each cube's faces go where the cubes before it end (the faces keep the order they were found in).
  for (let key = 0; key < filled.length - 1; key += 1) filled[key + 1] += filled[key];
  const cells = new Int16Array(count * 4);
  const tiles = new Uint16Array(count * 2);
  const shade = new Uint8Array(count * 16);
  const bounds = new Map<number, number[]>();
  const next = filled.slice(0, -1);
  for (let i = 0; i < faces.count; i += 1) {
    const key = cube[i];
    if (key < 0) continue;
    const n = next[key];
    next[key] += 1;
    const x = faces.x[i] + ox;
    const y = faces.y[i] + oy;
    const z = faces.z[i] + oz;
    cells[n * 4] = x;
    cells[n * 4 + 1] = y;
    cells[n * 4 + 2] = z;
    cells[n * 4 + 3] = faces.side[i];
    tiles[n * 2] = faces.tile[i];
    for (let b = 0; b < 16; b += 1) shade[n * 16 + b] = light[i * 16 + b];
    let chunk = bounds.get(key);
    if (!chunk) {
      chunk = [x, y, z, x + 1, y + 1, z + 1, filled[key], filled[key + 1] - filled[key]];
      bounds.set(key, chunk);
    } else {
      if (x < chunk[0]) chunk[0] = x;
      if (y < chunk[1]) chunk[1] = y;
      if (z < chunk[2]) chunk[2] = z;
      if (x + 1 > chunk[3]) chunk[3] = x + 1;
      if (y + 1 > chunk[4]) chunk[4] = y + 1;
      if (z + 1 > chunk[5]) chunk[5] = z + 1;
    }
  }
  const chunks = [...bounds.entries()].sort((a, b) => a[0] - b[0]).map(([, chunk]) => chunk);
  return { count, chunks, cells, tiles, light: shade };
}

/**
 * How long each stage of a build took, in milliseconds (for the curious, and the tests).
 * With helpers the stages run side by side, so they add up to more than `all`.
 */
export interface Timing {
  levels: number;
  faces: number;
  corners: number;
  suns: number;
  gather: number;
  all: number;
}

/** The light in every cell of a grid, asked for. */
export interface FloodJob {
  cells: Uint8Array;
  shape: [number, number, number];
  blocks: BlockTable;
  sky: boolean;
  front?: number[];
}

/** A share of the suns' rays: the faces to work out are every `every`-th one from `phase`. */
export interface SunJob {
  faces: { count: number; x: Int16Array; y: Int16Array; z: Int16Array; side: Uint8Array };
  solid: Uint8Array;
  shape: [number, number, number];
  sun?: BakedSun;
  key?: BakedSun;
  every: number;
  phase: number;
}

/**
 * Someone else to give work to: another worker (./worker.ts). What it is asked for, it
 * answers as `flood` and `sunShare` here would (and may say how long the flood `took` it).
 */
export interface Helper {
  flood(job: FloodJob): Promise<Levels & { took?: number }>;
  suns(job: SunJob): Promise<Uint8Array>;
}

export function flood(job: FloodJob): Levels {
  return levels(job.cells, job.shape, job.blocks, job.sky, job.front);
}

/** The suns' bytes for a job's share of the faces: eight a face (the sun's four corners, then the key suns'). */
export function sunShare(job: SunJob): Uint8Array {
  const out = new Uint8Array(Math.ceil(Math.max(job.faces.count - job.phase, 0) / job.every) * 8);
  if (job.sun) sunlight(job.faces, job.solid, job.shape, job.sun, out, 0, 8, job.every, job.phase);
  if (job.key) sunlight(job.faces, job.solid, job.shape, job.key, out, 4, 8, job.every, job.phase);
  return out;
}

const now = () => performance.now();

function gatherAll(grid: Grid, parts: readonly Part[], groups: readonly string[], faces: Faces, light: Uint8Array): Map<string, Records> {
  const out = new Map<string, Records>();
  for (const part of parts) out.set(part.name, gather(grid, faces, light, part, groups.indexOf(part.group)));
  return out;
}

/** Build every part of a grid's world: its faces, lit and sorted. `only` leaves the other parts out. */
export function build(grid: Grid, only?: readonly string[], timing?: Timing): Map<string, Records> {
  const took = timing ?? ({} as Timing);
  const started = now();
  const parts = grid.parts.filter((part) => !only || only.includes(part.name));
  const groups = [...new Set(grid.parts.map((part) => part.group))];
  const solid = solids(grid.cells, grid.blocks);
  const fields = { ...levels(grid.cells, grid.shape, grid.blocks, grid.sky, grid.front), solid };
  took.levels = now() - started;
  let t = now();
  const faces = findFaces(grid, groups);
  took.faces = now() - t;
  t = now();
  const light = new Uint8Array(faces.count * 16);
  cornerLight(faces, fields, grid.shape, light);
  took.corners = now() - t;
  t = now();
  if (grid.sun) sunlight(faces, solid, grid.shape, grid.sun, light, 8);
  if (grid.key) sunlight(faces, solid, grid.shape, grid.key, light, 12);
  took.suns = now() - t;
  t = now();
  const out = gatherAll(grid, parts, groups, faces, light);
  took.gather = now() - t;
  took.all = now() - started;
  return out;
}

/**
 * The same, with helpers, in less time: the light in the cells and the faces do not wait
 * for each other, so one helper floods the grid while the faces are found here; and the
 * suns' rays, most of the work, are shared among everyone. Whoever worked what out, the
 * faces come out the same. A helper that lets it down has its work done here.
 */
export async function buildWith(grid: Grid, helpers: readonly Helper[], timing?: Timing): Promise<Map<string, Records>> {
  if (!helpers.length) return build(grid, undefined, timing);
  const took = timing ?? ({} as Timing);
  const started = now();
  const groups = [...new Set(grid.parts.map((part) => part.group))];
  const flooded: FloodJob = { cells: grid.cells, shape: grid.shape, blocks: grid.blocks, sky: grid.sky, front: grid.front };
  const flooding = helpers[0].flood(flooded).catch((): Levels & { took?: number } => flood(flooded));
  let t = now();
  const faces = findFaces(grid, groups);
  took.faces = now() - t;
  const { count } = faces;
  const solid = solids(grid.cells, grid.blocks);
  const light = new Uint8Array(count * 16);
  // The rays need the faces and what stands in their way, no more: they need not wait for the flood.
  t = now();
  const hands = helpers.length + 1;
  let shares: Promise<Uint8Array[]> = Promise.resolve([]);
  if (grid.sun || grid.key) {
    // (Tight copies: what is sent to a helper is copied whole, slack and all.)
    const job: SunJob = {
      faces: { count, x: faces.x.slice(0, count), y: faces.y.slice(0, count), z: faces.z.slice(0, count), side: faces.side.slice(0, count) },
      solid,
      shape: grid.shape,
      sun: grid.sun,
      key: grid.key,
      every: hands,
      phase: 0,
    };
    const asked = helpers.map((helper, k) => helper.suns({ ...job, phase: k + 1 }).catch(() => sunShare({ ...job, phase: k + 1 })));
    const own = sunShare(job);
    shares = Promise.all(asked).then((others) => [own, ...others]);
  }
  const { day, lit, took: floodTook } = await flooding;
  took.levels = floodTook ?? now() - started;
  const c = now();
  cornerLight(faces, { day, lit, solid }, grid.shape, light);
  took.corners = now() - c;
  (await shares).forEach((share, phase) => {
    for (let i = phase, from = 0; i < count; i += hands, from += 8) {
      const to = i * 16 + 8;
      for (let b = 0; b < 8; b += 1) light[to + b] = share[from + b];
    }
  });
  took.suns = now() - t;
  t = now();
  const out = gatherAll(grid, grid.parts, groups, faces, light);
  took.gather = now() - t;
  took.all = now() - started;
  return out;
}
