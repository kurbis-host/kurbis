/**
 * In-world type: the chunky extruded face, built from strings. Bright faces, darker
 * sides, darkest underneath, as in the trailers. Also the two things that help it read:
 * a dark rim round the letters and a soft dark card behind them.
 */

import { BufferAttribute, BufferGeometry, DoubleSide, Mesh, ShaderMaterial, type IUniform } from "three";
import type { Face, Palette } from "./bundle";
import { HAZE_GLSL, LIGHT_GLSL, linearColor, type Globals } from "./shading";

export interface TypeLayout {
  leading?: number;
  tracking?: number;
  align?: "left" | "right" | "center";
}

/** Which cells of a block of lines are ink: rows top first. */
export function textMask(face: Face, lines: readonly string[], layout: TypeLayout = {}): boolean[][] {
  const tracking = layout.tracking ?? 1;
  const leading = layout.leading ?? 2;
  const masks = lines.map((line) => lineMask(face, line, tracking));
  const width = Math.max(...masks.map((mask) => mask[0].length));
  const rows: boolean[][] = [];
  masks.forEach((mask, index) => {
    const gap = width - mask[0].length;
    const left = layout.align === "left" ? 0 : layout.align === "right" ? gap : Math.floor(gap / 2);
    for (const row of mask) rows.push([...new Array<boolean>(left).fill(false), ...row, ...new Array<boolean>(gap - left).fill(false)]);
    if (index < masks.length - 1) for (let i = 0; i < leading; i += 1) rows.push(new Array<boolean>(width).fill(false));
  });
  return rows;
}

export function lineMask(face: Face, text: string, tracking = 1): boolean[][] {
  const rows: boolean[][] = Array.from({ length: face.rows }, () => []);
  [...text].forEach((character, index) => {
    const glyph = face.glyphs[character.toUpperCase()] ?? face.glyphs[character] ?? face.glyphs["?"];
    for (let r = 0; r < face.rows; r += 1) {
      for (const cell of glyph[r]) rows[r].push(cell === "#");
      if (index < text.length - 1) for (let i = 0; i < tracking; i += 1) rows[r].push(false);
    }
  });
  return rows;
}

/** Runs of true along a line of cells, as [first, one past last]. */
function runs(count: number, on: (i: number) => boolean): [number, number][] {
  const out: [number, number][] = [];
  let start = -1;
  for (let i = 0; i <= count; i += 1) {
    const here = i < count && on(i);
    if (here && start < 0) start = i;
    if (!here && start >= 0) {
      out.push([start, i]);
      start = -1;
    }
  }
  return out;
}

class Quads {
  readonly positions: number[] = [];
  readonly normals: number[] = [];
  readonly colors: number[] = [];
  readonly index: number[] = [];

  add(corners: number[][], normal: number[], color: readonly number[]) {
    const first = this.positions.length / 3;
    for (const corner of corners) {
      this.positions.push(...corner);
      this.normals.push(...normal);
      this.colors.push(color[0], color[1], color[2]);
    }
    this.index.push(first, first + 1, first + 2, first, first + 2, first + 3);
  }

  geometry(): BufferGeometry {
    const geometry = new BufferGeometry();
    geometry.setAttribute("position", new BufferAttribute(new Float32Array(this.positions), 3));
    geometry.setAttribute("normal", new BufferAttribute(new Float32Array(this.normals), 3));
    geometry.setAttribute("color", new BufferAttribute(new Float32Array(this.colors), 3));
    geometry.setIndex(this.index);
    return geometry;
  }
}

/**
 * Extruded type. Origin at the bottom centre of the front face; the front faces -Y and
 * the letters reach `depth` cells back.
 */
export function typeGeometry(mask: boolean[][], palette: Palette, cell: number, depth: number): BufferGeometry {
  const rows = mask.length;
  const cols = mask[0].length;
  const ink = (r: number, c: number) => r >= 0 && r < rows && c >= 0 && c < cols && mask[r][c];
  const x = (c: number) => (c - cols / 2) * cell;
  const z = (r: number) => (rows - r) * cell; // top edge of row r
  const back = depth * cell;
  const front = linearColor(palette.front).toArray();
  const top = linearColor(palette.top).toArray();
  const side = linearColor(palette.side).toArray();
  const bottom = linearColor(palette.bottom).toArray();
  const quads = new Quads();
  for (let r = 0; r < rows; r += 1) {
    const hi = z(r);
    const lo = z(r + 1);
    for (const [c0, c1] of runs(cols, (c) => ink(r, c))) {
      quads.add([[x(c0), 0, lo], [x(c1), 0, lo], [x(c1), 0, hi], [x(c0), 0, hi]], [0, -1, 0], front);
    }
    for (const [c0, c1] of runs(cols, (c) => ink(r, c) && !ink(r - 1, c))) {
      quads.add([[x(c0), 0, hi], [x(c1), 0, hi], [x(c1), back, hi], [x(c0), back, hi]], [0, 0, 1], top);
    }
    for (const [c0, c1] of runs(cols, (c) => ink(r, c) && !ink(r + 1, c))) {
      quads.add([[x(c0), back, lo], [x(c1), back, lo], [x(c1), 0, lo], [x(c0), 0, lo]], [0, 0, -1], bottom);
    }
  }
  for (let c = 0; c < cols; c += 1) {
    for (const [r0, r1] of runs(rows, (r) => ink(r, c) && !ink(r, c - 1))) {
      quads.add([[x(c), back, z(r1)], [x(c), 0, z(r1)], [x(c), 0, z(r0)], [x(c), back, z(r0)]], [-1, 0, 0], side);
    }
    for (const [r0, r1] of runs(rows, (r) => ink(r, c) && !ink(r, c + 1))) {
      quads.add([[x(c + 1), 0, z(r1)], [x(c + 1), back, z(r1)], [x(c + 1), back, z(r0)], [x(c + 1), 0, z(r0)]], [1, 0, 0], side);
    }
  }
  return quads.geometry();
}

