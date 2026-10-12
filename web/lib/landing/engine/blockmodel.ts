/**
 * Small things made of blocks (the slab that falls away from the cut face), built from
 * their cells as the art's mesher builds them: every face that shows, mapped onto the
 * atlas. Each piece is its own closed mesh, so it can fall by itself.
 */

import { BufferAttribute, BufferGeometry } from "three";
import type { BlockModel } from "../world/grid";
import { DIRS, QUADS } from "../world/light";

/** The mesh of one piece of a model, its vertices counted from the piece's `centre`. */
export function pieceGeometry(model: BlockModel, piece: BlockModel["pieces"][number], atlasAcross: number, tile: number): BufferGeometry {
  const [, ny, nz] = model.shape;
  const [ax, ay, az] = piece.at;
  const [sx, sy, sz] = piece.shape;
  const { tiles, opaque, group } = model.blocks;
  const inset = Math.fround(0.02 / (atlasAcross * tile));
  const at = (x: number, y: number, z: number) => (x < 0 || y < 0 || z < 0 || x >= sx || y >= sy || z >= sz ? 0 : model.cells[((ax + x) * ny + ay + y) * nz + az + z]);
  const positions: number[] = [];
  const normals: number[] = [];
  const uvs: number[] = [];
  const index: number[] = [];
  const offset = [model.origin[0] + ax - piece.centre[0], model.origin[1] + ay - piece.centre[1], model.origin[2] + az - piece.centre[2]];
  for (let x = 0; x < sx; x += 1) {
    for (let y = 0; y < sy; y += 1) {
      for (let z = 0; z < sz; z += 1) {
        const id = at(x, y, z);
        if (!id || group[id] !== "opaque") continue;
        for (let side = 0; side < 6; side += 1) {
          const [dx, dy, dz] = DIRS[side];
          const next = at(x + dx, y + dy, z + dz); // outside the piece there is nothing: it is a solid of its own
          if (opaque[next] || next === id) continue;
          const which = tiles[id][side];
          const column = which % atlasAcross;
          const row = Math.floor(which / atlasAcross);
          const u0 = column / atlasAcross + inset;
          const u1 = (column + 1) / atlasAcross - inset;
          const v0 = row / atlasAcross + inset;
          const v1 = (row + 1) / atlasAcross - inset;
          const first = positions.length / 3;
          QUADS[side].forEach((corner, j) => {
            positions.push(x + corner[0] + offset[0], y + corner[1] + offset[1], z + corner[2] + offset[2]);
            normals.push(dx, dy, dz);
            // (As a glb has it: v counted from the top. The models' shader turns it back.)
            uvs.push(j === 0 || j === 3 ? u0 : u1, 1 - (j < 2 ? v0 : v1));
          });
          index.push(first, first + 1, first + 2, first, first + 2, first + 3);
        }
      }
    }
  }
  const geometry = new BufferGeometry();
  geometry.setAttribute("position", new BufferAttribute(new Float32Array(positions), 3));
  geometry.setAttribute("normal", new BufferAttribute(new Float32Array(normals), 3));
  geometry.setAttribute("uv", new BufferAttribute(new Float32Array(uvs), 2));
  geometry.setIndex(index);
  return geometry;
}
