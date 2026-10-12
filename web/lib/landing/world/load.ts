/**
 * From a grid file's bytes to the world built from it. This is the whole job, wherever it
 * runs: in the worker (./worker.ts), or on the page itself where there is none.
 */

import { buildWith, type Helper, type Records, type Timing } from "./build";
import { patched, readGrid, readPatch } from "./grid";

/**
 * How long it took, in milliseconds: until the files were here (said by whoever fetched
 * them), their unpacking, and each stage of the build.
 */
export type Took = Timing & { fetch: number; unpack: number };

/**
 * Bytes as they were before gzip packed them. Grids are written packed, because a server
 * compresses the text it sends but not bytes like these; plain ones are taken as they
 * come (something on the way may have unpacked them already).
 */
export async function gunzipped(bytes: ArrayBuffer | Uint8Array): Promise<ArrayBuffer> {
  const view = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  if (view[0] !== 0x1f || view[1] !== 0x8b) return view.buffer.slice(view.byteOffset, view.byteOffset + view.byteLength) as ArrayBuffer;
  return new Response(new Blob([view as Uint8Array<ArrayBuffer>]).stream().pipeThrough(new DecompressionStream("gzip"))).arrayBuffer();
}

/**
 * Every part of the world in a grid file (with a patch file laid into it first, if one
 * is given). `helpers` take some of the work, if there are any.
 */
export async function buildWorld(gridFile: ArrayBuffer, patchFile?: ArrayBuffer, helpers: readonly Helper[] = []): Promise<{ parts: [string, Records][]; took: Took }> {
  const started = performance.now();
  const [gridBytes, patchBytes] = await Promise.all([gunzipped(gridFile), patchFile ? gunzipped(patchFile) : null]);
  let grid = readGrid(gridBytes);
  if (patchBytes) grid = patched(grid, readPatch(patchBytes));
  const took = { fetch: 0, unpack: performance.now() - started } as Took;
  return { parts: [...(await buildWith(grid, helpers, took))], took };
}
