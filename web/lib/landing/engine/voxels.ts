/**
 * The big voxel meshes. The records the page builds from a grid of blocks (../world)
 * are drawn as they come: one instanced quad per block face, the vertex shader placing
 * its four corners from the face's cell and direction. Faces are grouped by cube of the
 * world, one mesh a cube, so the frustum culls them.
 */

import {
  BufferAttribute,
  Group,
  InstancedBufferAttribute,
  InstancedBufferGeometry,
  InstancedInterleavedBuffer,
  InterleavedBufferAttribute,
  Mesh,
  ShaderMaterial,
  Sphere,
  Vector3,
  type IUniform,
} from "three";
import type { Records } from "./bundle";
import { HAZE_GLSL, LIGHT_GLSL, linearColor, type Globals } from "./shading";

export interface VoxelLook {
  /** How strongly the tiles' own glow shows (the emission atlas times this). */
  emit: number;
  /** Pixels clearer than this are cut away (leaves); 0 for solid blocks. */
  cut?: number;
  /** Sway of leaves in the wind, in blocks; 0 for none. */
  wind?: number;
  /** Seen through, at this opacity (water). */
  alpha?: number;
  /** Swell of a water surface: [level, height] pairs. */
  waves?: [number, number][];
  /** Light streaks playing on water. */
  shimmer?: boolean;
  /** One colour instead of the tiles' own (sRGB): water. */
  flat?: [number, number, number];
  /** The glow crawls and, on the sides of a fall, runs down (lava). */
  crawl?: boolean;
}

// Corner order per direction, as the mesher has it (+X, -X, +Y, -Y, +Z, -Z):
// counter-clockwise from outside, starting bottom-left with Z up.
const CORNERS = [
  [1, 0, 0], [1, 1, 0], [1, 1, 1], [1, 0, 1],
  [0, 1, 0], [0, 0, 0], [0, 0, 1], [0, 1, 1],
  [1, 1, 0], [0, 1, 0], [0, 1, 1], [1, 1, 1],
  [0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1],
  [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1],
  [0, 1, 0], [1, 1, 0], [1, 0, 0], [0, 0, 0],
];
const NORMALS = [
  [1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1],
];

const vec3List = (rows: number[][]) => rows.map((row) => `vec3(${row.map((n) => n.toFixed(1)).join(", ")})`).join(", ");

const VERTEX = /* glsl */ `
${LIGHT_GLSL}
const vec3 CORNERS[24] = vec3[24](${vec3List(CORNERS)});
const vec3 NORMALS[6] = vec3[6](${vec3List(NORMALS)});
const vec2 UVS[4] = vec2[4](vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0));

in float corner;
in vec4 aCell;
in vec2 aTile;
in vec4 aDay;
in vec4 aGlow;
in vec4 aSun;
in vec4 aKey;

uniform float uTime;
uniform float uWind;
uniform vec4 uWaves;

out vec2 vUv;
out vec3 vLight;
out vec3 vWorld;
flat out vec3 vNormal;
flat out float vTile;

// The same slow gusts and quick flutter the previs has, as a push that depends only on
// where a point is, so corners that meet stay together.
vec3 windAt(vec3 p) {
  float gust = sin(dot(p, vec3(0.35, 0.22, 0.10)) - uTime * 1.3);
  float flutter = sin(dot(p, vec3(1.3, -0.9, 0.7)) - uTime * 3.1);
  return vec3(1.0, 0.35, 0.0) * (gust * 0.7 + flutter * 0.3) + vec3(0.0, 0.0, 0.25) * flutter;
}

void main() {
  int face = int(aCell.w + 0.5);
  int which = int(corner + 0.5);
  vec3 world = aCell.xyz + CORNERS[face * 4 + which];
  if (uWind > 0.0) world += windAt(world) * uWind;
  for (int i = 0; i < 2; i++) {
    float level = uWaves[i * 2];
    float height = uWaves[i * 2 + 1];
    if (height > 0.0 && abs(world.z - level) < 0.01) {
      world.z += height * (0.6 * sin(0.9 * world.x + 0.6 * world.y - uTime * 1.6) + 0.4 * sin(-0.5 * world.x + 1.1 * world.y - uTime * 2.3 + 1.0));
    }
  }
  vec4 mask = vec4(equal(ivec4(which), ivec4(0, 1, 2, 3)));
  vec4 baked = vec4(dot(aDay, mask), dot(aGlow, mask), dot(aSun, mask), dot(aKey, mask)) / 255.0;
  vUv = UVS[which];
  vTile = aTile.x;
  vNormal = NORMALS[face];
  // Light is worked out here, at the four corners, and spread over the face: on blocks
  // that is all the detail there is, and it costs nothing per pixel.
  vLight = lightOn(vNormal, baked, world);
  vWorld = world;
  gl_Position = projectionMatrix * viewMatrix * vec4(world, 1.0);
}
`;

