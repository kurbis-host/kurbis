/**
 * The pumpkin, as two actors: one for the world it digs through, one for the Nether.
 * Its journey is scrubbed by the scroll; what it does of its own at a stop is added by
 * the stops themselves.
 */

import { Object3D, Vector3 } from "three";
import { Actor, setPose } from "../anim/actor";
import { lerp, linear, smooth } from "../anim/easing";
import type { Track } from "../anim/track";
import type { Constants, Points } from "../engine/bundle";
import { digAt, railAt, railHeading, vec } from "./context";

export interface Cast {
  pumpkin: Actor;
  nether: Actor;
  /** How bright its candle burns at a scroll position (before whatever dims it), and the glimmer it leaves on the tunnel. */
  candle(s: number): number;
  back(s: number): number;
  netherCandle(s: number): number;
}

/** What the cast is made from (the film's context has all of it; a test needs no more). */
export interface CastParts {
  C: Constants;
  points: Points;
  dig: Track;
  ride: Track;
  node(name: string): Object3D;
}

const FACES = ["asleep", "awake", "blink", "peek", "whee"] as const;
const DEG = 180 / Math.PI;

function actor(parts: CastParts, name: string): Actor {
  const bodies = new Map<string, Object3D>();
  for (const face of FACES) bodies.set(face, parts.node(`${name}_${face}`));
  return new Actor(parts.node(name), parts.node(`${name}_spin`), bodies, [parts.node(`${name}_stem`)]);
}

