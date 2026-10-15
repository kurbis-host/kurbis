/**
 * Materials for everything small (the glb): players, the pumpkin, props, sprites. The
 * glb carries no pictures; each material is rebuilt here from the note the art pipeline
 * left under its name, and lit like the rest of the world.
 */

import { DoubleSide, ShaderMaterial, type IUniform, type Material, type Texture } from "three";
import type { MaterialNote } from "./bundle";
import { HAZE_GLSL, LIGHT_GLSL, linearColor, type Globals } from "./shading";

const VERTEX = /* glsl */ `
in float _sway;        // how far a point of a plant is from its root
uniform float uTime;
uniform float uCalm;
uniform vec4 uMoves;   // wind: strength, rooted or loose; swell: the water's level, its height
uniform vec2 uAtlas;   // for what is mapped onto the block atlas: its tiles across, and a tile's pixels across (0: not)
out vec2 vUv;
flat out vec2 vTile;   // the atlas tile a face shows
out vec3 vWorld;
out vec3 vNormal;
void main() {
  vec4 world = modelMatrix * vec4(position, 1.0);
  if (uMoves.x > 0.0) {
    // Slow gusts with a quick flutter on top. The push depends only on where a point
    // is, so corners that meet stay together; a rooted thing moves by its distance from the root.
    float gust = sin(dot(world.xyz, vec3(0.35, 0.22, 0.10)) - uTime * 1.3);
    float flutter = sin(dot(world.xyz, vec3(1.3, -0.9, 0.7)) + uTime * 3.1);
    float amount = uMoves.x * mix(1.0, _sway, uMoves.y) * (1.0 - uCalm);
    float push = (gust + flutter * 0.3) * amount;
    world.xyz += vec3(push, push * 0.35, flutter * 0.15 * amount);
  }
  if (uMoves.w > 0.0 && abs(world.z - uMoves.z) < 0.04) {
    // What lies on the water rides its swell: two swells crossing each other.
    world.z += uMoves.w * (0.6 * sin(0.9 * world.x + 0.6 * world.y - uTime * 1.6) + 0.4 * sin(-0.5 * world.x + 1.1 * world.y - uTime * 2.3 + 1.0));
  }
  vWorld = world.xyz;
  vNormal = mat3(modelMatrix) * normal;
  vUv = vec2(uv.x, 1.0 - uv.y); // glTF counts v from the top; the pictures are loaded bottom row first, as the art has them
  vTile = floor(vUv * uAtlas.x); // (every corner of a face lies just inside its tile)
  gl_Position = projectionMatrix * viewMatrix * world;
}
`;

const FRAGMENT = /* glsl */ `
${LIGHT_GLSL}
${HAZE_GLSL}
uniform sampler2D uMap;
uniform sampler2D uGlowMap;
uniform vec4 uColor;      // what the picture is multiplied by (or the whole colour, without one)
uniform vec3 uGlow;       // light of its own
uniform vec4 uFlags;      // has picture, has glow picture, cut-out threshold, screen-door
uniform float uSelf;
uniform float uTint;      // the object's own opacity (things that fade share a material)
uniform float uStrip;     // frames in a picture that is a strip of them, one above the other (0: it is one picture)
uniform float uTime;
uniform vec2 uAtlas;
in vec2 vUv;
flat in vec2 vTile;
in vec3 vWorld;
in vec3 vNormal;

void main() {
  vec4 base = uColor;
  vec2 uv = vUv;
  // Only ever the face's own tile: seen small or edge on, a face's edge would sample the next one's pixels.
  if (uAtlas.x > 0.0) uv = clamp(uv, (vTile + 0.5 / uAtlas.y) / uAtlas.x, (vTile + 1.0 - 0.5 / uAtlas.y) / uAtlas.x);
  if (uStrip > 0.0) uv.y = fract(uv.y - floor(uTime * 20.0) / uStrip); // played as the game plays it
  if (uFlags.x > 0.5) base *= texture(uMap, uv);
  if (base.a < uFlags.z) discard;
  float alpha = base.a * uTint;
  if (uFlags.w > 0.5) {
    // A see-through thing drawn as a screen door, so layers need no sorting.
    float door = fract(52.9829189 * fract(dot(gl_FragCoord.xy, vec2(0.06711056, 0.00583715))));
    if (alpha < door) discard;
    alpha = 1.0;
  }
  vec3 glow = uGlow * (uFlags.y > 0.5 ? texture(uGlowMap, uv).rgb : (uFlags.x > 0.5 ? base.rgb : vec3(1.0)));
  vec3 normal = normalize(gl_FrontFacing ? vNormal : -vNormal);
  float day = daylightAt(vWorld);
  vec3 color = base.rgb * (uSelf + lightOn(normal, vec4(day, 0.0, day, day), vWorld)) + glow;
  gl_FragColor = vec4(seenFrom(color, vWorld), alpha);
}
`;

// Flat light, see-through by the object's opacity: ripples, hints, sparks of dust.
const FADING = /* glsl */ `
uniform vec3 uGlow;
uniform float uTint;
void main() {
  gl_FragColor = vec4(uGlow, uTint);
}
`;