/** A thin dark rim: every run of cells grown by `width` cells all round, just behind the faces. */
export function rimGeometry(mask: boolean[][], cell: number, width: number, color: readonly number[]): BufferGeometry {
  const rows = mask.length;
  const cols = mask[0].length;
  const quads = new Quads();
  const y = 0.06 * cell;
  for (let r = 0; r < rows; r += 1) {
    const lo = (rows - r - 1 - width) * cell;
    const hi = (rows - r + width) * cell;
    for (const [c0, c1] of runs(cols, (c) => mask[r][c])) {
      const x0 = (c0 - width - cols / 2) * cell;
      const x1 = (c1 + width - cols / 2) * cell;
      quads.add([[x0, y, lo], [x1, y, lo], [x1, y, hi], [x0, y, hi]], [0, -1, 0], color);
    }
  }
  return quads.geometry();
}

const TYPE_VERTEX = /* glsl */ `
in vec3 color;
out vec3 vColor;
out vec3 vWorld;
out vec3 vNormal;
void main() {
  vec4 world = modelMatrix * vec4(position, 1.0);
  vWorld = world.xyz;
  vNormal = normalize(mat3(modelMatrix) * normal);
  vColor = color;
  gl_Position = projectionMatrix * viewMatrix * world;
}
`;

const TYPE_FRAGMENT = /* glsl */ `
${LIGHT_GLSL}
${HAZE_GLSL}
uniform float uSelf;
uniform float uDay;
in vec3 vColor;
in vec3 vWorld;
in vec3 vNormal;
void main() {
  vec3 color = vColor * (uSelf + lightOn(normalize(vNormal), vec4(uDay, 0.0, uDay, uDay), vWorld));
  gl_FragColor = vec4(seenFrom(color, vWorld), 1.0);
}
`;

/**
 * Type keeps some light of its own (`self`), so it stays graphic in a dark place.
 * `day` is how much daylight reaches it: 1 in the open, 0 underground.
 */
export function typeMaterial(globals: Globals, self: number, day: number): ShaderMaterial {
  const uniforms: Record<string, IUniform> = { ...globals, uSelf: { value: self }, uDay: { value: day } };
  return new ShaderMaterial({ uniforms, vertexShader: TYPE_VERTEX, fragmentShader: TYPE_FRAGMENT });
}

const SCRIM_VERTEX = /* glsl */ `
out vec2 vUv;
void main() {
  vUv = uv;
  gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
}
`;

const SCRIM_FRAGMENT = /* glsl */ `
uniform vec2 uSoft;
uniform float uStrength;
in vec2 vUv;
float edges(float u, float share) {
  return smoothstep(0.0, share, u) * smoothstep(1.0, 1.0 - share, u);
}
void main() {
  gl_FragColor = vec4(0.0, 0.0, 0.0, edges(vUv.x, uSoft.x) * edges(vUv.y, uSoft.y) * uStrength);
}
`;

/**
 * A soft dark card to sit behind a block of type. `width` and `height` are the type's
 * own size; the card reaches `feather` further on every side. Faces -Y, centred.
 */
export function scrimMesh(width: number, height: number, feather: number, strength: number): Mesh {
  const w = width / 2 + feather;
  const h = height / 2 + feather;
  const geometry = new BufferGeometry();
  geometry.setAttribute("position", new BufferAttribute(new Float32Array([-w, 0, -h, w, 0, -h, w, 0, h, -w, 0, h]), 3));
  geometry.setAttribute("uv", new BufferAttribute(new Float32Array([0, 0, 1, 0, 1, 1, 0, 1]), 2));
  geometry.setIndex([0, 1, 2, 0, 2, 3]);
  const material = new ShaderMaterial({
    uniforms: {
      uSoft: { value: [Math.min((1.3 * feather) / (2 * w), 0.5), Math.min((1.3 * feather) / (2 * h), 0.5)] },
      uStrength: { value: strength },
    },
    vertexShader: SCRIM_VERTEX,
    fragmentShader: SCRIM_FRAGMENT,
    transparent: true,
    depthWrite: false,
    side: DoubleSide,
  });
  return new Mesh(geometry, material);
}
