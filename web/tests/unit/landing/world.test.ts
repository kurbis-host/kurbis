/**
 * The page builds the world's faces and their light itself, from the grids of blocks the
 * art pipeline sends (only the suns' shadows come with them, ready-made). This holds
 * that building to the art pipeline's own (kurbis/export.py), to the byte: the fixture
 * carries a digest of what each part comes to there.
 */

import { createHash } from "node:crypto";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { gunzipSync } from "node:zlib";
import { describe, expect, it } from "vitest";
import { readPack } from "../../../lib/landing/engine/pack";
import { BUNDLE } from "../../../lib/landing/engine/version";
import { build, buildWith, flood, type Helper, type Records } from "../../../lib/landing/world/build";
import { patched, readGrid, readModel, readPatch } from "../../../lib/landing/world/grid";

type Digest = { count: number; sha256: string };

const bundle = join(__dirname, "../../../public/landing");
const reference = JSON.parse(readFileSync(join(__dirname, "fixtures", "world.json"), "utf8")) as Record<"wide" | "tall" | "nether", Record<string, Digest>>;

function unpacked(name: string): ArrayBuffer {
  const bytes = gunzipSync(readFileSync(join(bundle, name)));
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
}

/** What a part's faces come to, whatever order they are in (as export.digest has it). */
function digest(records: Records | undefined): Digest | null {
  if (!records) return null;
  const { count, cells, tiles, light } = records;
  const order = Array.from({ length: count }, (_, i) => i).sort(
    (a, b) => cells[a * 4] - cells[b * 4] || cells[a * 4 + 1] - cells[b * 4 + 1] || cells[a * 4 + 2] - cells[b * 4 + 2] || cells[a * 4 + 3] - cells[b * 4 + 3],
  );
  const a = new Int16Array(count * 4);
  const b = new Uint16Array(count * 2);
  const c = new Uint8Array(count * 16);
  order.forEach((from, to) => {
    a.set(cells.subarray(from * 4, from * 4 + 4), to * 4);
    b.set(tiles.subarray(from * 2, from * 2 + 2), to * 2);
    c.set(light.subarray(from * 16, from * 16 + 16), to * 16);
  });
  const sha = createHash("sha256");
  for (const part of [a, b, c]) sha.update(new Uint8Array(part.buffer, part.byteOffset, part.byteLength));
  return { count, sha256: sha.digest("hex") };
}

describe("the world, built in the page", () => {
  const valley = readGrid(unpacked("valley.grid.gz"));

  it("builds the valley as the art pipeline does, to the byte", () => {
    const parts = build(valley);
    expect(Object.keys(reference.wide).sort()).toEqual([...parts.keys()].sort());
    for (const [name, want] of Object.entries(reference.wide)) expect(digest(parts.get(name)), name).toEqual(want);
  }, 120000);

  it("builds the tall frame's world from the same grid, with its dungeon laid in", () => {
    const parts = build(patched(valley, readPatch(unpacked("dungeon.tall.patch.gz"))));
    for (const [name, want] of Object.entries(reference.tall)) expect(digest(parts.get(name)), name).toEqual(want);
    expect(reference.tall.dungeon).not.toEqual(reference.wide.dungeon);
  }, 120000);

  it("comes to the same with a helper, and when the helper lets it down", async () => {
    // (In the page the helper is a worker of its own; what it is asked and answers is the same.)
    let asked = 0;
    const helper: Helper = {
      flood: async (job) => {
        asked += 1;
        return flood(job);
      },
    };
    const helped = await buildWith(valley, helper);
    expect(asked).toBe(1);
    for (const [name, want] of Object.entries(reference.wide)) expect(digest(helped.get(name)), name).toEqual(want);
    const alone = await buildWith(valley, { flood: () => Promise.reject(new Error("gone")) });
    for (const [name, want] of Object.entries(reference.wide)) expect(digest(alone.get(name)), name).toEqual(want);
  }, 120000);

  it("takes the suns' shadows from the grid, and refuses ones that are not for its faces", () => {
    const shadows = valley.shadows;
    expect(shadows?.suns.map((sun) => sun.column)).toEqual([8, 12]);
    expect(valley.shadowBytes.length).toBe((shadows?.suns ?? []).reduce((bytes, sun) => bytes + sun.faces * 2, 0));
    // A face too few for the first sun, one too many for the second: the bytes are all there, on the wrong faces.
    const [sun, key] = shadows?.suns ?? [];
    const shifted = { ...valley, shadows: { levels: shadows?.levels ?? [], suns: [{ ...sun, faces: sun.faces - 1 }, { ...key, faces: key.faces + 1 }] } };
    expect(() => build(shifted)).toThrow("shadows are not for these faces");
  }, 120000);

  it("builds the Nether as the art pipeline does, to the byte", () => {
    const parts = build(readGrid(unpacked("nether.grid.gz")));
    for (const [name, want] of Object.entries(reference.nether)) expect(digest(parts.get(name)), name).toEqual(want);
  }, 120000);

  it("gathers a part's faces into cubes of the world, each a run of its own", () => {
    const terrain = build(valley, ["valley.terrain"]).get("valley.terrain") as Records;
    let next = 0;
    for (const [x0, y0, z0, x1, y1, z1, first, count] of terrain.chunks) {
      expect(first).toBe(next);
      next += count;
      expect([x1 - x0, y1 - y0, z1 - z0].every((span) => span >= 1 && span <= 32)).toBe(true);
      for (let i = first; i < first + count; i += 1) {
        const [x, y, z] = terrain.cells.subarray(i * 4, i * 4 + 3);
        expect(x >= x0 && x < x1 && y >= y0 && y < y1 && z >= z0 && z < z1).toBe(true);
      }
    }
    expect(next).toBe(terrain.count);
  }, 120000);

  it("has the slab's pieces as cells", () => {
    const pack = readPack(unpacked("small.pack.gz"));
    const slab = readModel(pack.bytes("slab.grid").slice().buffer);
    expect(pack.has("atlas_color.png")).toBe(true);
    expect(slab.pieces.length).toBeGreaterThan(40);
    for (const piece of slab.pieces) {
      expect(piece.name).toMatch(/^Slab_\d\d_[01]$/);
      for (let axis = 0; axis < 3; axis += 1) expect(piece.at[axis] + piece.shape[axis]).toBeLessThanOrEqual(slab.shape[axis]);
    }
    expect(slab.cells.length).toBe(slab.shape[0] * slab.shape[1] * slab.shape[2]);
  });
});

describe("the bundle", () => {
  it("goes by the version the page asks for it by", () => {
    // The page may keep these files for good, so they must not change under a version.
    // (Changed the bundle by hand? Run art/landing/scripts/export_web.py instead.)
    const files: string[] = [];
    const walk = (folder: string) => {
      for (const name of readdirSync(folder)) {
        const path = join(folder, name);
        if (statSync(path).isDirectory()) walk(path);
        else files.push(path);
      }
    };
    walk(bundle);
    const sha = createHash("sha256");
    for (const path of files.map((file) => [relative(bundle, file).split("\\").join("/"), file] as const).sort((a, b) => (a[0] < b[0] ? -1 : 1))) {
      sha.update(Buffer.concat([Buffer.from(`${path[0]}\0`), readFileSync(path[1])]));
    }
    expect(BUNDLE).toBe(sha.digest("hex").slice(0, 16));
  });
});