const FRAGMENT = /* glsl */ `
${HAZE_GLSL}
uniform sampler2DArray uTiles;
uniform sampler2DArray uTilesEmit;
uniform float uTime;
uniform float uEmit;
uniform float uCut;
uniform float uAlpha;
uniform float uShimmer;
uniform float uCrawl;
uniform vec4 uFlat;

in vec2 vUv;
in vec3 vLight;
in vec3 vWorld;
flat in vec3 vNormal;
flat in float vTile;

float hash(vec3 p) {
  p = fract(p * vec3(0.1031, 0.1030, 0.0973));
  p += dot(p, p.yxz + 33.33);
  return fract((p.x + p.y) * p.z);
}

float noise(vec3 p) {
  vec3 i = floor(p);
  vec3 f = fract(p);
  f = f * f * (3.0 - 2.0 * f);
  return mix(
    mix(mix(hash(i), hash(i + vec3(1, 0, 0)), f.x), mix(hash(i + vec3(0, 1, 0)), hash(i + vec3(1, 1, 0)), f.x), f.y),
    mix(mix(hash(i + vec3(0, 0, 1)), hash(i + vec3(1, 0, 1)), f.x), mix(hash(i + vec3(0, 1, 1)), hash(i + vec3(1, 1, 1)), f.x), f.y),
    f.z);
}

void main() {
  vec3 at = vec3(vUv, vTile);
  vec4 tile = texture(uTiles, at);
  if (tile.a < uCut) discard;
  tile.rgb = mix(tile.rgb, uFlat.rgb, uFlat.a);
  vec3 glow = texture(uTilesEmit, at).rgb * uEmit;
  vec3 pixel = floor(vWorld * 16.0) / 16.0;  // the blocks' own pixel grid
  float alpha = uAlpha;
  if (uCrawl > 0.0) {
    // Lava is never still: blotches crawl over it, and run down the sides of a fall.
    float now = floor(uTime * 6.0) / 6.0;
    float side = abs(vNormal.z) < 0.5 ? 1.0 : 0.0;
    vec3 where = vec3(pixel.xy * 0.5, (pixel.z + now * 1.5 * side) * 0.8);
    glow *= mix(0.7, 1.35, smoothstep(0.32, 0.68, noise(where + vec3(0.0, 0.0, now * 0.2))));
    glow *= 1.0 + 0.08 * sin(uTime * 1.3) + 0.04 * sin(uTime * 3.7); // and its glow breathes
  }
  vec3 color = tile.rgb * vLight + glow;
  if (uShimmer > 0.0) {
    // Light playing on water: thin streaks on the pixel grid, redrawn a few times a second.
    float now = floor(uTime * 8.0) / 8.0;
    float along = abs(vNormal.z) > 0.5 ? pixel.y : pixel.z;
    float wave = 0.5 + 0.5 * sin(along * 8.4 + 2.6 * noise(vec3(pixel.x * 0.91, along * 2.9, 0.0)) * 6.2831 + now * 0.75);
    float dashes = noise(vec3(pixel.x * 0.8, along * 1.1, now * 0.3));
    float lit = smoothstep(0.90, 0.95, wave) * smoothstep(0.46, 0.54, dashes);
    color += vec3(1.0, 0.61, 0.39) * (0.85 * lit);
    alpha += lit * (1.0 - uAlpha) * 0.7;
  }
  gl_FragColor = vec4(seenFrom(color, vWorld), alpha);
}
`;

const QUAD = new Float32Array([0, 1, 2, 3]);
const INDEX = [0, 1, 2, 0, 2, 3];

export function voxelMaterial(globals: Globals, look: VoxelLook): ShaderMaterial {
  const waves = [0, 0, 0, 0];
  (look.waves ?? []).slice(0, 2).forEach(([level, height], i) => {
    waves[i * 2] = level;
    waves[i * 2 + 1] = height;
  });
  const uniforms: Record<string, IUniform> = {
    ...globals,
    uEmit: { value: look.emit },
    uCut: { value: look.cut ?? 0 },
    uWind: { value: look.wind ?? 0 },
    uAlpha: { value: look.alpha ?? 1 },
    uWaves: { value: waves },
    uShimmer: { value: look.shimmer ? 1 : 0 },
    uCrawl: { value: look.crawl ? 1 : 0 },
    uFlat: { value: look.flat ? [...linearColor(look.flat).toArray(), 1] : [0, 0, 0, 0] },
  };
  return new ShaderMaterial({
    uniforms,
    vertexShader: VERTEX,
    fragmentShader: FRAGMENT,
    transparent: look.alpha !== undefined && look.alpha < 1,
    depthWrite: look.alpha === undefined || look.alpha >= 1,
  });
}

/** One mesh per cube of the world, all sharing a material. */
export function voxelMeshes(name: string, records: Records, material: ShaderMaterial): Group {
  const group = new Group();
  group.name = name;
  for (const [x0, y0, z0, x1, y1, z1, first, count] of records.chunks) {
    const geometry = new InstancedBufferGeometry();
    geometry.setAttribute("position", new BufferAttribute(new Float32Array(12), 3));
    geometry.setAttribute("corner", new BufferAttribute(QUAD, 1));
    geometry.setIndex(INDEX);
    geometry.setAttribute("aCell", new InstancedBufferAttribute(records.cells.subarray(first * 4, (first + count) * 4), 4));
    geometry.setAttribute("aTile", new InstancedBufferAttribute(records.tiles.subarray(first * 2, (first + count) * 2), 2));
    const bytes = new InstancedInterleavedBuffer(records.light.subarray(first * 16, (first + count) * 16), 16);
    geometry.setAttribute("aDay", new InterleavedBufferAttribute(bytes, 4, 0));
    geometry.setAttribute("aGlow", new InterleavedBufferAttribute(bytes, 4, 4));
    geometry.setAttribute("aSun", new InterleavedBufferAttribute(bytes, 4, 8));
    geometry.setAttribute("aKey", new InterleavedBufferAttribute(bytes, 4, 12));
    geometry.instanceCount = count;
    const centre = new Vector3((x0 + x1) / 2, (y0 + y1) / 2, (z0 + z1) / 2);
    geometry.boundingSphere = new Sphere(centre, Math.hypot(x1 - x0, y1 - y0, z1 - z0) / 2 + 1.5);
    const mesh = new Mesh(geometry, material);
    mesh.matrixAutoUpdate = false;
    group.add(mesh);
  }
  return group;
}
