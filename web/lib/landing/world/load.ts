/**
 * From a grid file's address to the world built from it. This is the whole job, wherever it
 * runs: in the worker (./worker.ts), or on the page itself where there is none.
 */

import { buildWith, type Helper, type Records, type Timing } from "./build";
import { patched, readGrid, readPatch } from "./grid";

/** How long it took, in milliseconds: until the files were here, and each stage of the build. */
export type Took = Timing & { fetch: number };

/** A file of the bundle. (The server sends it through gzip and says so: what arrives here is unpacked.) */
async function file(url: string): Promise<ArrayBuffer> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`landing: ${url} answered ${response.status}`);
  return response.arrayBuffer();
}

/**
 * Every part of the world in the grid file at an address (with the patch at another laid
 * into it first, if one is given). A `helper` takes some of the work, if there is one.
 */
export async function buildWorld(gridUrl: string, patchUrl?: string, helper?: Helper): Promise<{ parts: [string, Records][]; took: Took }> {
  const began = performance.now();
  const [gridFile, patchFile] = await Promise.all([file(gridUrl), patchUrl ? file(patchUrl) : undefined]);
  let grid = readGrid(gridFile);
  if (patchFile) grid = patched(grid, readPatch(patchFile));
  const took = { fetch: performance.now() - began } as Took;
  return { parts: [...(await buildWith(grid, helper, took))], took };
}
