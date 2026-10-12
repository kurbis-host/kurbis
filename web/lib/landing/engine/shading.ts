/**
 * Light and haze, shared by every material of the film.
 *
 * Everything is worked out in linear light and written unclamped to a float target;
 * post.ts adds bloom and turns it into display colour (no tone curve: the previs was
 * graded in Blender's Standard view).
 *
 * Light on a surface is the sum of
 *   daylight   baked per corner (how much sky a place sees), tinted by the place
 *   the sun    baked visibility per corner times the facing
 *   key suns   the previs's other suns (a key from behind the camera at each open-air
 *              stop). They have no baked shadow; daylight stands in for it
 *   glow       baked reach of blocks that shine (lava, glowstone)
 *   lamps      a short list of live lamps: torches, the pumpkin's candle, area fills
 * and haze is light scattered in a few boxes of air. The numbers were fitted to
 * renders of the previs with each kind of light on its own.
 */

import { Color, Vector3, Vector4, type IUniform, type Texture } from "three";

export const LAMPS = 16;
export const KEYS = 3;
export const HAZES = 4;

export interface Globals {
  [name: string]: IUniform;
  uEye: IUniform<Vector3>;
  uTime: IUniform<number>;
  uSunDir: IUniform<Vector3>;
  /** The same direction, under the name the haze reads it by. */
  uHazeSun: IUniform<Vector3>;
  /** The sun on a face turned full to it: irradiance over pi, in linear colour. */
  uSunColor: IUniform<Color>;
  /** The sun's irradiance, for what haze scatters. */
  uSunHaze: IUniform<Color>;
  /** Daylight on a face that looks straight up, on one that looks sideways, and on one that looks down. */
  uSkyUp: IUniform<Color>;
  uSkySide: IUniform<Color>;
  uSkyDown: IUniform<Color>;
  /** Light of shining blocks at their own face. */
  uGlowColor: IUniform<Color>;
  uKeyDir: IUniform<Vector3[]>;
  uKeyColor: IUniform<Vector3[]>;
  /** Where each lamp is, and the least distance (squared) its light is reckoned from. */
  uLampAt: IUniform<Vector4[]>;
  /** Its light at one block, and 1 if it is an area lamp, which shines mostly the way it points. */
  uLampColor: IUniform<Vector4[]>;
  uLampAim: IUniform<Vector3[]>;
  uHazeLo: IUniform<Vector3[]>;
  uHazeHi: IUniform<Vector3[]>;
  /** rgb of what it scatters, and its density per block. */
  uHazeColor: IUniform<Vector4[]>;
  /** forward lobe (g), where it starts to thin with height, where it is gone, share of sun it gets. */
  uHazeShape: IUniform<Vector4[]>;
  /** Light the haze has of its own (lamps standing in it). */
  uHazeGlow: IUniform<Vector3[]>;
  uTiles: IUniform<Texture | null>;
  uTilesEmit: IUniform<Texture | null>;
  /** Things without baked light count as in daylight above this height, and in the dark below it. */
  uDayFloor: IUniform<number>;
  /** 1 where the visitor asked for less motion: the wind drops. */
  uCalm: IUniform<number>;
}

export function makeGlobals(): Globals {
  const many = <T>(count: number, make: () => T) => Array.from({ length: count }, make);
  const sun = { value: new Vector3(0, 1, 0.1).normalize() };
  return {
    uEye: { value: new Vector3() },
    uTime: { value: 0 },
    uSunDir: sun,
    uHazeSun: sun,
    uSunColor: { value: new Color(0, 0, 0) },
    uSunHaze: { value: new Color(0, 0, 0) },
    uSkyUp: { value: new Color(0, 0, 0) },
    uSkySide: { value: new Color(0, 0, 0) },
    uSkyDown: { value: new Color(0, 0, 0) },
    uGlowColor: { value: new Color(0, 0, 0) },
    uKeyDir: { value: many(KEYS, () => new Vector3(0, 0, 1)) },
    uKeyColor: { value: many(KEYS, () => new Vector3()) },
    uLampAt: { value: many(LAMPS, () => new Vector4()) },
    uLampColor: { value: many(LAMPS, () => new Vector4()) },
    uLampAim: { value: many(LAMPS, () => new Vector3(0, 0, -1)) },
    uHazeLo: { value: many(HAZES, () => new Vector3()) },
    uHazeHi: { value: many(HAZES, () => new Vector3()) },
    uHazeColor: { value: many(HAZES, () => new Vector4()) },
    uHazeShape: { value: many(HAZES, () => new Vector4()) },
    uHazeGlow: { value: many(HAZES, () => new Vector3()) },
    uTiles: { value: null },
    uTilesEmit: { value: null },
    uDayFloor: { value: -9 },
    uCalm: { value: 0 },
  };
}

