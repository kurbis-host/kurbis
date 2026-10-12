/**
 * Light, baked into the world the way the art pipeline bakes it (kurbis/export.py), to
 * the byte: daylight and block glow flooding from cell to cell as the game counts them,
 * the smooth, corner-darkened light of every face, and which corners see a sun.
 *
 * The art works in 32-bit floats, so the arithmetic here is rounded to them at every
 * step (Math.fround): the same faces get the same bytes.
 */

import type { BakedSun, BlockTable } from "./grid";

const f = Math.fround;

/** The six sides, as the mesher numbers them: +X, -X, +Y, -Y, +Z, -Z. */
export const DIRS: readonly (readonly [number, number, number])[] = [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]];
/** A side's corners: counter-clockwise from outside, starting bottom left with Z up. */
export const QUADS: readonly (readonly (readonly [number, number, number])[])[] = [
  [[1, 0, 0], [1, 1, 0], [1, 1, 1], [1, 0, 1]],
  [[0, 1, 0], [0, 0, 0], [0, 0, 1], [0, 1, 1]],
  [[1, 1, 0], [0, 1, 0], [0, 1, 1], [1, 1, 1]],
  [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1]],
  [[0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]],
  [[0, 1, 0], [1, 1, 0], [1, 0, 0], [0, 0, 0]],
];
/** How dark a corner is with 3, 2, 1, 0 neighbours standing in it. */
const AO = [f(0.5), f(0.66), f(0.82), 1];

/** Daylight and block glow in each cell, 0..15. */
export interface Levels {
  day: Uint8Array;
  lit: Uint8Array;
}

export interface Fields extends Levels {
  /** Cells that stand in the way of light on a corner, and of the sun (see `solids`). */
  solid: Uint8Array;
}

/** Which cells stand in the way of light on a corner, and of the sun: opaque ones and leaves. */
export function solids(cells: Uint8Array, blocks: BlockTable): Uint8Array {
  const inTheWay = Uint8Array.from(blocks.group, (group, id) => (blocks.opaque[id] || group === "leaves" ? 1 : 0));
  const solid = new Uint8Array(cells.length);
  for (let i = 0; i < cells.length; i += 1) solid[i] = inTheWay[cells[i]];
  return solid;
}

/** A stack of cell indices that grows as it is pushed to. */
class Stack {
  items = new Int32Array(1 << 12);
  length = 0;

  push(value: number) {
    if (this.length === this.items.length) {
      const grown = new Int32Array(this.items.length * 2);
      grown.set(this.items);
      this.items = grown;
    }
    this.items[this.length] = value;
    this.length += 1;
  }
}

/**
 * Let light flow from cell to cell, losing `cost` of the cell it enters (one level, more
 * through leaves), through every cell that passes light. The levels are written in place.
 * (The art relaxes the whole grid fifteen times; this visits each cell as its level is
 * settled, brightest first, and ends at the same levels.)
 */
function spread(level: Uint8Array, passes: Uint8Array, cost: Uint8Array, shape: readonly [number, number, number]) {
  const [nx, ny, nz] = shape;
  const sx = ny * nz;
  const sy = nz;
  const levels: Stack[] = Array.from({ length: 16 }, () => new Stack());
  for (let i = 0; i < level.length; i += 1) {
    const here = level[i];
    if (here === 0) continue;
    // A cell among its equals has nothing to give them: only the rim of the open sky spreads.
    const x = (i / sx) | 0;
    const rest = i - x * sx;
    const y = (rest / sy) | 0;
    const z = rest - y * sy;
    if (
      (x > 0 && level[i - sx] < here - 1) || (x < nx - 1 && level[i + sx] < here - 1) ||
      (y > 0 && level[i - sy] < here - 1) || (y < ny - 1 && level[i + sy] < here - 1) ||
      (z > 0 && level[i - 1] < here - 1) || (z < nz - 1 && level[i + 1] < here - 1)
    ) levels[here].push(i);
  }
  for (let now = 15; now > 1; now -= 1) {
    const stack = levels[now];
    for (let at = 0; at < stack.length; at += 1) {
      const i = stack.items[at];
      if (level[i] !== now) continue; // it was raised since: that entry speaks for it
      const x = (i / sx) | 0;
      const rest = i - x * sx;
      const y = (rest / sy) | 0;
      const z = rest - y * sy;
      for (let side = 0; side < 6; side += 1) {
        let n: number;
        if (side === 0) { if (x === nx - 1) continue; n = i + sx; }
        else if (side === 1) { if (x === 0) continue; n = i - sx; }
        else if (side === 2) { if (y === ny - 1) continue; n = i + sy; }
        else if (side === 3) { if (y === 0) continue; n = i - sy; }
        else if (side === 4) { if (z === nz - 1) continue; n = i + 1; }
        else { if (z === 0) continue; n = i - 1; }
        if (!passes[n]) continue;
        const given = now - cost[n];
        if (given > level[n]) {
          level[n] = given;
          levels[given].push(n);
        }
      }
    }
  }
}

