/**
 * A bendable limb, as the trailers have them: one piece from shoulder to hand (hip to
 * foot), which bends at the elbow (knee) along a short arc, so the joint stays whole and
 * round instead of two boxes hinged with a gap.
 *
 * The box is cut into slices along its length; each frame, every slice is placed along
 * the bent centre line. Unbent it is exactly the art's box, with the skin laid on it the
 * way the art lays it (kurbis/boxmodel.py: BoxMesh.box), so it reads as the same model.
 *
 * Units: the art's pixels (a sixteenth of a block). The limb hangs from its pivot along
 * -Z, its front toward -Y; `s` is the distance down from the pivot.
 */

import { BufferAttribute, BufferGeometry } from "three";

/** One box of the skin: where it is (pixels, relative to the pivot), its size, and where its unwrap starts on the skin. */
export interface LimbBox {
  lo: readonly [number, number, number];
  size: readonly [number, number, number];
  uv: readonly [number, number];
  /** How far it is grown all round (the second layer of a skin: sleeves, trousers). */
  inflate: number;
}

/** The skin is 64 by 64 pixels. */
const SKIN = 64;
/** Slices along the length: enough for a round bend. */
const SLICES = 24;
/** The art's six faces, in its order: +X, -X, +Y, -Y, +Z, -Z, each as the unit cube's corners (kurbis/voxel.py: QUADS). */
const QUADS = [
  [[1, 0, 0], [1, 1, 0], [1, 1, 1], [1, 0, 1]],
  [[0, 1, 0], [0, 0, 0], [0, 0, 1], [0, 1, 1]],
  [[1, 1, 0], [0, 1, 0], [0, 1, 1], [1, 1, 1]],
  [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1]],
  [[0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]],
  [[0, 1, 0], [1, 1, 0], [1, 0, 0], [0, 0, 0]],
] as const;
const NORMALS = [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]] as const;

/** Where each face's picture is on the skin: [u, v, width, height] in pixels from the top left (the standard unwrap). */
function regions(size: readonly [number, number, number], uv: readonly [number, number]): number[][] {
  const [w, d, h] = size;
  const [u, v] = uv;
  return [
    [u + d + w, v + d, d, h], // +X: its left side
    [u, v + d, d, h], // -X: its right side
    [u + d + w + d, v + d, w, h], // +Y: its back
    [u + d, v + d, w, h], // -Y: its front
    [u + d, v, w, d], // +Z: the top
    [u + d + w, v, w, d], // -Z: the bottom
  ];
}

/** Rest positions (pixels), their distance down the limb, normals, and uvs: what bending starts from. */
interface Rest {
  x: Float32Array;
  y: Float32Array;
  s: Float32Array;
  nx: Float32Array;
  ny: Float32Array;
  nz: Float32Array;
}

export class Limb {
  readonly geometry = new BufferGeometry();
  private readonly rest: Rest;
  private readonly position: BufferAttribute;
  private readonly normal: BufferAttribute;
  private bent = NaN;

