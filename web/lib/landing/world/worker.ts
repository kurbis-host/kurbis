/**
 * The world is built off the page's own thread: some hundreds of milliseconds of work
 * that would otherwise hold up everything else the page is doing while it loads.
 *
 * A worker is one of two things. A builder takes requests one at a time, in the order
 * they come, so the valley (which the first frame needs) is done before the Nether
 * (which the last one does). A helper is handed a line to a builder and does what the
 * builder asks of it on that line: the work of a build that can be done side by side.
 */

import { flood, sunShare, type FloodJob, type Helper, type Records, type SunJob } from "./build";
import type { Levels } from "./light";
import { buildWorld, type Took } from "./load";

export interface BuildRequest {
  id: number;
  /** The grid's file, and that of a patch to lay into it first: as fetched. */
  grid: ArrayBuffer;
  patch?: ArrayBuffer;
  /** Lines to helpers, if the page has any to spare. */
  ports?: MessagePort[];
}

/** What makes a worker a helper: the line its builder will ask it on. */
export interface HelpRequest {
  help: MessagePort;
}

/** From the page to a builder: one of the helpers it was given for a build is no more. */
export interface HelperLost {
  lost: number;
  helper: number;
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
  flood?: FloodJob;
  suns?: SunJob;
}

interface Answered {
  id: number;
  levels?: Levels;
  share?: Uint8Array;
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
  const ask = (job: Omit<Asked, "id">) =>
    new Promise<Answered>((resolve, reject) => {
      if (gone) return reject(new Error("landing: a helper was lost"));
      const id = (asked += 1);
      const late = setTimeout(() => {
        waiting.delete(id);
        reject(new Error("landing: a helper did not answer"));
      }, PATIENCE);
      waiting.set(id, { resolve, reject, late });
      port.postMessage({ id, ...job } satisfies Asked);
    });
  return {
    async flood(job) {
      const { levels, took } = await ask({ flood: job });
      if (!levels) throw new Error("landing: a helper answered something else");
      return { ...levels, took };
    },
    async suns(job) {
      const { share } = await ask({ suns: job });
      if (!share) throw new Error("landing: a helper answered something else");
      return share;
    },
    drop() {
      gone = true;
      for (const job of waiting.values()) {
        clearTimeout(job.late);
        job.reject(new Error("landing: a helper was lost"));
      }
      waiting.clear();
    },
  };
}

/** As a helper: answer what is asked on the line. */
function help(port: MessagePort) {
  port.onmessage = ({ data }: MessageEvent<Asked>) => {
    try {
      if (data.flood) {
        const started = performance.now();
        const levels = flood(data.flood);
        port.postMessage({ id: data.id, levels, took: performance.now() - started } satisfies Answered, [levels.day.buffer, levels.lit.buffer]);
      } else if (data.suns) {
        const share = sunShare(data.suns);
        port.postMessage({ id: data.id, share } satisfies Answered, [share.buffer]);
      }
    } catch (error) {
      port.postMessage({ id: data.id, error: error instanceof Error ? error.message : String(error) } satisfies Answered);
    }
  };
}

/** The helpers of each build that is still owed. */
const crews = new Map<number, ReturnType<typeof helperOn>[]>();
let queue: Promise<void> = Promise.resolve();

scope.onmessage = ({ data }) => {
  if ("help" in data) return help(data.help);
  if ("lost" in data) return crews.get(data.lost)?.[data.helper]?.drop();
  const crew = (data.ports ?? []).map(helperOn);
  crews.set(data.id, crew);
  queue = queue.then(async () => {
    try {
      const { parts, took } = await buildWorld(data.grid, data.patch, crew);
      // The faces are handed over, not copied.
      const buffers = parts.flatMap(([, records]) => [records.cells.buffer, records.tiles.buffer, records.light.buffer]);
      scope.postMessage({ id: data.id, parts, took }, buffers);
    } catch (error) {
      scope.postMessage({ id: data.id, error: error instanceof Error ? error.message : String(error) });
    }
    crews.delete(data.id);
  });
};