/**
 * Two fields of light over a grid, each 0..15 a cell: daylight coming down from the open
 * sky (and, with `front`, in through the -Y side: the level standing outside it at each
 * layer), and the glow of blocks that shine. Leaves let light through but dim it.
 */
export function levels(cells: Uint8Array, shape: readonly [number, number, number], blocks: BlockTable, sky: boolean, front?: readonly number[]): Levels {
  const [nx, ny, nz] = shape;
  const count = cells.length;
  const opaqueOf = Uint8Array.from(blocks.opaque, Number);
  const leafyOf = Uint8Array.from(blocks.group, (group) => (group === "leaves" ? 1 : 0));
  const brightOf = Uint8Array.from(blocks.bright, Number);
  const passes = new Uint8Array(count);
  const cost = new Uint8Array(count);
  for (let i = 0; i < count; i += 1) {
    const id = cells[i];
    passes[i] = opaqueOf[id] ? 0 : 1;
    cost[i] = 1 + leafyOf[id];
  }
  const day = new Uint8Array(count);
  if (sky) {
    // Open to the sky: nothing opaque at or above the cell.
    for (let column = 0; column < nx * ny; column += 1) {
      const base = column * nz;
      for (let z = nz - 1; z >= 0 && passes[base + z]; z -= 1) day[base + z] = 15;
    }
    if (front) {
      for (let x = 0; x < nx; x += 1) {
        const base = x * ny * nz;
        for (let z = 0; z < nz; z += 1) {
          if (passes[base + z]) day[base + z] = Math.max(day[base + z], Math.max(front[z], 1) - 1);
        }
      }
    }
    spread(day, passes, cost, shape);
  }
  const lit = new Uint8Array(count);
  for (let i = 0; i < count; i += 1) {
    if (brightOf[cells[i]]) {
      lit[i] = 15;
      passes[i] = 1;
    }
  }
  spread(lit, passes, cost, shape);
  return { day, lit };
}

/** For each corner of a side: the two sideways steps that lead to the cells meeting at that corner. */
function cornerSteps(side: number): [number[], number[]][] {
  const axis = side >> 1;
  return QUADS[side].map((corner) => {
    const steps: number[][] = [];
    for (let t = 0; t < 3; t += 1) {
      if (t === axis) continue;
      const step = [0, 0, 0];
      step[t] = corner[t] > 0.5 ? 1 : -1;
      steps.push(step);
    }
    return [steps[0], steps[1]] as [number[], number[]];
  });
}

/** The byte for a corner: the mean of four levels (their sum given), darkened by how many of its three neighbours stand in it. */
const BYTE = (() => {
  const table = new Uint8Array(61 * 4);
  for (let sum = 0; sum <= 60; sum += 1) {
    for (let standing = 0; standing < 4; standing += 1) {
      const value = f(f(f(f(sum / 4) / 15) * AO[3 - standing]) * 255);
      table[sum * 4 + standing] = Math.min(Math.max(value, 0), 255) | 0;
    }
  }
  return table;
})();

/**
 * Smooth light for every corner of every face: sixteen bytes a face, four each of
 * daylight, glow, direct sun and the key suns (the last two are filled in by `sunlight`).
 * A corner takes the mean of the four cells that meet there in front of the face, and is
 * darkened by how many of them are solid, the way the game shades its corners.
 */
