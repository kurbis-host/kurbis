/**
 * Everything that changes with the place rather than with a thing: the suns, the sky's
 * light, the haze, the covers that fill the frame, and the bend of the lens in the
 * portal. All of it scrubbed by the scroll.
 */

import { Color } from "three";
import { linear } from "../anim/easing";
import type { Post } from "../engine/post";
import { SKY_DOWN, SKY_SIDE, SKY_UP, setHaze, type Stage } from "../engine/stage";
import { HAZES } from "../engine/shading";

/** How much of the sun the open-air haze scatters, against the textbook figure (fitted to the previs). */
const SUNLIT = 1.16;
/**
 * The Nether's air glows: the lava sea's lamps stand in it. Their colour, at the
 * strength the previs shows between the pillars (measured there: about 0.13 of red
 * added through a hundred blocks of it).
 */
const NETHER_GLOW = [0.35, 0.119, 0.0245];

interface Ramps {
  sun: (s: number) => number;
  heroKey: (s: number) => number;
  heroFill: (s: number) => number;
  landingKey: (s: number) => number;
  digKey: (s: number) => number;
  digDaylight: (s: number) => number;
  sky: (s: number) => number;
  mist: (s: number) => number;
  digHaze: (s: number) => number;
}

export function makeGrade(stage: Stage, post: Post): (s: number) => void {
  const through = stage.data.constants.THROUGH;
  const [closing, white0, white1, clear] = stage.data.constants.WHITE;
  // The cloud the dive goes through, and the portal's swirl: each fills the frame for a moment.
  const white = linear([[closing, 0], [white0, 1], [white1, 1], [clear, 0]]);
  // (Twice: into the Nether, and home again through the second frame.)
  const swirls = stage.data.constants.SWIRL.map(([a, b, c, d]) => linear([[a, 0], [b, 1], [c, 1], [d, 0]]));
  const swirl = (s: number) => Math.max(...swirls.map((one) => one(s)));
  const home = stage.data.constants.HOME;
  const portalWobble = linear([[4.95, 0], [5.14, 0.09], [5.28, -0.07], [5.4, 0.045], [5.52, 0]]);
  // (Time turned back on the homestead keeps the picture still: the house rebuilding is the show.)
  const wobble = portalWobble;
  post.setWhite(new Color(1.0, 0.84, 0.8).multiplyScalar(1.3));
  const portal = stage.pictures.named.get("block/nether_portal.png");
  if (portal) post.setSwirl(portal, stage.tall ? [0.55, 0.458] : [0.98, 0.815]);
  const ramps: Ramps = {
    sun: linear([[2.3, 5.0], [2.55, 0.0], [home - 0.01, 0.0], [home, 5.0]]), // (at 7 the low sun glared over the surface)
    heroKey: linear([[0.0, 2.1], [0.78, 2.1], [0.95, 0.0]]),
    heroFill: linear([[0.0, 125.0], [0.42, 125.0], [0.52, 0.0]]),
    landingKey: linear([[0.66, 0.0], [0.86, 2.3], [1.56, 2.3], [1.74, 0.0], [home - 0.01, 0.0], [home, 2.3]]),
    digKey: linear([[1.54, 0.0], [1.72, 0.9], [2.42, 0.9], [2.62, 0.0]]),
    digDaylight: linear([[1.54, 0.0], [1.72, 2600.0], [2.5, 2600.0], [2.8, 0.0]]),
    sky: linear([[1.5, 1.0], [1.78, 0.55], [2.45, 0.55], [2.75, 0.1], [through - 0.01, 0.1], [through, 0.05], [home - 0.01, 0.05], [home, 1.0]]),
    mist: linear([[0.62, 0.0], [0.9, 0.005], [1.5, 0.005], [1.74, 0.0]]),
    digHaze: linear([[1.5, 0.0], [1.75, 0.011]]),
  };
  const { lamps, globals, fog } = stage;
  return (s) => {
    post.covers.white = white(s);
    post.covers.swirl = swirl(s);
    post.covers.wobble = wobble(s);
    lamps.set("Sun", ramps.sun(s));
    lamps.set("Hero_Key", ramps.heroKey(s));
    lamps.set("Hero_Fill", ramps.heroFill(s));
    lamps.set("Landing_Key", ramps.landingKey(s));
    lamps.set("Dig_Key", ramps.digKey(s));
    lamps.set("Dig_Daylight", ramps.digDaylight(s));
    const sky = ramps.sky(s);
    stage.skyStrength.value = sky;
    globals.uSkyUp.value.copy(SKY_UP).multiplyScalar(sky);
    globals.uSkySide.value.copy(SKY_SIDE).multiplyScalar(sky);
    globals.uSkyDown.value.copy(SKY_DOWN).multiplyScalar(sky);

    // Haze: whichever boxes of air are in play here, into the shader's few slots.
    const wanted: [name: string, density: number, sunlit: number, glow?: readonly number[]][] = [
      ["Haze", s < through || s >= home ? (fog.get("Haze")?.density ?? 0) : 0, SUNLIT],
      ["Landing_Mist", ramps.mist(s), SUNLIT],
      ["Dig_Haze", s < 3.2 ? ramps.digHaze(s) : 0, 0],
      ["Cave_Haze", s > 2.4 && s < 4.2 ? (fog.get("Cave_Haze")?.density ?? 0) : 0, 0],
      ["Dungeon_Haze", s > 3.8 && s < through ? (fog.get("Dungeon_Haze")?.density ?? 0) : 0, 0],
      ["Nether_Haze", s >= through && s < home ? (fog.get("Nether_Haze")?.density ?? 0) : 0, 0, NETHER_GLOW],
    ];
    let slot = 0;
    stage.hazes.clear(); // (and which slot each has, for what is laid over the grade: ./weather.ts)
    for (const [name, density, sunlit, glow] of wanted) {
      const box = fog.get(name);
      if (!box || density <= 0 || slot >= HAZES) continue;
      setHaze(globals, slot, box, density, sunlit, glow);
      stage.hazes.set(name, slot);
      slot += 1;
    }
    for (; slot < HAZES; slot += 1) globals.uHazeColor.value[slot].w = 0;
  };
}
