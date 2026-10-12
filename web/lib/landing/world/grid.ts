/**
 * The world as the art builds it: a box of block ids. The page gets this (a few tens of
 * kilobytes) instead of the faces it draws (megabytes), and works the faces and their
 * light out itself (./build.ts), exactly as the art pipeline would.
 *
 * Nothing here needs a window or a GPU: it runs in a worker, and in the tests.
 */

/** What the builder must know about each kind of block, by block id (0 is air). */
export interface BlockTable {
  /** The atlas tile on each side: +X, -X, +Y, -Y, +Z, -Z. */
  tiles: number[][];
  /** Whether it hides what is behind it. */
  opaque: boolean[];
  /** Which mesh its faces go into: "opaque", "leaves", "water", or something not drawn from the grid. */
  group: string[];
  /** Whether it shines (lava, glowstone, a lamp). */
  bright: boolean[];
}

/** A sun whose shadow is baked: the way to it, how far a ray is followed, and the lowest layer that is lit at all. */
export interface BakedSun {
  toward: [number, number, number];
  reach: number;
  floor: number;
}

/** One piece of the world's faces: those of one group inside (or outside) some boxes. */
export interface Part {
  name: string;
  group: string;
  /** World boxes [x0, y0, z0, x1, y1, z1] a face must lie in (any of them), if given. */
  inside?: number[][];
  /** And boxes it must not lie in. */
  outside?: number[][];
}

/** The header of a grid file: everything about a world but its cells. */
export interface GridHeader {
  /** Cells across x, y and z. A cell (x, y, z) is at index (x * ny + y) * nz + z. */
  shape: [number, number, number];
  /** The world coordinates of cell (0, 0, 0). */
  origin: [number, number, number];
  blocks: BlockTable;
  /** Sides of the box that count as buried, so no face shows there: indices into +X, -X, +Y, -Y, +Z, -Z. */
  closed: number[];
  /** Whether daylight falls in from above. */
  sky: boolean;
  /** Daylight standing outside the -Y side at each layer (the cut face), if any does. */
  front?: number[];
  sun?: BakedSun;
  key?: BakedSun;
  parts: Part[];
}

export interface Grid extends GridHeader {
  cells: Uint8Array;
}

/** A change to a grid: a box of other cells (the dungeon, which differs by frame shape). */
export interface Patch {
  at: [number, number, number];
  shape: [number, number, number];
  cells: Uint8Array;
}

/** A small thing made of blocks, cut into pieces that each become a mesh (the slab that falls from the cut face). */
export interface BlockModel {
  shape: [number, number, number];
  /** The world coordinates of cell (0, 0, 0). */
  origin: [number, number, number];
  blocks: BlockTable;
  /** What the art calls the material its pieces are in. */
  material: string;
  /** Each piece: its name, the box of cells it is, and where its mesh's own origin stands in the world. */
  pieces: { name: string; at: [number, number, number]; shape: [number, number, number]; centre: [number, number, number] }[];
  cells: Uint8Array;
}

const MAGIC = 0x4b564731; // 'KVG1'

function headed(buffer: ArrayBuffer): { header: unknown; cells: Uint8Array } {
  const view = new DataView(buffer);
  if (view.getUint32(0, false) !== MAGIC) throw new Error("landing: not a grid file");
  const bytes = view.getUint32(4, true);
  const header = JSON.parse(new TextDecoder().decode(new Uint8Array(buffer, 8, bytes))) as unknown;
  return { header, cells: new Uint8Array(buffer, 8 + bytes) };
}

/** Read a grid file: 'KVG1', the header's length, the header (JSON), then one byte a cell. */
export function readGrid(buffer: ArrayBuffer): Grid {
  const { header, cells } = headed(buffer);
  const grid = header as GridHeader;
  const [nx, ny, nz] = grid.shape;
  if (cells.length < nx * ny * nz) throw new Error("landing: the grid file is short of cells");
  return { ...grid, cells: cells.subarray(0, nx * ny * nz) };
}

export function readModel(buffer: ArrayBuffer): BlockModel {
  const { header, cells } = headed(buffer);
  return { ...(header as Omit<BlockModel, "cells">), cells };
}

export function readPatch(buffer: ArrayBuffer): Patch {
  const { header, cells } = headed(buffer);
  const patch = header as Omit<Patch, "cells">;
  return { ...patch, cells: cells.subarray(0, patch.shape[0] * patch.shape[1] * patch.shape[2]) };
}

/** The grid with a patch laid into it (a new grid: the one given is left as it was). */
export function patched(grid: Grid, patch: Patch): Grid {
  const cells = grid.cells.slice();
  const [, ny, nz] = grid.shape;
  const [px, py, pz] = patch.at;
  const [sx, sy, sz] = patch.shape;
  for (let x = 0; x < sx; x += 1) {
    for (let y = 0; y < sy; y += 1) {
      const from = (x * sy + y) * sz;
      cells.set(patch.cells.subarray(from, from + sz), ((px + x) * ny + py + y) * nz + pz);
    }
  }
  return { ...grid, cells };
}
