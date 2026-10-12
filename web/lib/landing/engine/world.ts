/**
 * The world's faces, built from its grid of blocks: asked for here, worked out in a
 * worker (../world), handed back as the records the renderer draws. Where a worker
 * cannot be had, the page does the work itself.
 *
 * The first world is usually on its way before this code is: the page's HTML starts
 * the worker itself (../boot.ts), and what it started is taken over here.
 */

import type { Records } from "../world/build";
import type { Took } from "../world/load";
import type { BuildReply, BuildRequest, HelpRequest, HelperLost } from "../world/worker";
import { bundleUrl } from "./address";

export interface Built {
  parts: Map<string, Records>;
  took: Took;
}

/** What the page's HTML began before any of this was here (see ../boot.ts, which writes it). */
export interface Booted {
  /** The worker building the first world, the one helping it, and what was asked of them. */
  worker?: Worker;
  helper?: Worker;
  grid?: string;
  patch?: string;
  /** What the worker has said since, and whether it broke. */
  replies: BuildReply[];
  failed?: boolean;
  /** Files of the bundle already asked for, by name. */
  fetched: Record<string, Promise<Response> | undefined>;
}

declare global {
  interface Window {
    __kurbis?: Booted;
  }
}

interface Job {
  grid: string;
  patch?: string;
  resolve(built: Built): void;
  reject(error: Error): void;
}

let workerUrl: string | undefined;
let worker: Worker | null | undefined;
let asked = 0;
/** Worlds asked for and not yet here: the worker is kept for as long as there are any. */
let owed = 0;
const waiting = new Map<number, Job>();

/** Where the worker's script is (the page is told by the server, which knows its version). */
export function workAt(url: string) {
  workerUrl = url;
}

async function here(grid: string, patch?: string): Promise<Built> {
  const { buildWorld } = await import("../world/load");
  const { parts, took } = await buildWorld(grid, patch);
  return { parts: new Map(parts), took };
}

function hear(data: BuildReply) {
  const job = waiting.get(data.id);
  if (!job) return;
  waiting.delete(data.id);
  if (data.parts && data.took) job.resolve({ parts: new Map(data.parts), took: data.took });
  else job.reject(new Error(data.error ?? "landing: the world could not be built"));
}

/** No worker after all (it did not load, or is not allowed): the page builds what is still owed. */
function broke(made: Worker) {
  worker = null;
  made.terminate();
  for (const [id, job] of waiting) {
    waiting.delete(id);
    here(job.grid, job.patch).then(job.resolve, job.reject);
  }
}

function wire(made: Worker): Worker {
  made.onmessage = ({ data }: MessageEvent<BuildReply>) => hear(data);
  made.onerror = () => broke(made);
  return made;
}

function start(): Worker | null {
  try {
    return workerUrl ? wire(new Worker(workerUrl)) : null;
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
 * builds. With `after`, the world is not begun until that has come to its end: for one
 * that is not needed yet, and should take nothing from one that is.
 */
export function loadWorld(grid: string, patch?: string, { helped = false, after }: { helped?: boolean; after?: Promise<unknown> } = {}): Promise<Built> {
  const id = (asked += 1);
  owed += 1;
  const request: BuildRequest = { id, grid: bundleUrl(grid), patch: patch ? bundleUrl(patch) : undefined };
  let helper: Worker | undefined;
  const booted = window.__kurbis;
  // What the page's HTML began already, if it is this world: its worker is this one's from here on.
  const begun = booted?.worker && booted.grid === request.grid && booted.patch === request.patch && worker === undefined ? booted : undefined;
  if (begun) {
    worker = wire(begun.worker as Worker);
    helper = begun.helper;
    begun.worker = undefined;
    begun.helper = undefined;
    request.id = 0; // (the number it was asked under)
  } else if (worker === undefined) worker = start();
  const build = async (): Promise<Built> => {
    if (after) await after.then(() => {}, () => {});
    const builder = worker;
    if (!builder) return here(request.grid, request.patch);
    return new Promise<Built>((resolve, reject) => {
      waiting.set(request.id, { grid: request.grid, patch: request.patch, resolve, reject });
      if (begun) {
        for (const reply of begun.replies) hear(reply);
        if (begun.failed) broke(builder);
        return;
      }
      let port: MessagePort | undefined;
      if (helped && workerUrl) {
        try {
          helper = new Worker(workerUrl);
          const line = new MessageChannel();
          helper.postMessage({ help: line.port2 } satisfies HelpRequest, [line.port2]);
          // (One that did not load answers nothing: the builder is told, and does its work itself.)
          helper.onerror = () => worker?.postMessage({ lost: id } satisfies HelperLost);
          port = line.port1;
        } catch {
          // (No helper, then: the builder does it all.)
        }
      }
      builder.postMessage({ ...request, port } satisfies BuildRequest, port ? [port] : []);
    });
  };
  return build()
    .then((built) => {
      performance.mark(`landing:built:${grid.split(".")[0]}`, { detail: built.took }); // (for looking into how loading went)
      return built;
    })
    .finally(() => {
      helper?.terminate();
      owed -= 1;
      if (owed === 0 && worker) {
        // Nothing more is owed: let the worker, and the grids it holds, go.
        worker.terminate();
        worker = undefined;
      }
    });
}
