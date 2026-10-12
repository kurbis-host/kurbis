/**
 * The page's own timeline against the previs it was ported from: the camera and the
 * pumpkin, sampled along the whole scroll in both frame shapes. The fixtures are written
 * by the art pipeline (art/landing/scripts/export_web.py) and hold only numbers.
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { Object3D, Quaternion, Vector3 } from "three";
import { describe, expect, it } from "vitest";
import { Shot, shotAt, type CameraState, type ShotData } from "../../../lib/landing/anim/shot";
import { Track, type TrackData } from "../../../lib/landing/anim/track";
import type { Constants, Points } from "../../../lib/landing/engine/bundle";
import { makeCast } from "../../../lib/landing/film/cast";

interface Reference {
  step: number;
  /** s, eye xyz, quaternion wxyz, lens */
  camera: number[][];
  /** s, position xyz, yaw, tilt xyz, squash, grow, calm */
  actors: { pumpkin: number[][]; nether_pumpkin: number[][] };
  constants: Constants;
  points: Points;
  tracks: { dig: TrackData; ride: TrackData };
  shots: ShotData[];
}

function load(aspect: string): Reference {
  return JSON.parse(readFileSync(join(__dirname, "fixtures", `reference.${aspect}.json`), "utf8")) as Reference;
}

describe.each(["wide", "tall"])("the %s film against the previs", (aspect) => {
  const ref = load(aspect);

  it("puts the camera where the previs has it", () => {
    const shots = ref.shots.map((shot) => new Shot(shot));
    const state: CameraState = { eye: new Vector3(), quat: new Quaternion(), lens: 0 };
    const want = new Quaternion();
    let worst = 0;
    for (const [s, x, y, z, qw, qx, qy, qz, lens] of ref.camera) {
      shotAt(shots, s).at(s, state);
      worst = Math.max(worst, state.eye.distanceTo(new Vector3(x, y, z)));
      // (The fixture rounds to five places, so its quaternion is not quite of length one.)
      expect(Math.abs(state.quat.dot(want.set(qx, qy, qz, qw).normalize())), `turn at ${s}`).toBeGreaterThan(1 - 1e-8);
      expect(state.lens, `lens at ${s}`).toBeCloseTo(lens, 3);
    }
    expect(worst).toBeLessThan(2e-3);
  });

  it("moves the pumpkin as the previs does", () => {
    const things = new Map<string, Object3D>();
    const node = (name: string) => {
      let thing = things.get(name);
      if (!thing) {
        thing = new Object3D();
        things.set(name, thing);
      }
      return thing;
    };
    const cast = makeCast({ C: ref.constants, points: ref.points, dig: Track.from(ref.tracks.dig), ride: Track.from(ref.tracks.ride), node });
    for (const [actor, rows] of [[cast.pumpkin, ref.actors.pumpkin], [cast.nether, ref.actors.nether_pumpkin]] as const) {
      for (const [s, x, y, z, yaw, tx, ty, tz, squash, grow, calm] of rows) {
        const pose = actor.state(s);
        const at = `at ${s}`;
        expect(pose.pos.distanceTo(new Vector3(x, y, z)), `place ${at}`).toBeLessThan(2e-3);
        expect(pose.yaw, `yaw ${at}`).toBeCloseTo(yaw, 2);
        expect(pose.tilt[0], `tilt x ${at}`).toBeCloseTo(tx, 2);
        expect(pose.tilt[1], `tilt y ${at}`).toBeCloseTo(ty, 2);
        expect(pose.tilt[2], `tilt z ${at}`).toBeCloseTo(tz, 2);
        expect(pose.squash, `squash ${at}`).toBeCloseTo(squash, 4);
        expect(pose.grow, `grow ${at}`).toBeCloseTo(grow, 4);
        expect(pose.calm ? 1 : 0, `calm ${at}`).toBe(calm);
      }
    }
  });
});
