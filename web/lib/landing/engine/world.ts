/**
 * The world's faces, built from its grid of blocks: asked for here, worked out in a
 * worker (../world), handed back as the records the renderer draws. Where a worker
 * cannot be had, the page does the work itself.
 */

import type { Records } from "../world/build";
import type { Took } from "../world/load";
import type { BuildReply, BuildRequest, HelpRequest, HelperLost } from "../world/worker";
import { bundleUrl } from "./address";

export interface Built {
  parts: Map<string, Records>;
  took: Took;
}

interface Job {
  /** The files the worker was sent, by name: to fetch again if the work has to be done here after all. */
  grid: string;
  patch?: string;
  resolve(built: Built): void;
  reject(error: Error): void;
}

let worker: Worker | null | undefined;
let asked = 0;
/** Worlds asked for and not yet here: the worker is kept for as long as there are any. */
let owed = 0;
const waiting = new Map<number, Job>();

const hire = () => new Worker(new URL("../world/worker.ts", import.meta.url), { type: "module" });

/** A file of the bundle. (The server sends it through gzip and says so: what arrives here is unpacked.) */
async function file(name: string): Promise<ArrayBuffer> {
  const response = await fetch(bundleUrl(name));
  if (!response.ok) throw new Error(`landing: ${name} answered ${response.status}`);
  return response.arrayBuffer();
}

async function here(grid: ArrayBuffer, patch?: ArrayBuffer): Promise<Built> {
  const { buildWorld } = await import("../world/load");
  const { parts, took } = await buildWorld(grid, patch);
  return { parts: new Map(parts), took };
}

function start(): Worker | null {
  try {
    const made = hire();
    made.onmessage = ({ data }: MessageEvent<BuildReply>) => {
      const job = waiting.get(data.id);
      if (!job) return;
      waiting.delete(data.id);
      if (data.parts && data.took) job.resolve({ parts: new Map(data.parts), took: data.took });
      else job.reject(new Error(data.error ?? "landing: the world could not be built"));
    };
    made.onerror = () => {
      // No worker after all (it did not load, or is not allowed): the page builds what is still owed.
      worker = null;
      made.terminate();
      for (const [id, job] of waiting) {
        waiting.delete(id);
        Promise.all([file(job.grid), job.patch ? file(job.patch) : undefined])
          .then(([grid, patch]) => here(grid, patch))
          .then(job.resolve, job.reject);
      }
    };
    return made;
  } catch {
    return null;
  }
}

/** Whether the device can spare a worker to help with a build, beside the one that builds and the page itself. */
export function spare(): boolean {
  return (navigator.hardwareConcurrency || 2) > 2;
}

/**
 * Build the world in a grid file of the bundle (with a patch laid into it first, if one
 * is named). With `helped`, one more worker takes some of the work off the one that
 * builds. With `after`, the file is not asked for until that has come to its end: for a
 * world that is not needed yet, and should take nothing from one that is.
 */
export function loadWorld(grid: string, patch?: string, { helped = false, after }: { helped?: boolean; after?: Promise<unknown> } = {}): Promise<Built> {
  const id = (asked += 1);
  owed += 1;
  // The worker is started now, and the files are asked for here: they are on their way while it starts.
  if (worker === undefined) worker = start();
  let helper: Worker | undefined;
  let port: MessagePort | undefined;
  if (worker && helped) {
    try {
      helper = hire();
      const line = new MessageChannel();
      helper.postMessage({ help: line.port2 } satisfies HelpRequest, [line.port2]);
      // (One that did not load answers nothing: the builder is told, and does its work itself.)
      helper.onerror = () => worker?.postMessage({ lost: id } satisfies HelperLost);
      port = line.port1;
    } catch {
      // (No helper, then: the builder does it all.)
    }
  }
  const build = async (): Promise<Built> => {
    if (after) await after.then(() => {}, () => {});
    const began = performance.now();
    const [gridFile, patchFile] = await Promise.all([file(grid), patch ? file(patch) : undefined]);
    const fetched = performance.now() - began;
    const builder = worker;
    const built = builder
      ? await new Promise<Built>((resolve, reject) => {
          waiting.set(id, { grid, patch, resolve, reject });
          // The files are handed over, not copied.
          builder.postMessage({ id, grid: gridFile, patch: patchFile, port } satisfies BuildRequest, [gridFile, ...(patchFile ? [patchFile] : []), ...(port ? [port] : [])]);
        })
      : await here(gridFile, patchFile);
    built.took.fetch = fetched;
    performance.mark(`landing:built:${grid.split(".")[0]}`, { detail: built.took }); // (for looking into how loading went)
    return built;
  };
  return build().finally(() => {
    helper?.terminate();
    owed -= 1;
    if (owed === 0 && worker) {
      // Nothing more is owed: let the worker, and the grids it holds, go.
      worker.terminate();
      worker = undefined;
    }
  });
}
