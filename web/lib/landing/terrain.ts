/**
 * Procedural cross-section of the landing world: a slab of blocks three deep,
 * seen from the front. Deterministic, so server and client agree.
 */

export const BLOCK_TYPES = [
  "grass",
  "dirt",
  "stone",
  "deep",
  "bedrock",
  "nether",
  "sand",
  "water",
  "leaf",
  "log",
  "moss",
  "lava",
] as const;

export type BlockType = (typeof BLOCK_TYPES)[number];

export const WORLD = { xMin: -24, xMax: 32, yMin: -96, yMax: 9, depth: 3 } as const;

/** Points of interest shared by the terrain carving and the 3D props. */
export const SPOTS = {
  mascotLanding: { x: 8.5, y: 0.5 },
  tree: { x: 5 },
  pond: { x: 14 },
  cave: { x: 10, y: -22, rx: 8, ry: 4.6, floor: -25 },
  spawner: { x: 10, y: -44.5, halfWidth: 4, top: -42, bottom: -47 },
  chest: { x: 10, y: -52, halfWidth: 2, top: -51, bottom: -53 },
  netherCavern: { x: 10.5, y: -85, rx: 9, ry: 6, floor: -89 },
} as const;

export interface BlockBatch {
  type: BlockType;
  count: number;
  /** x, y, z per block. */
  positions: Float32Array;
  /** Brightness multiplier per block. */
  shades: Float32Array;
}

function hash(x: number, y: number, z = 0): number {
  let h = Math.imul(x | 0, 374761393) ^ Math.imul(y | 0, 668265263) ^ Math.imul(z | 0, 1274126177);
  h = Math.imul(h ^ (h >>> 13), 1274126177);
  return ((h ^ (h >>> 16)) >>> 0) / 4294967295;
}

function smooth(t: number): number {
  return t * t * (3 - 2 * t);
}

/** Value noise in 0..1. */
function noise(x: number, y: number, scale: number, seed: number): number {
  const fx = x / scale;
  const fy = y / scale;
  const x0 = Math.floor(fx);
  const y0 = Math.floor(fy);
  const tx = smooth(fx - x0);
  const ty = smooth(fy - y0);
  const top = hash(x0, y0, seed) * (1 - tx) + hash(x0 + 1, y0, seed) * tx;
  const bottom = hash(x0, y0 + 1, seed) * (1 - tx) + hash(x0 + 1, y0 + 1, seed) * tx;
  return top * (1 - ty) + bottom * ty;
}

/** Y of the topmost ground block in a column. */
export function surfaceTop(x: number): number {
  if (x >= -3 && x <= 25) return 0;
  return Math.round(noise(x, 0, 5, 3) * 2);
}

function inEllipse(x: number, y: number, spot: { x: number; y: number; rx: number; ry: number }, wobble: number): boolean {
  const dx = (x - spot.x) / spot.rx;
  const dy = (y - spot.y) / spot.ry;
  return dx * dx + dy * dy < 1 + wobble;
}

function ground(x: number, y: number): BlockType | null {
  const top = surfaceTop(x);
  if (y > top) return null;

  const pond = Math.abs(x - SPOTS.pond.x);
  if (y >= -1 && pond <= 3 + y) return "water";
  if (y >= -3 && pond <= 5 + y) return "sand";

  if (y === top) return "grass";

  const dirtBottom = top - 13 - Math.round(noise(x, 0, 6, 1) * 4);
  if (y > dirtBottom) return y < -7 && noise(x, y, 4, 2) > 0.74 ? "stone" : "dirt";

  if (y > -52) return y > dirtBottom - 5 && noise(x, y, 4, 4) > 0.7 ? "dirt" : "stone";
  if (y > -58) return hash(x, y, 5) < (-52 - y) / 6 ? "deep" : "stone";
  if (y > -67) return "deep";
  if (y > -72) return y <= -69 || hash(x, y, 6) < 0.5 ? "bedrock" : "deep";

  if (y > -74 && hash(x, y, 6) < 0.4) return "bedrock";
  if (y <= -94) return noise(x, 0, 5, 8) > 0.35 ? "lava" : "nether";
  return y < -75 && noise(x, y, 4, 7) > 0.78 ? "lava" : "nether";
}

