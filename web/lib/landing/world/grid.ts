/**
 * The world as the art builds it: a box of block ids. The page gets this (a tenth of a
 * megabyte or two) instead of the faces it draws (megabytes), and works the faces and
 * their light out itself (./build.ts), exactly as the art pipeline would. Only the
 * suns' shadows come ready-made, with the grid: marching their rays is most of what
 * baking a world costs, and too much to ask of a weak device.
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

/**
 * The suns' shadows on a world's faces, as they are sent: for each sun, two bytes for
 * every face that can see it at all, in the order the faces are found (./build.ts).
 * The two bytes are the face's four corners, each a place among `levels`.
 */
export interface Shadows {
  /** The light a corner gets, by its place: a sun is seen whole, or dimmed by what stands in the way. */
  levels: number[];
  suns: {
    /** Where this sun's four bytes stand among the sixteen of a face's light. */
    column: number;
    /** Which faces can see it: those of these sides (+X, -X, +Y, -Y, +Z, -Z), at or above this layer. */
    sides: number[];
    floor: number;
    /** How many such faces there are. */
    faces: number;
  }[];
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
  shadows?: Shadows;
  parts: Part[];
}

export interface Grid extends GridHeader {
  cells: Uint8Array;
  /** The shadows' bytes (see `Shadows`), sun after sun: none where there are no suns. */
  shadowBytes: Uint8Array;
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

/** Read a grid file: 'KVG1', the header's length, the header (JSON), one byte a cell, then the suns' shadows. */
export function readGrid(buffer: ArrayBuffer): Grid {
  const { header, cells } = headed(buffer);
  const grid = header as GridHeader;
  const [nx, ny, nz] = grid.shape;
  const count = nx * ny * nz;
  const shadowed = (grid.shadows?.suns ?? []).reduce((bytes, sun) => bytes + sun.faces * 2, 0);
  if (cells.length < count + shadowed) throw new Error("landing: the grid file is short");
  return { ...grid, cells: cells.subarray(0, count), shadowBytes: cells.subarray(count, count + shadowed) };
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
