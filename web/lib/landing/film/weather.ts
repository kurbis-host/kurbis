/**
 * The time of day and the weather, as the chat's /time and /weather set them (./chat.ts).
 * The film is lit for one golden hour, which the scroll grades (./grade.ts); night and rain
 * are laid over that grade each frame, after it has had its say, and taking them off again
 * leaves it as it was.
 *
 *   Night   the sun is down: the daylight the art lit each stop with (the sun, the key suns,
 *           the fills, the light from the sky) is turned down to a cool moonlight; the sky's
 *           gradient goes to the night's, with a last trace of the sunset low where the sun
 *           went, and the sky's own stars come out lower than its dusk has them; the haze is
 *           the night's dark blue, and the clouds and the leaves no longer catch the sun.
 *           There is no moon: in the game it rises across the sky from where the sun set,
 *           which here is behind the camera at every stop.
 *   Rain    the art's rain (the shower under the cloud the visitor can click: ./valley.ts),
 *           falling everywhere round the camera, down to the ground; and the light a little
 *           greyer under it, as the game's is.
 *
 * Both are the open air's: the sky, the pond, the top of the dig, and home again at the end.
 * The deeper dig, the cave, the cart's tunnel and the homestead's cavern are under the
 * ground, and the Nether is another dimension: there, nothing changes, and the next place
 * in the open air shows what was set.
 */

import { Color, Vector3, Vector4, type Mesh, type ShaderMaterial } from "three";
import { clamp01, lerp, linear } from "../anim/easing";
import { flakesMesh } from "../engine/flakes";
import type { Post } from "../engine/post";
import type { Ctx } from "./context";

/** The art's daylight (lamps the grade sets by the scroll), and how much of each is left by night: the sun is down; the rest is moonlight. */
const MOONLIT: Record<string, number> = { Sun: 0, Hero_Key: 0.5, Hero_Fill: 0.65, Landing_Key: 0.2, Dig_Key: 0.2, Dig_Daylight: 0.16 };
/** Moonlight's colour, and what the light from the sky is by night (a share of each of its colours). */
const MOON = new Color(0.5, 0.66, 1);
const NIGHT_SKY = new Color(0.16, 0.36, 0.85);
/**
 * The night sky by height over the horizon (the art's stops for its dusk are at the same
 * heights): near black overhead, a deep blue toward the horizon, and the last of the sunset
 * on it. Linear colour, as the art's.
 */
const NIGHT: readonly (readonly [number, number, number])[] = [
  [0.004, 0.004, 0.009],
  [0.02, 0.02, 0.036],
  [0.05, 0.042, 0.06],
  [0.03, 0.034, 0.07],
  [0.018, 0.026, 0.062],
  [0.01, 0.016, 0.045],
  [0.005, 0.009, 0.03],
  [0.002, 0.004, 0.016],
];
/** How much of the sunset's glow is left round where the sun went down, and how many more stars, how low. */
const AFTERGLOW = 0.1;
const STARS = 3.2;
const STARS_FROM = 0.02;
/** The haze by night: the dark blue the distance fades into (its colour, and the light it has of its own). */
const HAZE_NIGHT = new Vector3(0.45, 0.55, 1);
const HAZE_GLOW = 0.2;
/** Type keeps some light of its own, so it stays graphic in a dark place (engine/type.ts): the open air's, this much more of it by night. */
const LETTERS = 1.5;
/** The cloud the dive goes through, by night (post.ts: linear, at its strength). */
const WHITE_NIGHT = new Color(0.07, 0.08, 0.13);
/** What glows of its own by day for the sun's sake, and how much of that is left by night: the clouds, the falling leaves and petals, the shower under a cloud. */
const SUNLIT_GLOW: [RegExp, number][] = [[/^Cloud/, 0.15], [/^(Leaves_|Petals_air|Petal_Burst)/, 0.3], [/^Rain$/, 0.35]];

/**
 * Rain: the box of it round the camera (blocks: across, along, up), how far ahead of the eye
 * its middle is, and how many drops. Seen from among them rather than from far off, as the
 * cloud's shower is, they are finer and fainter than its, and none comes nearer the eye than a
 * few blocks.
 */
