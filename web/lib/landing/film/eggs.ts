/**
 * Names that do in the name fields what they do in the game. They are decoration (the
 * page's own HTML does not repeat them), and each lasts only while its name stands:
 *
 *   dinnerbone, grumm   the name turns upside down; and so does the visitor, wherever
 *                       the film shows them as a player
 *   jeb                 the name runs through the sixteen wool colours, as a sheep
 *                       called jeb_ does (a name cannot hold the "_")
 *   herobrine           the field says "Removed." first, as the patch notes always have
 *   creeper             the name does what the creeper does to the house: it hisses,
 *                       it blows up, and time turns back
 */

import { BoxGeometry, Color, Euler, Mesh, MeshBasicMaterial, Quaternion, SRGBColorSpace, Vector3, type BufferAttribute, type BufferGeometry, type Object3D } from "three";
import { backOut, clamp01, easeOut, lerp, seeded, smooth, wrap } from "../anim/easing";
import type { Palette, Rgb } from "../engine/bundle";
import { COPY } from "../copy";
import type { Ctx } from "./context";

export type Egg = "upsideDown" | "rainbow" | "removed" | "creeper";

const NAMES: [Egg, readonly string[]][] = [
  ["upsideDown", COPY.eggs.upsideDown],
  ["rainbow", [COPY.eggs.rainbow]],
  ["removed", [COPY.eggs.removed.name]],
  ["creeper", [COPY.eggs.creeper]],
];

/** The egg a name is, if it is one (in either case). */
export function eggOf(name: string): Egg | null {
  const typed = name.toLowerCase();
  return NAMES.find(([, names]) => names.includes(typed))?.[0] ?? null;
}

// -- herobrine -------------------------------------------------------------------------------
/** What a field says of Herobrine before what it found, and for how long (seconds). */
export const REMOVED = COPY.eggs.removed.said.toUpperCase();
export const REMOVED_FOR = 1.6;
/** In the grey of the game's own terse messages ("World saved."). */
export const SYSTEM: Palette = { front: [0.8, 0.8, 0.8], top: [0.7, 0.7, 0.7], side: [0.5, 0.5, 0.5], bottom: [0.3, 0.3, 0.3] };

// -- jeb_ ------------------------------------------------------------------------------------
/** The sixteen dyes in the game's order, as its wool takes them (its DyeColor texture colours). */
const DYES: Rgb[] = [0xf9fffe, 0xf9801d, 0xc74ebd, 0x3ab3da, 0xfed83d, 0x80c71f, 0xf38baa, 0x474f52, 0x9d9d97, 0x169c9c, 0x8932b8, 0x3c44aa, 0x835432, 0x5e7c16, 0xb02e26, 0x1d1d21].map(
  (hex) => [((hex >> 16) & 255) / 255, ((hex >> 8) & 255) / 255, (hex & 255) / 255],
);
/** Seconds from one colour to the next (the sheep's 25 ticks), and how far behind the one before each letter is, in colours. */
const WOOL = 1.25;
const RIPPLE = 0.22;
/** A dye on each face of a letter, as the film's palettes shade theirs: the front, the top, the sides, underneath. */
const SHADES = [1, 0.86, 0.62, 0.38];
/** The rim round each letter while it changes colour, in cells: dark on a bright ground, pale on a dark one. */
const RIM = 0.45;
/** The pale rim: the game's outline round glowing black ink on a sign. */
const PALE: Rgb = [0.94, 0.92, 0.8];

// -- creeper ---------------------------------------------------------------------------------
/** Seconds: the fuse, the blast (until the last letter is down), the beat after it, and time turned back. */
const FUSE = 1.4;
const BLAST = 0.75;
const BEAT = 0.6;
const REWIND = 0.7;
/** How hard the letters fall, in letter heights a second squared; and where they land, in cells under their line (the underline). */
const FALL = 26;
const GROUND = 3;
/** The white a letter flashes, on each face (shaded as the dyes are, so the letters stay apart): brighter than white, so pale letters in the dark flash too. */
const FLASH = SHADES.map((k) => new Color().setRGB(k, k, k, SRGBColorSpace).multiplyScalar(1.4));

