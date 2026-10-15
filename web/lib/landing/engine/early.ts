/**
 * Everything the film needs, gathered in one place for the stage to take. Most of it is
 * on its way before this code is: the page's HTML asks for it (../boot.ts), and the
 * world's faces are being built while the page's code is still arriving. What the HTML
 * did not begin (after a change of frame, or coming here from another page) is begun
 * here.
 *
 * (The files named `.gz` are stored packed and sent as they are, with the server saying
 * so: the browser unpacks them on the way in, and what is read here is plain.)
 */

import { bundleUrl } from "./address";
import type { Aspect, LayoutData, SceneData } from "./bundle";
import { loadWorld, spare, type Built } from "./world";

export interface Early {
  scene: Promise<SceneData>;
  layout: Promise<LayoutData>;
  /** The small files, as one (./pack.ts), and the models (a glb). */
  pack: Promise<ArrayBuffer>;
  models: Promise<ArrayBuffer>;
  /** The valley, with what lies under it and the dungeon; and the Nether. */
  valley: Promise<Built>;
  nether: Promise<Built>;
}

const begun = new Map<Aspect, Early>();

/** Which frame the film is laid out for: the tall one on a screen that is taller than wide. */
export function aspectNow(): Aspect {
  return window.innerWidth < window.innerHeight ? "tall" : "wide";
}

async function get(path: string): Promise<Response> {
  // (The page's HTML has usually asked already: ../boot.ts. What it asked for is taken once.)
  const booted = window.__kurbis?.fetched;
  const asked = booted?.[path];
  if (booted) booted[path] = undefined;
  const response = await (asked ?? fetch(bundleUrl(path)));
  if (!response.ok) throw new Error(`landing: ${path} answered ${response.status}`);
  return response;
}

/** Start fetching (and building) what the film needs in this frame. Asking again changes nothing. */
export function begin(aspect: Aspect): Early {
  let early = begun.get(aspect);
  if (!early) {
    performance.mark("landing:asked"); // (these marks are for looking into how loading went)
    // The valley is what the first frame shows: it is asked for first, and built with a helper.
    const valley = loadWorld("valley.grid.gz", undefined, { helped: spare() }) // (both frames' worlds are one now: no patch for the tall one);
    early = {
      valley,
      // The Nether is the film's last stop: it waits for the valley, and takes nothing from the first frame.
      nether: loadWorld("nether.grid.gz", undefined, { after: valley }),
      scene: get("scene.json").then((response) => response.json() as Promise<SceneData>),
      layout: get(`layout.${aspect}.json`).then((response) => response.json() as Promise<LayoutData>),
      pack: get("small.pack.gz").then((response) => response.arrayBuffer()),
      models: get(`models.${aspect}.glb.gz`).then((response) => response.arrayBuffer()),
    };
    for (const promise of Object.values(early)) (promise as Promise<unknown>).catch(() => {}); // (whoever takes them hears of it)
    begun.set(aspect, early);
  }
  return early;
}

/** What was begun for a frame, handed over: nothing here holds on to megabytes of faces after. */
export function take(aspect: Aspect): Early {
  const early = begin(aspect);
  begun.delete(aspect);
  return early;
}
