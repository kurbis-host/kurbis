/**
 * From a grid file's bytes to the world built from it. This is the whole job, wherever it
 * runs: in the worker (./worker.ts), or on the page itself where there is none.
 */

import { buildWith, type Helper, type Records, type Timing } from "./build";
import { patched, readGrid, readPatch } from "./grid";

/** How long it took, in milliseconds: until the files were here (said by whoever fetched them), and each stage of the build. */
export type Took = Timing & { fetch: number };

/**
 * Every part of the world in a grid file (with a patch file laid into it first, if one
 * is given). A `helper` takes some of the work, if there is one.
 */
export async function buildWorld(gridFile: ArrayBuffer, patchFile?: ArrayBuffer, helper?: Helper): Promise<{ parts: [string, Records][]; took: Took }> {
  let grid = readGrid(gridFile);
  if (patchFile) grid = patched(grid, readPatch(patchFile));
  const took = { fetch: 0 } as Took;
  return { parts: [...(await buildWith(grid, helper, took))], took };
}