  /**
   * `joint` is how far down from the pivot the elbow is, `reach` half the length of
   * the bend's arc (pixels): longer is rounder. `scale` turns pixels into the world's units.
   */
  constructor(
    boxes: readonly LimbBox[],
    private readonly joint: number,
    private readonly reach: number,
    private readonly scale: number,
  ) {
    const xs: number[] = [];
    const ys: number[] = [];
    const ss: number[] = [];
    const ns: number[][] = [];
    const uvs: number[] = [];
    const index: number[] = [];
    for (const box of boxes) {
      const [lx, ly, lz] = box.lo.map((v) => v - box.inflate);
      const [sx, sy, sz] = box.size.map((v) => v + 2 * box.inflate);
      const faces = regions(box.size, box.uv);
      QUADS.forEach((quad, k) => {
        const [u, v, w, h] = faces[k];
        // Pixel uv of each corner, in the art's order: (left, bottom), (right, bottom), (right, top), (left, top).
        const corner = [[u, v + h], [u + w, v + h], [u + w, v], [u, v]];
        const side = k < 4; // the sides run the length of the limb, and are sliced; the ends are not
        const rows = side ? SLICES : 1;
        const first = xs.length;
        for (let r = 0; r <= rows; r += 1) {
          const t = r / rows; // 0 at the corners' z = 0 edge, 1 at z = 1 (sides only)
          for (const c of side ? [0, 1] : [0, 1, 2, 3]) {
            // On a side, corners 0 and 1 are the bottom edge, 3 and 2 the top: blend down the slice.
            const a = side ? QUADS[k][c] : quad[c];
            const b = side ? QUADS[k][3 - c] : quad[c];
            const cx = side ? a[0] + (b[0] - a[0]) * t : a[0];
            const cy = side ? a[1] + (b[1] - a[1]) * t : a[1];
            const cz = side ? t : a[2];
            xs.push(lx + cx * sx);
            ys.push(ly + cy * sy);
            ss.push(-(lz + cz * sz));
            ns.push([...NORMALS[k]]);
            const ua = corner[c];
            const ub = side ? corner[3 - c] : corner[c];
            uvs.push((ua[0] + (ub[0] - ua[0]) * t) / SKIN, (ua[1] + (ub[1] - ua[1]) * t) / SKIN);
          }
          if (side && r > 0) {
            const at = first + (r - 1) * 2;
            index.push(at, at + 1, at + 3, at, at + 3, at + 2);
          }
        }
        if (!side) index.push(first, first + 1, first + 2, first, first + 2, first + 3);
      });
    }
    const count = xs.length;
    this.rest = {
      x: Float32Array.from(xs),
      y: Float32Array.from(ys),
      s: Float32Array.from(ss),
      nx: Float32Array.from(ns.map((n) => n[0])),
      ny: Float32Array.from(ns.map((n) => n[1])),
      nz: Float32Array.from(ns.map((n) => n[2])),
    };
    this.position = new BufferAttribute(new Float32Array(count * 3), 3);
    this.normal = new BufferAttribute(new Float32Array(count * 3), 3);
    this.geometry.setAttribute("position", this.position);
    this.geometry.setAttribute("normal", this.normal);
    this.geometry.setAttribute("uv", new BufferAttribute(Float32Array.from(uvs), 2));
    this.geometry.setIndex(index);
    this.bend(0);
  }

  /**
   * Bend the joint by `angle` (radians): positive brings the lower part forward (-Y), as
   * an elbow does; negative takes it back, as a knee does. Returns where the lower part
   * is, for what it holds: its origin (at the joint, in pixels from the pivot) and its angle.
   */
  bend(angle: number) {
    if (angle === this.bent) return;
    this.bent = angle;
    const { x, y, s, nx, ny, nz } = this.rest;
    const pos = this.position.array as Float32Array;
    const nor = this.normal.array as Float32Array;
    const s0 = this.joint - this.reach;
    const s1 = this.joint + this.reach;
    const straight = Math.abs(angle) < 1e-4;
    const radius = straight ? 0 : (2 * this.reach) / angle;
    for (let i = 0; i < s.length; i += 1) {
      const along = s[i];
      // How far the slice has turned, and where the centre line is there.
      let a = 0;
      let py = 0;
      let pz = -along;
      if (!straight && along > s0) {
        const f = Math.min((along - s0) / (s1 - s0), 1);
        a = angle * f;
        py = radius * (Math.cos(a) - 1);
        pz = -s0 - radius * Math.sin(a);
        if (along > s1) {
          const beyond = along - s1;
          py -= Math.sin(angle) * beyond;
          pz -= Math.cos(angle) * beyond;
        }
      }
      const c = Math.cos(a);
      const sn = Math.sin(a);
      // The cross-section turns with the centre line (a turn about X by -a).
      pos[i * 3] = x[i] * this.scale;
      pos[i * 3 + 1] = (py + y[i] * c) * this.scale;
      pos[i * 3 + 2] = (pz - y[i] * sn) * this.scale;
      nor[i * 3] = nx[i];
      nor[i * 3 + 1] = ny[i] * c + nz[i] * sn;
      nor[i * 3 + 2] = -ny[i] * sn + nz[i] * c;
    }
    this.position.needsUpdate = true;
    this.normal.needsUpdate = true;
    this.geometry.computeBoundingSphere();
  }

  /** Where the lower part's frame is when bent by `angle`: its origin at the joint (world units, from the pivot) and its turn about X. */
  lower(angle: number): { y: number; z: number; turn: number } {
    const s0 = this.joint - this.reach;
    if (Math.abs(angle) < 1e-4) return { y: 0, z: -this.joint * this.scale, turn: 0 };
    const radius = (2 * this.reach) / angle;
    const endY = radius * (Math.cos(angle) - 1);
    const endZ = -s0 - radius * Math.sin(angle);
    // The joint's own place on the lower part: `reach` back up from the end of the arc.
    return { y: (endY + Math.sin(angle) * this.reach) * this.scale, z: (endZ + Math.cos(angle) * this.reach) * this.scale, turn: -angle };
  }
}