/** Upside down: seconds to turn over, and back. */
const TURN = 0.34;
const UNTURN = 0.22;

const FACES = ["front", "top", "side", "bottom"] as const;
const Y = new Vector3(0, 1, 0);

interface Letter {
  readonly mesh: Mesh;
  readonly ch: string;
}

/** A letter that has its own colours for now: the glyph it shares otherwise, which face each corner is on, and its rim. */
interface Own {
  shared: BufferGeometry;
  faces: Uint8Array;
  rim: Mesh | null;
}

export interface LineEggs {
  /** The line's text as it now stands: the egg it is starts, the one it was stops. */
  type(text: string, time: number): void;
  /** Over the letters as the line has just laid them (`width` from the first letter's left to the last one's right). */
  lay(letters: readonly Letter[], time: number, width: number, fit: number): void;
}

/** What the eggs do to a typed line (./name-line.ts). `palette` is the line's own. */
export function lineEggs(ctx: Ctx, parent: Object3D, style: { palette: string | Palette; cell: number; day: number; left: number; line: number }): LineEggs {
  const { stage } = ctx;
  const rows = stage.data.glyphs.block.rows;
  const own = typeof style.palette === "string" ? stage.data.palettes[style.palette] : style.palette;
  // Light letters stand on something dark (the Nether), dark ones on something bright (the sky's plate): a rim that differs from the ground.
  const pale = own.front[0] + own.front[1] + own.front[2] > 1.5;
  let egg: Egg | null = null;
  let since = 0;
  const turn = { from: 0, to: 0, at: -Infinity };
  const turned = (time: number) => {
    const u = (time - turn.at) / (turn.to > turn.from ? TURN : UNTURN);
    return lerp(turn.from, turn.to, turn.to > turn.from ? backOut(u, 1.2) : smooth(u));
  };

  // Letters with colours of their own (the rainbow, the fuse's flash) wear a copy of their glyph.
  const owned = new Map<Mesh, Own>();
  const ownOf = (mesh: Mesh): Own => {
    let made = owned.get(mesh);
    if (!made) {
      const shared = mesh.geometry;
      mesh.geometry = shared.clone();
      const normals = mesh.geometry.getAttribute("normal");
      const faces = new Uint8Array(normals.count);
      for (let i = 0; i < normals.count; i += 1) faces[i] = normals.getY(i) < -0.5 ? 0 : normals.getZ(i) > 0.5 ? 1 : normals.getZ(i) < -0.5 ? 3 : 2;
      owned.set(mesh, (made = { shared, faces, rim: null }));
    }
    return made;
  };
  const tones = FACES.map(() => new Color());
  const paint = (mesh: Mesh, palette: Palette, white = 0) => {
    const { faces } = ownOf(mesh);
    FACES.forEach((face, k) => tones[k].setRGB(palette[face][0], palette[face][1], palette[face][2], SRGBColorSpace).lerp(FLASH[k], white));
    const colors = mesh.geometry.getAttribute("color") as BufferAttribute;
    for (let i = 0; i < faces.length; i += 1) colors.setXYZ(i, tones[faces[i]].r, tones[faces[i]].g, tones[faces[i]].b);
    colors.needsUpdate = true;
  };
  const rimmed = (mesh: Mesh, ch: string) => {
    const made = ownOf(mesh);
    if (made.rim) return;
    made.rim = stage.makeRim([ch], { cell: style.cell, width: RIM, day: style.day });
    if (pale) {
      const colors = made.rim.geometry.getAttribute("color") as BufferAttribute;
      const tone = new Color().setRGB(PALE[0], PALE[1], PALE[2], SRGBColorSpace);
      for (let i = 0; i < colors.count; i += 1) colors.setXYZ(i, tone.r, tone.g, tone.b);
    }
    mesh.add(made.rim);
  };
  const plain = (mesh: Mesh) => {
    const made = owned.get(mesh);
    if (!made) return;
    mesh.geometry.dispose();
    mesh.geometry = made.shared;
    if (made.rim) {
      mesh.remove(made.rim);
      made.rim.geometry.dispose();
    }
    owned.delete(mesh);
  };
  const dye: Palette = { front: [0, 0, 0], top: [0, 0, 0], side: [0, 0, 0], bottom: [0, 0, 0] };

  // The blast: each letter on its own path (the same every time, so time turned back retraces it), and smoke.
  // (Speeds in letter heights a second: outward, the further the letter is from the middle; toward us; aside, whatever; up.
  // It turns mostly in its own plane, so it still reads as a letter as it tumbles.)
  const flights: { out: number; toward: number; aside: number; up: number; spin: Vector3 }[] = [];
  const flightOf = (i: number) => {
    while (flights.length <= i) {
      const random = seeded(31 + 17 * flights.length);
      const spin = new Vector3((random() - 0.5) * 7, (random() < 0.5 ? -1 : 1) * lerp(5, 11, random()), (random() - 0.5) * 7);
      flights.push({ out: lerp(1.4, 2.6, random()), toward: lerp(0.3, 1.2, random()), aside: (random() - 0.5) * 1.2, up: lerp(6, 9, random()), spin });
    }
    return flights[i];
  };
  let puffs: Mesh[] = [];
  const smoke = (fly: number, at: Vector3, h: number) => {
    if (!puffs.length && fly > 0) {
      const random = seeded(5);
      const box = new BoxGeometry(1, 1, 1);
      // (Shades of the game's explosion smoke, as the house's: pale at the heart, greyer at the edge.)
      puffs = Array.from({ length: 10 }, (_, k) => {
        const v = [0.82, 0.7, 0.6, 0.52][k % 4];
        const puff = new Mesh(box, new MeshBasicMaterial({ color: new Color(v, v * 0.98, v * 0.95), transparent: true, opacity: 0, depthWrite: false }));
        puff.userData = { dir: new Vector3((random() - 0.5) * 3.2, -lerp(0.1, 0.8, random()), lerp(-0.3, 1.4, random())), size: lerp(0.45, 1.0, random()), spin: random() * 3 };
        puff.renderOrder = 6;
        parent.add(puff);
        return puff;
      });
    }
    for (const puff of puffs) {
      const { dir, size, spin } = puff.userData as { dir: Vector3; size: number; spin: number };
      puff.visible = fly > 0.01;
      if (!puff.visible) continue;
      const grown = easeOut(clamp01(fly / 0.7), 3);
      puff.position.set(at.x + dir.x * grown * h, at.y + dir.y * grown * h, at.z + (dir.z * grown + 0.5 * fly) * h);
      puff.scale.setScalar(size * h * (0.3 + grown));
      puff.rotation.set(spin * fly, spin * 0.7 * fly, 0);
      (puff.material as MeshBasicMaterial).opacity = 0.7 * (1 - clamp01((fly - 0.25) / 0.5));
    }
  };

  const centre = new Vector3();
  const pivot = new Vector3();
  const swollen = new Vector3();
  const heart = new Vector3();
  const spun = new Quaternion();
  const euler = new Euler();
  const seen = new Set<Mesh>();

  return {
    type(text, time) {
      const next = eggOf(text);
      if (next === egg) return;
      turn.from = turned(time);
      turn.to = next === "upsideDown" ? 1 : 0;
      turn.at = time;
      egg = next;
      since = time;
    },
    lay(letters, time, width, fit) {
      const t = time - since;
      const h = rows * style.cell * fit; // a letter's height
      centre.set(style.left + width / 2, 0, style.line + h / 2);
      const upside = turned(time);
      const creeping = egg === "creeper" && t < FUSE + BLAST + BEAT + REWIND;
      // How far into its flight the blast is: on to its end, held there, and back to where it began.
      const fly = !creeping || t < FUSE ? 0 : t < FUSE + BLAST ? t - FUSE : t < FUSE + BLAST + BEAT ? BLAST : BLAST * (1 - smooth((t - FUSE - BLAST - BEAT) / REWIND));
      seen.clear();
      letters.forEach(({ mesh, ch }, i) => {
        seen.add(mesh);
        mesh.rotation.set(0, 0, 0);
        if (upside !== 0) {
          // Upside down: the whole line turned half round in its own plane, about its middle.
          const angle = Math.PI * upside;
          mesh.position.sub(centre).applyAxisAngle(Y, angle).add(centre);
          mesh.rotation.y = angle;
        }
        if (egg === "rainbow") {
          // Each letter through the dyes in turn, blending from one to the next, a little behind the letter before it.
          const u = t / WOOL - i * RIPPLE;
          const k = Math.floor(u);
          const a = DYES[wrap(k, 16)];
          const b = DYES[wrap(k + 1, 16)];
          const f = u - k;
          FACES.forEach((face, j) => {
            for (let c = 0; c < 3; c += 1) dye[face][c] = lerp(a[c], b[c], f) * SHADES[j];
          });
          paint(mesh, dye);
          rimmed(mesh, ch);
        } else if (creeping && t < FUSE) {
          // The fuse: it swells (wider more than taller, trembling, but not so wide the letters run together), and flashes white quicker and quicker.
          const g = t / FUSE;
          const tremble = 1 + Math.sin(g * 100) * g * 0.02;
          const swell = g ** 3;
          mesh.scale.multiply(swollen.set((1 + 0.22 * swell) * tremble, (1 + 0.22 * swell) * tremble, (1 + 0.12 * swell) / tremble));
          const blinks = 1.6 * t + (2.6 * t * t) / FUSE; // (from under twice a second to seven times)
          paint(mesh, own, g > 0.12 && Math.sin(blinks * 2 * Math.PI) > 0.2 ? 0.45 + 0.5 * g : 0);
        } else if (creeping && fly > 0) {
          // The blast: out from the middle and up, toward us, turning over as it falls; white for its first moment.
          // Once down on the underline, it lies where it fell, as the house's blocks do.
          const { out, toward, aside, up, spin } = flightOf(i);
          const side = clamp01(Math.abs(mesh.position.x - centre.x) / Math.max(width / 2, h)) * Math.sign(mesh.position.x - centre.x);
          const drop = (mesh.position.z - (style.line - GROUND * style.cell)) / h;
          const down = Math.min(fly, (up + Math.sqrt(up * up + 2 * FALL * drop)) / FALL);
          mesh.position.x += (side * out + aside) * down * h;
          mesh.position.y -= toward * down * h;
          mesh.position.z += (up * down - 0.5 * FALL * down * down) * h;
          // (Turning about its own middle, not its foot.)
          euler.set(spin.x * down, spin.y * down, spin.z * down);
          mesh.rotation.copy(euler);
          spun.setFromEuler(euler);
          pivot.set(0, 0, (h / 2) * (mesh.scale.z / fit));
          mesh.position.add(pivot).sub(pivot.applyQuaternion(spun));
          const flash = t - FUSE < 0.14 ? 1 - (t - FUSE) / 0.14 : 0;
          if (flash > 0) paint(mesh, own, flash);
          else plain(mesh);
        } else plain(mesh);
      });
      // Letters typed away since go back to sharing their glyph.
      if (owned.size) for (const mesh of [...owned.keys()]) if (!seen.has(mesh)) plain(mesh);
      if (puffs.length || fly > 0) smoke(fly, heart.set(centre.x, -0.2 * h, style.line + 0.35 * h), h);
    },
  };
}
