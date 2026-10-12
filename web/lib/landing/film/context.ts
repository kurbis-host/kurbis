/** What every part of the film is handed: the stage, the timeline, and the art's numbers. */

import { Object3D, Vector3 } from "three";
import type { Timeline } from "../anim/timeline";
import { Track } from "../anim/track";
import type { Constants, Points } from "../engine/bundle";
import type { Stage } from "../engine/stage";
import type { Cast } from "./cast";
import type { Interface } from "./interface";
import type { Touch } from "./touch";

export interface Ctx {
  stage: Stage;
  tl: Timeline;
  C: Constants;
  points: Points;
  tall: boolean;
  /** How far along the dig (in steps) and along the rails (in blocks) the story is at a scroll position. */
  dig: Track;
  ride: Track;
  /** A thing of the set by its name. `node` insists it is there; `find` does not. */
  node(name: string): Object3D;
  find(name: string): Object3D | null;
  /** This for the wide frame, that for the tall one. */
  pick<T>(wide: T, tall: T): T;
  /** The interface over the film, once its pictures are in. */
  ui: Interface | null;
  /** The pumpkin, for the stops where it answers the visitor. */
  cast: Cast;
  /** The visitor's hand. */
  touch: Touch;
}

export function vec(v: readonly number[]): Vector3 {
  return new Vector3(v[0], v[1], v[2]);
}

export function makeContext(stage: Stage, tl: Timeline, touch: Touch, makeCast: (ctx: Ctx) => Cast): Ctx {
  const missing = new Set<string>();
  const ctx: Ctx = {
    stage,
    tl,
    C: stage.data.constants,
    points: stage.layout.points,
    tall: stage.tall,
    dig: Track.from(stage.layout.tracks.dig),
    ride: Track.from(stage.layout.tracks.ride),
    node(name) {
      const found = stage.nodes.get(name);
      if (found) return found;
      // A thing the art no longer has must not take the page down with it.
      if (!missing.has(name)) {
        missing.add(name);
        console.warn(`landing: nothing called ${name}`);
      }
      const stand = new Object3D();
      stand.name = name;
      stage.nodes.set(name, stand);
      return stand;
    },
    find: (name) => stage.nodes.get(name) ?? null,
    pick: (wide, tall) => (stage.tall ? tall : wide),
    ui: null,
    touch,
    cast: null as unknown as Cast,
  };
  ctx.cast = makeCast(ctx);
  return ctx;
}

// -- the rails and the dig, as the art lays them -----------------------------------
/** Where the rails are at a distance along them. */
export function railAt(points: Points, d: number, out = new Vector3()): Vector3 {
  const [pts, far] = points.rail;
  const along = Math.min(Math.max(d, 0), far[far.length - 1]);
  let i = 0;
  while (i < pts.length - 2 && far[i + 1] <= along) i += 1;
  const u = (along - far[i]) / (far[i + 1] - far[i]);
  return out.set(pts[i][0] + (pts[i + 1][0] - pts[i][0]) * u, pts[i][1] + (pts[i + 1][1] - pts[i][1]) * u, pts[i][2] + (pts[i + 1][2] - pts[i][2]) * u);
}

const ahead = new Vector3();
const behind = new Vector3();

/** The way the rails run there. */
export function railHeading(points: Points, d: number, out = new Vector3(), reach = 0.35): Vector3 {
  return out.copy(railAt(points, d + reach, ahead)).sub(railAt(points, d - reach, behind)).normalize();
}

/** Where the dig has got to after `i` steps (a fraction is part of a step). */
export function digAt(points: Points, i: number, out = new Vector3()): Vector3 {
  const { steps } = points;
  const k = Math.min(Math.floor(i), steps.length - 2);
  const u = i - k;
  return out.set(steps[k][0] + (steps[k + 1][0] - steps[k][0]) * u, 1, steps[k][1] + (steps[k + 1][1] - steps[k][1]) * u);
}
