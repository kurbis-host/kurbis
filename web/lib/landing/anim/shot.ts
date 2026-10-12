import { Matrix4, Quaternion, Vector3 } from "three";
import { Track, type TrackData } from "./track";

export interface ShotData {
  s0: number;
  s1: number;
  tracks: Record<"eye" | "target" | "up" | "roll" | "lens", TrackData>;
}

export interface CameraState {
  eye: Vector3;
  quat: Quaternion;
  /** Focal length in millimetres on a 36 mm sensor laid along the long side of the designed frame. */
  lens: number;
}

/** One continuous stretch of camera, scrubbed by the scroll. */
export class Shot {
  readonly s0: number;
  readonly s1: number;
  private readonly eye: Track;
  private readonly target: Track;
  private readonly up: Track;
  private readonly roll: Track;
  private readonly lens: Track;

  constructor(data: ShotData) {
    this.s0 = data.s0;
    this.s1 = data.s1;
    this.eye = Track.from(data.tracks.eye);
    this.target = Track.from(data.tracks.target);
    this.up = Track.from(data.tracks.up);
    this.roll = Track.from(data.tracks.roll);
    this.lens = Track.from(data.tracks.lens);
  }

  at(s: number, out: CameraState): CameraState {
    this.eye.at(s, a);
    this.target.at(s, b);
    this.up.at(s, c);
    out.eye.set(a[0], a[1], a[2]);
    lookQuat(out.eye, target.set(b[0], b[1], b[2]), hint.set(c[0], c[1], c[2]), this.roll.value(s), out.quat);
    out.lens = this.lens.value(s);
    return out;
  }
}

/** The shot that is playing at s: shots are played back to back. */
export function shotAt(shots: readonly Shot[], s: number): Shot {
  let current = shots[0];
  for (const shot of shots) if (shot.s0 <= s + 1e-9) current = shot;
  return current;
}

/**
 * Orientation looking from `eye` to `target`. `up` is a hint, so it may be any direction
 * not parallel to the view; `roll` (degrees) turns about the view axis.
 */
export function lookQuat(eye: Vector3, to: Vector3, up: Vector3, roll: number, out: Quaternion): Quaternion {
  forward.subVectors(to, eye).normalize();
  right.crossVectors(forward, up);
  if (right.lengthSq() < 1e-12) right.crossVectors(forward, north);
  right.normalize();
  above.crossVectors(right, forward);
  out.setFromRotationMatrix(basis.makeBasis(right, above, back.copy(forward).negate()));
  if (roll) out.multiply(turn.setFromAxisAngle(viewAxis, (roll * Math.PI) / 180));
  return out;
}

/**
 * Vertical field of view, in degrees, for a lens and a viewport. The frame the shot was
 * designed for (16:9 wide, 9:16 tall) is always wholly in view; other shapes see more
 * around it, never less.
 */
export function fieldOfView(lens: number, aspect: number, tall: boolean): number {
  const half = Math.atan(18 / lens); // half the angle along the designed frame's long side
  const design = tall ? 9 / 16 : 16 / 9;
  const halfHeight = tall ? Math.tan(half) : Math.tan(half) / design;
  const halfWidth = halfHeight * design;
  const fit = aspect >= design ? halfHeight : halfWidth / aspect;
  return (2 * Math.atan(fit) * 180) / Math.PI;
}

const a: number[] = [];
const b: number[] = [];
const c: number[] = [];
const target = new Vector3();
const hint = new Vector3();
const forward = new Vector3();
const right = new Vector3();
const above = new Vector3();
const back = new Vector3();
const north = new Vector3(0, 1, 0);
const viewAxis = new Vector3(0, 0, 1);
const basis = new Matrix4();
const turn = new Quaternion();