function carved(x: number, y: number): boolean {
  const { cave, spawner, chest, netherCavern } = SPOTS;
  const wobble = (noise(x, y, 3, 9) - 0.5) * 0.4;
  if (y > cave.floor && inEllipse(x, y, cave, wobble)) return true;
  if (y > netherCavern.floor && inEllipse(x, y, netherCavern, wobble)) return true;
  if (Math.abs(x - spawner.x) <= spawner.halfWidth && y <= spawner.top && y >= spawner.bottom) return true;
  if (Math.abs(x - chest.x) <= chest.halfWidth && y <= chest.top && y >= chest.bottom) return true;
  return false;
}

function nearSpawner(x: number, y: number): boolean {
  const { spawner } = SPOTS;
  return Math.abs(x - spawner.x) <= spawner.halfWidth + 1 && y <= spawner.top + 1 && y >= spawner.bottom - 1;
}

function tree(x: number, y: number, z: number): BlockType | null {
  const dx = x - SPOTS.tree.x;
  if (dx === 0 && z === 1 && y >= 1 && y <= 4) return "log";
  const dy = y - 5.2;
  const dz = z - 1;
  if (y >= 4 && dx * dx + dy * dy * 1.3 + dz * dz * 1.5 < 5.6) return "leaf";
  return null;
}

/** Block at a cell, or null for air. `z` counts away from the viewer (0 is the front). */
export function blockAt(x: number, y: number, z: number): BlockType | null {
  if (y > surfaceTop(x)) return tree(x, y, z);
  const base = ground(x, y);
  if (!base) return null;
  if (z < 2 && carved(x, y)) return null;
  if (base === "stone" && nearSpawner(x, y) && hash(x, y, 10 + z) < 0.6) return "moss";
  return base;
}

function open(x: number, y: number, z: number): boolean {
  if (z < 0 || y > WORLD.yMax) return true;
  if (z >= WORLD.depth || y < WORLD.yMin || x < WORLD.xMin || x > WORLD.xMax) return false;
  const block = blockAt(x, y, z);
  return block === null || block === "water";
}

function exposed(x: number, y: number, z: number): boolean {
  return (
    open(x, y, z - 1) || open(x, y + 1, z) || open(x, y - 1, z) || open(x - 1, y, z) || open(x + 1, y, z) || open(x, y, z + 1)
  );
}

/** Every visible block, grouped by type for instanced rendering. */
export function generateTerrain(): BlockBatch[] {
  const cells = new Map<BlockType, number[]>();
  for (let y = WORLD.yMax; y >= WORLD.yMin; y -= 1) {
    for (let x = WORLD.xMin; x <= WORLD.xMax; x += 1) {
      for (let z = 0; z < WORLD.depth; z += 1) {
        const type = blockAt(x, y, z);
        if (!type) continue;
        // Water only shows its own surface and front, never the faces between water blocks.
        if (type === "water" ? z > 0 && blockAt(x, y + 1, z) === "water" : !exposed(x, y, z)) continue;
        const depthShade = z === 0 ? 1 : z === 1 ? 0.74 : 0.52;
        let list = cells.get(type);
        if (!list) cells.set(type, (list = []));
        list.push(x, y, -z, depthShade * (0.9 + hash(x, y, 20 + z) * 0.2));
      }
    }
  }

  return BLOCK_TYPES.flatMap((type) => {
    const list = cells.get(type);
    if (!list) return [];
    const count = list.length / 4;
    const positions = new Float32Array(count * 3);
    const shades = new Float32Array(count);
    for (let index = 0; index < count; index += 1) {
      positions[index * 3] = list[index * 4];
      positions[index * 3 + 1] = list[index * 4 + 1];
      positions[index * 3 + 2] = list[index * 4 + 2];
      shades[index] = list[index * 4 + 3];
    }
    return [{ type, count, positions, shades }];
  });
}