const BOX: [number, number, number] = [40, 40, 26];
const AHEAD = 14;
const DROPS = 2600;
const DROP: [number, number] = [0.035, 1.1];
const RAIN_TINT = 0.34;
const NEAR = 3;
/** Under rain: how much greyer the sky is, and how much of the daylight is left. */
const GREYER = 0.6;
const OVERCAST = 0.7;

/** Seconds it takes night to fall (the game sets it at once; a moment reads better), and rain to start or stop. */
const DUSK = 0.35;
const SHOWER = 1.2;

export interface Weather {
  readonly night: boolean;
  readonly rain: boolean;
  setNight(night: boolean): void;
  setRain(rain: boolean): void;
}

export function setWeather(ctx: Ctx, post: Post): Weather {
  const { tl, C, stage } = ctx;
  const { lamps, globals } = stage;
  // The open air: down to the top of the dig, where it goes under the ground; and home again (whose swirl hides the change).
  const outdoors = linear([[2.2, 1], [2.4, 0], [C.HOME - 0.01, 0], [C.HOME, 1]]);
  const wanted = { night: false, rain: false };
  let dark = 0; // how far night has fallen, and how hard it rains (0..1)
  let wet = 0;

  // What the grade and the art leave alone, as they had it: the daylight's colours, the sky's look, what glows for the sun.
  const daylight = Object.keys(MOONLIT).flatMap((name) => {
    const lamp = lamps.byName.get(name);
    return lamp ? [{ lamp, base: lamp.color.clone(), left: MOONLIT[name] }] : [];
  });
  const sky = (stage.scene.getObjectByName("Sky") as Mesh | undefined)?.material as ShaderMaterial | undefined;
  const look = sky?.uniforms;
  const day = look && {
    stops: (look.uStops.value as Vector4[]).map((stop) => stop.clone()),
    glow: (look.uGlow.value as Vector3).clone(),
    lobes: (look.uLobes.value as Vector4).clone(),
    stars: look.uStars.value as number,
  };
  const glowing: { uniform: { value: number[] }; base: number[]; left: number }[] = [];
  const seen = new Set<ShaderMaterial>();
  for (const [name, node] of stage.nodes) {
    const left = SUNLIT_GLOW.find(([pattern]) => pattern.test(name))?.[1];
    if (left === undefined) continue;
    node.traverse((part) => {
      for (const material of [(part as Mesh).material ?? []].flat() as ShaderMaterial[]) {
        const uniform = material.uniforms?.uGlow as { value: number[] } | undefined;
        if (!uniform || seen.has(material) || !Array.isArray(uniform.value)) continue;
        seen.add(material);
        glowing.push({ uniform, base: [...uniform.value], left });
      }
    });
  }
  const letters = stage.typeLook("Type", 1);
  const lettersDay = letters.uniforms.uSelf.value as number;
  const white = post.white(); // (as the grade set it)
  const scratch = new Color();

  // The rain: the art's own shower, made again as a box of it that goes where the camera goes. Its
  // drops keep their places in the world as it does (each falls through boxes laid end to end), so
  // the box can move without a drop moving with it; and it stops at the ground.
  const shower = stage.layout.nodes.find((node) => node.name === "Rain");
  let rain: Mesh | null = null;
  let drops: ShaderMaterial | null = null;
  let wetGlow: number[] = [];
  if (shower?.note) {
    rain = flakesMesh(globals, { ...shower.note, lo: [0, 0, 0], hi: BOX, count: DROPS, size: DROP, seed: 41 }, stage.data.materials[shower.material ?? ""], 0);
    rain.name = "Weather_Rain";
    rain.frustumCulled = false;
    rain.renderOrder = 6;
    rain.visible = false;
    drops = rain.material as ShaderMaterial;
    drops.uniforms.uNear.value = NEAR;
    wetGlow = [...(drops.uniforms.uGlow.value as number[])];
    stage.scene.add(rain);
  }
  const forward = new Vector3();
  const grey = new Vector4();

  tl.always(({ s, dt }) => {
    dark += ((wanted.night ? 1 : 0) - dark) * (1 - Math.exp(-dt / DUSK));
    wet += ((wanted.rain ? 1 : 0) - wet) * (1 - Math.exp(-dt / SHOWER));
    if (Math.abs(dark - (wanted.night ? 1 : 0)) < 1e-3) dark = wanted.night ? 1 : 0;
    if (Math.abs(wet - (wanted.rain ? 1 : 0)) < 1e-3) wet = wanted.rain ? 1 : 0;
    const open = outdoors(s);
    const n = dark * open;
    const r = wet * open;

    // The light: moonlight for daylight, and less of either under rain.
    const overcast = lerp(1, OVERCAST, r);
    for (const { lamp, base, left } of daylight) {
      lamp.energy *= lerp(1, left, n) * overcast;
      lamp.color.copy(base).lerp(MOON, n);
    }
    if (n > 0 || r > 0) {
      for (const key of [globals.uSkyUp, globals.uSkySide, globals.uSkyDown]) key.value.lerp(scratch.copy(key.value).multiply(NIGHT_SKY), n).multiplyScalar(overcast);
      // The haze: the night's blue, which the distance fades into. (By day the sun lights it, and it is left as it is.)
      for (const name of ["Haze", "Landing_Mist"]) {
        const slot = stage.hazes.get(name);
        if (slot === undefined) continue;
        const color = globals.uHazeColor.value[slot];
        color.set(lerp(color.x, HAZE_NIGHT.x, n), lerp(color.y, HAZE_NIGHT.y, n), lerp(color.z, HAZE_NIGHT.z, n), color.w);
        globals.uHazeGlow.value[slot].addScaledVector(HAZE_NIGHT, HAZE_GLOW * n);
      }
    }
    for (const { uniform, base, left } of glowing) for (let i = 0; i < base.length; i += 1) uniform.value[i] = base[i] * lerp(1, left, n);
    letters.uniforms.uSelf.value = lettersDay * lerp(1, LETTERS, n);
    post.setWhite(scratch.copy(white).lerp(WHITE_NIGHT, n).multiplyScalar(overcast));

    // The sky: the night's gradient, the sunset's last glow, no sun, and the stars out lower. Under rain, greyer.
    if (look && day) {
      (look.uStops.value as Vector4[]).forEach((stop, i) => {
        const from = day.stops[i];
        const to = NIGHT[Math.min(i, NIGHT.length - 1)];
        stop.set(from.x, lerp(from.y, to[0], n), lerp(from.z, to[1], n), lerp(from.w, to[2], n));
        const level = (0.3 * stop.y + 0.59 * stop.z + 0.11 * stop.w) * 0.8;
        grey.set(stop.x, level, level, level);
        stop.lerp(grey, GREYER * r);
        stop.x = from.x;
      });
      (look.uGlow.value as Vector3).copy(day.glow).multiplyScalar(lerp(1, AFTERGLOW, n) * lerp(1, 0.3, r));
      (look.uLobes.value as Vector4).set(day.lobes.x, day.lobes.y, day.lobes.z, day.lobes.w * (1 - n) * (1 - r));
      look.uStars.value = lerp(day.stars, STARS, n) * (1 - r);
      look.uStarsFrom.value = lerp(0.16, STARS_FROM, n);
    }

    // The rain, round the camera and a little ahead of it, from the ground up.
    if (rain && drops) {
      rain.visible = r > 0.003;
      if (!rain.visible) return;
      const eye = stage.camera.position;
      stage.camera.getWorldDirection(forward);
      const lo = drops.uniforms.uLo.value as number[];
      lo[0] = eye.x + forward.x * AHEAD - BOX[0] / 2;
      lo[1] = eye.y + forward.y * AHEAD - BOX[1] / 2;
      lo[2] = Math.max(eye.z - BOX[2] / 2, C.POND_LEVEL);
      rain.userData.tint = RAIN_TINT * clamp01(r);
      const glow = drops.uniforms.uGlow.value as number[];
      for (let i = 0; i < 3; i += 1) glow[i] = wetGlow[i] * lerp(1, 0.35, n); // (by night it shines less)
    }
  });

  return {
    get night() {
      return wanted.night;
    },
    get rain() {
      return wanted.rain;
    },
    setNight(night) {
      wanted.night = night;
    },
    setRain(on) {
      wanted.rain = on;
    },
  };
}