/** Light falling on a face: for a vertex shader (the voxels) or a fragment shader (the rest). */
export const LIGHT_GLSL = /* glsl */ `
#define LAMPS ${LAMPS}
#define KEYS ${KEYS}
uniform vec3 uSunDir;
uniform vec3 uSunColor;
uniform vec3 uSkyUp;
uniform vec3 uSkySide;
uniform vec3 uSkyDown;
uniform vec3 uGlowColor;
uniform vec3 uKeyDir[KEYS];
uniform vec3 uKeyColor[KEYS];
uniform vec4 uLampAt[LAMPS];
uniform vec4 uLampColor[LAMPS];
uniform vec3 uLampAim[LAMPS];
uniform float uDayFloor;

// Daylight on things that carry no baked light of their own: all of it in the open, none below ground.
float daylightAt(vec3 world) {
  return smoothstep(uDayFloor - 4.0, uDayFloor, world.z);
}

// baked = (daylight, glow, sun, key suns) as the exporter wrote them, 0..1.
vec3 lightOn(vec3 normal, vec4 baked, vec3 world) {
  vec3 sky = normal.z >= 0.0 ? mix(uSkySide, uSkyUp, normal.z) : mix(uSkySide, uSkyDown, -normal.z);
  vec3 sum = sky * baked.x
    + uGlowColor * baked.y * baked.y
    + uSunColor * baked.z * max(dot(normal, uSunDir), 0.0);
  for (int i = 0; i < KEYS; i++) {
    sum += uKeyColor[i] * (max(dot(normal, uKeyDir[i]), 0.0) * baked.w);
  }
  for (int i = 0; i < LAMPS; i++) {
    vec3 to = uLampAt[i].xyz - world;
    float far2 = dot(to, to);
    vec3 ray = to * inversesqrt(far2 + 1e-4);
    float facing = max(dot(normal, ray), 0.0);
    float spread = mix(1.0, max(dot(uLampAim[i], -ray), 0.0), uLampColor[i].w);
    sum += uLampColor[i].rgb * (facing * spread / max(far2, uLampAt[i].w));
  }
  return sum;
}
`;

/** What haze does to a colour seen from the eye: for fragment shaders. */
export const HAZE_GLSL = /* glsl */ `
#define HAZES ${HAZES}
uniform vec3 uEye;
uniform vec3 uHazeSun;
uniform vec3 uSunHaze;
uniform vec3 uHazeLo[HAZES];
uniform vec3 uHazeHi[HAZES];
uniform vec4 uHazeColor[HAZES];
uniform vec4 uHazeShape[HAZES];
uniform vec3 uHazeGlow[HAZES];

float forwardLobe(float cosine, float g) {
  float k = 1.0 + g * g - 2.0 * g * cosine;
  return (1.0 - g * g) / (12.566371 * k * sqrt(k));
}

// What is left of a colour seen along dir over dist, and what the haze in between adds.
// The last "shaded" blocks of the way lie in the shadow of what is seen there.
vec3 throughHaze(vec3 color, vec3 dir, float dist, float shaded) {
  for (int i = 0; i < HAZES; i++) {
    float density = uHazeColor[i].a;
    if (density <= 0.0) continue;
    vec3 inv = 1.0 / dir;
    vec3 a = (uHazeLo[i] - uEye) * inv;
    vec3 b = (uHazeHi[i] - uEye) * inv;
    vec3 near = min(a, b);
    vec3 far = max(a, b);
    float t0 = max(max(max(near.x, near.y), near.z), 0.0);
    float t1 = min(min(min(far.x, far.y), far.z), dist);
    if (t1 <= t0) continue;
    float thick = 1.0;
    vec4 shape = uHazeShape[i];
    if (shape.z > shape.y) {
      thick = 0.0;
      for (int k = 0; k < 6; k++) {
        float z = uEye.z + dir.z * mix(t0, t1, (float(k) + 0.5) / 6.0);
        thick += clamp((shape.z - z) / (shape.z - shape.y), 0.0, 1.0);
      }
      thick /= 6.0;
    }
    float held = 1.0 - exp(-density * thick * (t1 - t0));
    float sunlit = 1.0 - exp(-density * thick * max(min(t1, dist - shaded) - t0, 0.0));
    vec3 lit = uSunHaze * (shape.w * forwardLobe(dot(dir, uHazeSun), shape.x) * sunlit) + uHazeGlow[i] * held;
    color = color * (1.0 - held) + uHazeColor[i].rgb * lit;
  }
  return color;
}

// A thing seen against the sun shades the air in front of itself, the more of it the
// nearer the line of sight runs to the sun's own direction. That is why what stands
// before the sunset keeps its colour instead of fading into the glow.
vec3 seenFrom(vec3 color, vec3 world) {
  vec3 to = world - uEye;
  float dist = length(to);
  vec3 dir = to / dist;
  float toSun = dot(dir, uHazeSun);
  float shaded = toSun > 0.0 ? min(dist, 3.0 * toSun / sqrt(max(1.0 - toSun * toSun, 1e-4))) : 0.0;
  return throughHaze(color, dir, dist, shaded);
}
`;

/** sRGB numbers, as the palettes and notes give them, as a linear colour. */
export function linearColor(rgb: readonly number[], scale = 1): Color {
  return new Color().setRGB(rgb[0], rgb[1], rgb[2], "srgb").multiplyScalar(scale);
}
