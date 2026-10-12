/**
 * The world is built off the page's own thread: a fifth of a second of work on a fast
 * machine, most of a second on a slow one, that would otherwise hold up everything
 * else the page is doing while it loads.
 *
 * This file is bundled by itself into public/world.worker.js (scripts/build-world-worker.mjs),
 * so that the page's HTML can start it before any of the page's own code is here
 * (lib/landing/boot.ts). It fetches its grids itself.
 *
 * A worker is one of two things. A builder takes requests one at a time, in the order
 * they come. A helper is handed a line to a builder and floods grids with light for it
 * on that line, while the builder finds their faces.
 */

import { flood, type FloodJob, type Helper, type Records } from "./build";
import type { Levels } from "./light";
import { buildWorld, type Took } from "./load";

export interface BuildRequest {
  id: number;
  /** Where the grid's file is, and that of a patch to lay into it first. */
  grid: string;
  patch?: string;
  /** A line to a helper, if the page has one to spare. */
  port?: MessagePort;
}

/** What makes a worker a helper: the line its builder will ask it on. */
export interface HelpRequest {
  help: MessagePort;
}

/** From the page to a builder: the helper it was given for a build is no more. */
export interface HelperLost {
  lost: number;
}

export interface BuildReply {
  id: number;
  parts?: [string, Records][];
  took?: Took;
  error?: string;
}

/** What a builder asks a helper, and what it hears back. */
interface Asked {
  id: number;
  flood: FloodJob;
}

interface Answered {
  id: number;
  levels?: Levels;
  /** How long the helper worked on it, in milliseconds. */
  took?: number;
  error?: string;
}

const scope = self as unknown as {
  onmessage: ((event: MessageEvent<BuildRequest | HelpRequest | HelperLost>) => void) | null;
  postMessage(reply: BuildReply, transfer?: Transferable[]): void;
};

/** A helper that is slower than this is given up on. (One that is known to be gone is not waited for at all.) */
const PATIENCE = 10000;

/** A helper at the other end of a line. `drop` gives it up: what it was asked for, and will be, fails at once. */
function helperOn(port: MessagePort): Helper & { drop(): void } {
  let asked = 0;
  let gone = false;
  const waiting = new Map<number, { resolve(answer: Answered): void; reject(error: Error): void; late: ReturnType<typeof setTimeout> }>();
  port.onmessage = ({ data }: MessageEvent<Answered>) => {
    const job = waiting.get(data.id);
    if (!job) return;
    waiting.delete(data.id);
    clearTimeout(job.late);
    if (data.error) job.reject(new Error(data.error));
    else job.resolve(data);
  };
  return {
    async flood(job) {
      const { levels, took } = await new Promise<Answered>((resolve, reject) => {
        if (gone) return reject(new Error("landing: the helper was lost"));
        const id = (asked += 1);
        const late = setTimeout(() => {
          waiting.delete(id);
          reject(new Error("landing: the helper did not answer"));
        }, PATIENCE);
        waiting.set(id, { resolve, reject, late });
        port.postMessage({ id, flood: job } satisfies Asked);
      });
      if (!levels) throw new Error("landing: the helper answered something else");
      return { ...levels, took };
    },
    drop() {
      gone = true;
      for (const job of waiting.values()) {
        clearTimeout(job.late);
        job.reject(new Error("landing: the helper was lost"));
      }
      waiting.clear();
    },
  };
}

/** As a helper: answer what is asked on the line. */
function help(port: MessagePort) {
  port.onmessage = ({ data }: MessageEvent<Asked>) => {
    try {
      const started = performance.now();
      const levels = flood(data.flood);
      port.postMessage({ id: data.id, levels, took: performance.now() - started } satisfies Answered, [levels.day.buffer, levels.lit.buffer]);
    } catch (error) {
      port.postMessage({ id: data.id, error: error instanceof Error ? error.message : String(error) } satisfies Answered);
    }
  };
}

/** The helper of each build that is still owed. */
const helpers = new Map<number, ReturnType<typeof helperOn>>();
let queue: Promise<void> = Promise.resolve();

scope.onmessage = ({ data }) => {
  if ("help" in data) return help(data.help);
  if ("lost" in data) return helpers.get(data.lost)?.drop();
  const helper = data.port ? helperOn(data.port) : undefined;
  if (helper) helpers.set(data.id, helper);
  queue = queue.then(async () => {
    try {
      const { parts, took } = await buildWorld(data.grid, data.patch, helper);
      // The faces are handed over, not copied.
      const buffers = parts.flatMap(([, records]) => [records.cells.buffer, records.tiles.buffer, records.light.buffer]);
      scope.postMessage({ id: data.id, parts, took }, buffers);
    } catch (error) {
      scope.postMessage({ id: data.id, error: error instanceof Error ? error.message : String(error) });
    }
    helpers.delete(data.id);
  });
};