export function cornerLight(faces: { count: number; x: Int16Array; y: Int16Array; z: Int16Array; side: Uint8Array }, fields: Fields, shape: readonly [number, number, number], out: Uint8Array) {
  const [nx, ny, nz] = shape;
  const { day, lit, solid } = fields;
  // For each side and corner, the way to the three other cells that meet there (one step sideways, the other, both): as steps along the axes, and as a step through the cells.
  const steps = new Int8Array(6 * 4 * 3 * 3);
  const strides = new Int32Array(6 * 4 * 3);
  for (let side = 0; side < 6; side += 1) {
    cornerSteps(side).forEach(([a, b], j) => {
      [a, b, [a[0] + b[0], a[1] + b[1], a[2] + b[2]]].forEach((step, which) => {
        const k = (side * 4 + j) * 3 + which;
        steps.set(step, k * 3);
        strides[k] = (step[0] * ny + step[1]) * nz + step[2];
      });
    });
  }
  const ahead = Int8Array.from(DIRS.flat());
  // Outside the grid there is no light and nothing stands.
  const at = (x: number, y: number, z: number) => (x < 0 || y < 0 || z < 0 || x >= nx || y >= ny || z >= nz ? -1 : (x * ny + y) * nz + z);
  for (let i = 0; i < faces.count; i += 1) {
    const side = faces.side[i];
    const fx = faces.x[i] + ahead[side * 3];
    const fy = faces.y[i] + ahead[side * 3 + 1];
    const fz = faces.z[i] + ahead[side * 3 + 2];
    const front = at(fx, fy, fz);
    const day0 = front < 0 ? 0 : day[front];
    const lit0 = front < 0 ? 0 : lit[front];
    // (Nearly every face looks into a cell with the grid all around it: no need to ask of each neighbour whether it is there.)
    const inner = fx > 0 && fy > 0 && fz > 0 && fx < nx - 1 && fy < ny - 1 && fz < nz - 1;
    for (let j = 0; j < 4; j += 1) {
      const k = (side * 4 + j) * 3;
      const i1 = inner ? front + strides[k] : at(fx + steps[k * 3], fy + steps[k * 3 + 1], fz + steps[k * 3 + 2]);
      const i2 = inner ? front + strides[k + 1] : at(fx + steps[k * 3 + 3], fy + steps[k * 3 + 4], fz + steps[k * 3 + 5]);
      const ic = inner ? front + strides[k + 2] : at(fx + steps[k * 3 + 6], fy + steps[k * 3 + 7], fz + steps[k * 3 + 8]);
      const o1 = i1 < 0 ? 0 : solid[i1];
      const o2 = i2 < 0 ? 0 : solid[i2];
      const oc = (ic < 0 ? 0 : solid[ic]) | (o1 & o2);
      const standing = o1 + o2 + oc;
      const daySum = day0 + (o1 ? day0 : i1 < 0 ? 0 : day[i1]) + (o2 ? day0 : i2 < 0 ? 0 : day[i2]) + (oc ? day0 : ic < 0 ? 0 : day[ic]);
      const litSum = lit0 + (o1 ? lit0 : i1 < 0 ? 0 : lit[i1]) + (o2 ? lit0 : i2 < 0 ? 0 : lit[i2]) + (oc ? lit0 : ic < 0 ? 0 : lit[ic]);
      out[i * 16 + j] = BYTE[daySum * 4 + standing];
      out[i * 16 + 4 + j] = BYTE[litSum * 4 + standing];
    }
  }
}

/** The grid is looked at in columns COARSE cells across, to know where a ray has nothing to meet. */
const COARSE = 8;

/**
 * Over each column of the grid, COARSE cells across: the height above which nothing
 * stands (0 where nothing stands at all). A ray that is above it meets nothing there.
 */
function roofs(solid: Uint8Array, shape: readonly [number, number, number]): { roof: Uint8Array; across: number } {
  const [nx, ny, nz] = shape;
  const across = Math.ceil(ny / COARSE);
  const roof = new Uint8Array(Math.ceil(nx / COARSE) * across);
  for (let x = 0; x < nx; x += 1) {
    for (let y = 0; y < ny; y += 1) {
      const column = ((x / COARSE) | 0) * across + ((y / COARSE) | 0);
      const base = (x * ny + y) * nz;
      let z = nz - 1;
      while (z >= roof[column] && !solid[base + z]) z -= 1;
      if (z + 1 > roof[column]) roof[column] = z + 1;
    }
  }
  return { roof, across };
}

/**
 * Which corners see a sun: a ray from each, a little in from the corner and off the
 * face, marched through the grid half a block at a time. Whatever stands in the way
 * (leaves too) dims it. Written into four bytes a face, starting at `column`.
 *
 * (A sun is above the horizon, so a ray only rises. Once it is above everything that
 * stands in the stretch of the grid it is over, the samples it would take there are not
 * taken: they would all find air. Every sample that is taken is the one the art takes,
 * so the result is the same.)
 *
 * With `every` and `phase`, only every so-manieth face is worked out (those whose number
 * leaves `phase` when divided by `every`): the rays are most of the work of building a
 * world, and this is how several workers share them. `out` has `width` bytes for each
 * face that is worked out, in their order.
 */
