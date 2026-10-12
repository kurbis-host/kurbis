/**
 * Everything the film needs, asked for as early as the page can ask: the moment the page
 * is up. The world's faces are being built while the film's own code (the renderer, most
 * of what there is to download) is still on its way.
 *
 * Nothing here needs the renderer, so the page can afford to carry it from the start.
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
  const response = await fetch(bundleUrl(path));
  if (!response.ok) throw new Error(`landing: ${path} answered ${response.status}`);
  return response;
}

/** Start fetching (and building) what the film needs in this frame. Asking again changes nothing. */
export function begin(aspect: Aspect): Early {
  let early = begun.get(aspect);
  if (!early) {
    performance.mark("landing:asked"); // (these marks are for looking into how loading went)
    // The valley is what the first frame shows: it is asked for first, and built with all hands.
    const valley = loadWorld("valley.grid.gz", aspect === "tall" ? "dungeon.tall.patch.gz" : undefined, { helpers: spare() });
    early = {
      valley,
      // The Nether is the film's last stop: it waits for the valley, and takes nothing from the first frame.
      nether: loadWorld("nether.grid.gz", undefined, { after: valley }),
      scene: get("scene.json").then((response) => response.json() as Promise<SceneData>),
      layout: get(`layout.${aspect}.json`).then((response) => response.json() as Promise<LayoutData>),
      pack: get("small.pack").then((response) => response.arrayBuffer()),
      models: get(`models.${aspect}.glb`).then((response) => response.arrayBuffer()),
    };
    for (const promise of Object.values(early)) (promise as Promise<unknown>).catch(() => {}); // (whoever takes them hears of it)
    begun.set(aspect, early);
    // The film's own code as well: it is asked for now, not when the canvas that plays it has come and asks.
    void import("./stage").catch(() => {});
    void import("../film/film").catch(() => {});
  }
  return early;
}

/** What was begun for a frame, handed over: nothing here holds on to megabytes of faces after. */
export function take(aspect: Aspect): Early {
  const early = begin(aspect);
  begun.delete(aspect);
  return early;
}