// The cracks a block shows as it is dug: ten stages side by side in one picture, and the
// object says which one shows (0..1), so every cracking block shares the material.
const CRACK = /* glsl */ `
${LIGHT_GLSL}
${HAZE_GLSL}
uniform sampler2D uMap;
uniform float uStages;
uniform float uTint;
in vec2 vUv;
in vec3 vWorld;
in vec3 vNormal;
void main() {
  float which = floor(clamp(uTint, 0.0, 0.999) * uStages);
  vec4 base = texture(uMap, vec2((fract(vUv.x) + which) / uStages, vUv.y));
  if (base.a < 0.3) discard;
  vec3 normal = normalize(gl_FrontFacing ? vNormal : -vNormal);
  float day = daylightAt(vWorld);
  gl_FragColor = vec4(seenFrom(base.rgb * lightOn(normal, vec4(day, 0.0, day, day), vWorld), vWorld), 1.0);
}
`;

/** Materials that many things share while each fades (or cracks) by itself: they read the thing's own tint. */
export function readsTint(note: MaterialNote | undefined): boolean {
  return note?.kind === "fading" || note?.kind === "crack";
}

export interface Pictures {
  atlas: Texture;
  atlasEmit: Texture;
  /** Pictures by the name the notes use. */
  named: Map<string, Texture>;
  /** The atlas's tiles across, and a tile's pixels across. */
  grid: [number, number];
}

/** What the art said moves a thing by itself: wind in a plant, the swell under what floats. */
export interface Moves {
  wind?: { strength: number; rooted: boolean };
  waves?: { level: number; height: number }[];
}

function shader(globals: Globals, extra: Record<string, IUniform>, fragmentShader: string, options: Partial<ShaderMaterial> = {}): ShaderMaterial {
  return new ShaderMaterial({ uniforms: { uMoves: { value: [0, 0, 0, 0] }, uStrip: { value: 0 }, uAtlas: { value: [0, 0] }, ...globals, ...extra }, vertexShader: VERTEX, fragmentShader, side: DoubleSide, ...options } as ConstructorParameters<typeof ShaderMaterial>[0]);
}

export function modelMaterial(globals: Globals, note: MaterialNote, pictures: Pictures, moves: Moves = {}): Material {
  const made = build(globals, note, pictures);
  const wave = moves.waves?.[0];
  if (moves.wind || wave) (made as ShaderMaterial).uniforms.uMoves.value = [moves.wind?.strength ?? 0, moves.wind?.rooted ? 1 : 0, wave?.level ?? 0, wave?.height ?? 0];
  return made;
}

function build(globals: Globals, note: MaterialNote, pictures: Pictures): Material {
  const none = pictures.atlas;
  if (note.kind === "voxel") {
    return shader(globals, {
      uMap: { value: pictures.atlas },
      uGlowMap: { value: pictures.atlasEmit },
      uColor: { value: [1, 1, 1, note.alpha ?? 1] },
      uGlow: { value: [note.emit ?? 0, note.emit ?? 0, note.emit ?? 0] },
      uFlags: { value: [1, 1, 0, note.dither ? 1 : 0] },
      uSelf: { value: note.glow ?? 0 },
      uTint: { value: 1 },
      uAtlas: { value: pictures.grid },
    }, FRAGMENT);
  }
  if (note.kind === "flat") {
    const color = linearColor(note.rgb ?? [1, 1, 1]);
    const glow = linearColor(note.emit_rgb ?? note.rgb ?? [1, 1, 1], note.emit ?? 0);
    const alpha = note.alpha ?? 1;
    return shader(globals, {
      uMap: { value: none },
      uGlowMap: { value: none },
      uColor: { value: [color.r, color.g, color.b, alpha] },
      uGlow: { value: [glow.r, glow.g, glow.b] },
      uFlags: { value: [0, 0, 0, note.dither ? 1 : 0] },
      uSelf: { value: 0 },
      uTint: { value: 1 },
    }, FRAGMENT, { transparent: alpha < 1 && !note.dither, depthWrite: alpha >= 1 || Boolean(note.dither) });
  }
  if (note.kind === "tex") {
    const map = pictures.named.get(note.image ?? `@${note.made}`) ?? none;
    const glowMap = note.emit_image ? pictures.named.get(note.emit_image) : undefined;
    const tint = note.tint ? linearColor(note.tint) : null;
    const emit = note.emit ?? 0;
    return shader(globals, {
      uMap: { value: map },
      uGlowMap: { value: glowMap ?? none },
      uColor: { value: tint ? [tint.r, tint.g, tint.b, 1] : [1, 1, 1, 1] },
      uGlow: { value: [emit, emit, emit] },
      uFlags: { value: [1, glowMap ? 1 : 0, note.clip ? 0.5 : 0, 0] },
      uSelf: { value: 0 },
      uTint: { value: 1 },
      uStrip: { value: note.image === "block/nether_portal.png" ? 32 : 0 },
    }, FRAGMENT);
  }
  if (note.kind === "fading") {
    const glow = linearColor(note.rgb ?? [1, 1, 1], note.strength ?? 1);
    return shader(globals, { uGlow: { value: [glow.r, glow.g, glow.b] }, uTint: { value: 1 } }, FADING, { transparent: true, depthWrite: false });
  }
  if (note.kind === "crack") {
    const map = pictures.named.get(`@${note.made}`) ?? none;
    return shader(globals, { uMap: { value: map }, uStages: { value: note.stages ?? 10 }, uTint: { value: 0 } }, CRACK, { polygonOffset: true, polygonOffsetFactor: -1, polygonOffsetUnits: -2 });
  }
  // Scrims get their own materials where they are built.
  return shader(globals, { uGlow: { value: [1, 0, 1] }, uTint: { value: 1 } }, FADING);
}