export function sunlight(
  faces: { count: number; x: Int16Array; y: Int16Array; z: Int16Array; side: Uint8Array },
  solid: Uint8Array,
  shape: readonly [number, number, number],
  sun: BakedSun,
  out: Uint8Array,
  column: number,
  width = 16,
  every = 1,
  phase = 0,
) {
  const [nx, ny, nz] = shape;
  const toward = [f(sun.toward[0]), f(sun.toward[1]), f(sun.toward[2])];
  const [sx, sy, sz] = toward;
  const rising = sz >= 0;
  const { roof, across } = roofs(solid, shape);
  // Samples a ray takes while it moves one block along x and along y (none, if it does not move along it).
  const paceX = sx === 0 ? Infinity : 1 / (Math.abs(sx) * 0.5);
  const paceY = sy === 0 ? Infinity : 1 / (Math.abs(sy) * 0.5);
  // How far along each axis a ray has moved at each sample.
  const dx = new Float32Array(sun.reach);
  const dy = new Float32Array(sun.reach);
  const dz = new Float32Array(sun.reach);
  for (let step = 0; step < sun.reach; step += 1) {
    dx[step] = sx * (0.5 * step);
    dy[step] = sy * (0.5 * step);
    dz[step] = sz * (0.5 * step);
  }
  const dim = f(0.45);
  const least = f(0.02);
  const inset = f(0.12);
  const lift = f(0.05);
  // Where each corner's ray starts, from its cell: a little toward the middle of the face, and off it.
  const starts = QUADS.map((quad, side) => {
    const middle = [0, 1, 2].map((c) => f(f(f(f(quad[0][c] + quad[1][c]) + quad[2][c]) + quad[3][c]) / 4));
    return { quad, middle, off: DIRS[side].map((d) => f(d * lift)) };
  });
  const faced = DIRS.map((dir) => f(f(f(dir[0] * sx) + f(dir[1] * sy)) + f(dir[2] * sz)) > 0.02);
  for (let i = phase, row = 0; i < faces.count; i += every, row += 1) {
    const side = faces.side[i];
    if (!faced[side] || faces.z[i] < sun.floor) continue;
    const cx = faces.x[i];
    const cy = faces.y[i];
    const cz = faces.z[i];
    const { quad, middle, off } = starts[side];
    for (let j = 0; j < 4; j += 1) {
      const corner = quad[j];
      const ax = cx + corner[0];
      const ay = cy + corner[1];
      const az = cz + corner[2];
      const x0 = f(f(ax + f(f(f(cx + middle[0]) - ax) * inset)) + off[0]);
      const y0 = f(f(ay + f(f(f(cy + middle[1]) - ay) * inset)) + off[1]);
      const z0 = f(f(az + f(f(f(cz + middle[2]) - az) * inset)) + off[2]);
      let seen = 1;
      for (let step = 1; step < sun.reach; step += 1) {
        const px = f(x0 + dx[step]);
        const py = f(y0 + dy[step]);
        const pz = f(z0 + dz[step]);
        const x = Math.floor(px);
        const y = Math.floor(py);
        const z = Math.floor(pz);
        if (x < 0 || y < 0 || z < 0 || x >= nx || y >= ny || z >= nz) break;
        if (solid[(x * ny + y) * nz + z]) {
          seen = f(seen * dim);
          if (!(seen > least)) break;
          continue;
        }
        if (!rising) continue;
        const bx = (x / COARSE) | 0;
        const by = (y / COARSE) | 0;
        if (z < roof[bx * across + by]) continue;
        // Above everything in this stretch: go on to the last sample that is still well inside it.
        const tx = ((sx > 0 ? (bx + 1) * COARSE - px : px - bx * COARSE) - 1e-3) * paceX;
        const ty = ((sy > 0 ? (by + 1) * COARSE - py : py - by * COARSE) - 1e-3) * paceY;
        const skip = Math.ceil(Math.min(tx, ty)) - 1;
        if (skip > 0) step += skip;
      }
      out[row * width + column + j] = Math.min(Math.max(f(seen * 255), 0), 255) | 0;
    }
  }
}