export function makeCast(parts: CastParts): Cast {
  const { C, points, dig, ride } = parts;
  const place = points.cast;
  const [fall0, fall1] = C.FALL;
  const hero = vec(C.HERO_PUMPKIN);
  const spot = vec(C.LANDING_SPOT);
  const fallFrom = vec(place.fall_from);
  const digStart = vec(place.dig_start);
  const hole = vec(place.hole);
  const landing = vec(place.landing);
  const mid = vec(place.mid);
  const home = vec(place.home);
  const seat = vec(place.seat);
  const board = vec(place.board);
  const alight = vec(place.alight);
  const wallSpot = vec(place.wall_spot);
  const opening = vec(place.opening);
  const signSpot = vec(place.sign_spot);
  const before = vec(place.before);
  const inside = vec(place.inside);
  const { facing } = place;

  const a = actor(parts, "Kurbis");
  a.rest(0, hero, -5);
  a.hop(C.WAKE_UP, 0.31, hero, hero, 0.3, { yaw: [-5, 0], antic: 0.1, land: 0.25 }); // startled awake
  a.hop(0.31, fall0, hero, fallFrom, 0.5, { land: 0 }); // shuffles off the edge of the cloud
  a.move(fall0, fall1, (u, out) => {
    setPose(out, fallFrom, 0, 1 + 0.16 * smooth(u * 4));
    out.pos.lerp(spot, u);
    out.pos.z = lerp(fallFrom.z, spot.z, u ** 1.8);
    out.tilt[0] = -720 * u;
  });
  a.move(fall1, 0.93, (u, out) => setPose(out, spot, 6, 1 - 0.5 * (1 - u) ** 2 * Math.cos(u * 9))); // impact
  a.rest(0.93, spot, 6);
  a.hop(1.46, C.STOMP, spot, digStart, 2.6, { yaw: [6, 0], flips: 1, antic: 0.1, land: 0 });
  a.move(C.STOMP, C.DIG0, (u, out) => setPose(out, digStart, 0, 1 - 0.52 * (1 - u) ** 2 * Math.abs(Math.cos(u * 10))));

  const turns = 5; // whole turns while it digs
  a.move(C.DIG0, C.BREAK, (u, out) => {
    const far = dig.value(lerp(C.DIG0, C.BREAK, u));
    setPose(out, digAt(points, far, out.pos));
    out.tilt[1] = (360 * turns * far) / points.through;
  });
  a.move(C.BREAK, C.LAND, (u, out) => {
    setPose(out, hole, 0, 1 + 0.14 * smooth(u * 3));
    out.pos.lerp(landing, u ** 1.6);
    out.tilt[0] = -540 * u;
  });
  a.move(C.LAND, C.LAND + 0.04, (u, out) => setPose(out, landing, 14, 1 - 0.5 * (1 - u) ** 2 * Math.cos(u * 9)));
  a.hop(C.LAND + 0.04, 2.93, landing, mid, 1.2, { yaw: [14, 20] });
  a.hop(2.93, C.WAKE, mid, home, 1.3, { yaw: [20, 370], land: 0.12 });
  a.rest(C.WAKE, home, 10);

  // Into the cart, through the tunnel, and out in front of the ore wall.
  a.hop(3.57, C.BOARD, home, board, 1.5, { yaw: [10, 180], flips: 1, land: 0.1 });
  const heading = new Vector3();
  a.move(C.BOARD, C.ARRIVE, (u, out) => {
    const far = ride.value(lerp(C.BOARD, C.ARRIVE, u));
    railHeading(points, far, heading);
    setPose(out, railAt(points, far, out.pos).add(seat), Math.atan2(heading.x, -heading.y) * DEG);
    out.tilt[0] = Math.asin(Math.max(Math.min(-heading.z, 1), -1)) * DEG;
  });
  a.hop(C.ARRIVE, 4.11, alight, wallSpot, 1.5, { yaw: [90, facing], flips: 1 });
  a.rest(4.11, wallSpot, facing);
  a.hop(4.24, 4.31, wallSpot, opening, 1.6, { yaw: [facing, -180], land: 0.1 }); // well ahead of the camera, through the opening
  a.hop(4.31, 4.38, opening, signSpot, 1.5, { yaw: [-180, -100], flips: 1 });
  a.rest(4.38, signSpot, -100);
  a.hop(4.77, 4.84, signSpot, before, 0.9, { yaw: [-100, 90] });
  a.hop(4.84, C.PORTAL_LIT, before, inside, 1.1, { yaw: [90, 90], flips: 1, land: 0 }); // into the frame, and it lights
  a.appear(-1, C.PORTAL_LIT + 0.004);
  a.face(0, "asleep").face(C.WAKE_UP, "awake").face(fall0, "whee").face(fall1, "awake").face(C.DIG0, "whee").face(C.LAND, "awake");
  a.face(C.BOARD - 0.04, "whee").face(C.ARRIVE + 0.05, "awake").face(4.84, "whee");

  // The Nether: already through, hopping ahead along the bridge to the second frame.
  const n = actor(parts, "Kurbis_Nether");
  const spots = place.nether_spots.map(vec);
  n.appear(C.THROUGH - 0.02);
  n.rest(C.THROUGH - 0.02, spots[0], 180);
  const hops: [number, number][] = [[5.34, 5.46], [5.46, 5.6], [5.6, 5.74], [5.74, 5.9]]; // always well ahead of the camera
  hops.forEach(([s0, s1], i) => {
    const last = i === hops.length - 1;
    n.hop(s0, s1, spots[i], spots[i + 1], last ? 1.2 : 1.5, { yaw: last ? [180, 372] : [180, 180], flips: last ? 0 : 1 });
  });
  n.rest(5.9, spots[spots.length - 1], 12);
  n.face(0, "whee").face(5.9, "awake");

  return {
    pumpkin: a,
    nether: n,
    candle: linear([
      [C.WAKE_UP, 0], [C.WAKE_UP + 0.01, 16], [fall1, 16], [fall1 + 0.02, 40], [C.STOMP, 40], [1.66, 70], [C.BREAK, 70], [C.BREAK + 0.02, 150],
      [C.WAKE, 150], [C.WAKE + 0.1, 34], [3.57, 34], [C.BOARD, 110], [C.ARRIVE, 110], [4.12, 30], [C.PORTAL_LIT, 30], [C.PORTAL_LIT + 0.01, 0],
    ]),
    back: linear([[3.58, 0], [C.BOARD, 16], [C.ARRIVE, 16], [C.ARRIVE + 0.06, 0]]),
    netherCandle: linear([[C.THROUGH - 0.03, 0], [C.THROUGH - 0.02, 26]]),
  };
}
