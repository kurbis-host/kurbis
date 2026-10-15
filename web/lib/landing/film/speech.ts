/**
 * What the friends say when clicked: a line in a speech bubble over their head, in the
 * manner of the game's nametag. Each place has its own lines for each friend (../copy.ts),
 * shuffled once a visit and said in turn, round again when they run out; back at a place,
 * they go on where they left off. A place may have one said first (the cave's mines), and
 * some said only once something has happened there (Herobrine gone, the house back), at
 * the next clicks after it. What happens may have them say a line too (a boot on the
 * line), in place of whatever they were saying, as a click would.
 *
 * A bubble follows its speaker's head, over their nametag if they have one, and keeps
 * off the bar along the top, the place's headline, and whatever else the interface has
 * drawn. Where there is no room over the nametag it takes the next best place: between
 * a raised nametag and the head, else beside the head, pointing at it. It pops in, stays
 * a few seconds and fades; a second click says the next line at once. Two can show at
 * once where they do not overlap; where they would, the older one goes. What is said
 * belongs to its place: when the scroll leaves it, it goes.
 */

import { Vector3, type Box3, type Mesh, type Object3D, type PerspectiveCamera } from "three";
import { POP, clamp01, lerp } from "../anim/easing";
import { BUBBLE_TAIL, LINE, type Card, type Draw, type Hud } from "../engine/hud";
import type { Tip } from "./interface";

/** Seconds a bubble stays before it fades, and fades over; how fast one that is sent off goes. */
const STAY = 3;
const FADE = 0.4;
const GONE = 0.15;
/** Interface pixels kept to the frame's edges, and between a bubble and what it keeps off; between its tip and what it points at. */
const EDGE = 4;
const ROOM = 1;
const GAP = 1;
/** How far a tip may come down onto a bare head where there is no room over it. */
const DROP = 2;
/** Interface pixels kept between two bubbles, so that two side by side read as two (nearer than half that, they overlap). */
const APART = 5;

/** A box on the interface: [left, bottom, right, top], in interface pixels from the bottom left. */
type Box = readonly [number, number, number, number];

/** Lines said only once something has happened where they are said. */
export interface After {
  /** When it last happened, or null while it has not (or no longer holds: the house is whole again). */
  since: () => number | null;
  /** What is said of it: one at each click after it, in order, and then the place's own lines again. */
  lines: readonly string[];
}

/**
 * One place's lines for one friend, in turn: shuffled once, said in that order, and round
 * again when they run out. One may be said first, and comes round with the rest after.
 */
export class Turns {
  private readonly deck: string[];
  private next = 0;
  private last: string | null = null;
  /** For each thing that has happened, when it did, and how many of its lines have been said since. */
  private readonly told = new Map<After, { at: number; said: number }>();

  constructor(lines: readonly string[], first: string | null = null, random: () => number = Math.random) {
    const deck = [...new Set(lines)].filter((line) => line !== first);
    for (let i = deck.length - 1; i > 0; i -= 1) {
      const j = Math.floor(random() * (i + 1));
      [deck[i], deck[j]] = [deck[j], deck[i]];
    }
    this.deck = first === null ? deck : [first, ...deck];
  }

  /** The next line: what is still to be said of something that has happened (`after`), else the next in turn. */
  take(after: readonly After[] = []): string {
    for (const one of after) {
      const at = one.since();
      if (at === null) continue;
      let told = this.told.get(one);
      if (told?.at !== at) this.told.set(one, (told = { at, said: 0 }));
      if (told.said < one.lines.length) return (this.last = one.lines[told.said++]);
    }
    for (let tries = 0; tries < this.deck.length; tries += 1) {
      const line = this.deck[this.next];
      this.next = (this.next + 1) % this.deck.length;
      if (line !== this.last) return (this.last = line);
    }
    return (this.last = this.deck[0]); // (only one line to say here, and it was just said)
  }

  /** A line said for a reason of its own (a boot came up): the next turn is not that again. */
  said(line: string) {
    this.last = line;
  }
}

/** Someone who talks, in one place. */
export interface Speaker {
  /** The art's rig they talk from: its head, and its nametag if it has one (a tip `of` it). */
  rig: string;
  /** The stretch of scroll their place is in sight over. */
  seen: readonly [number, number];
  /** Whether they are there at all: one who has left says nothing more. */
  here?: () => boolean;
  /** The place's headline, standing in the world: their bubble keeps off it. */
  headline: readonly (Object3D | null)[];
  /** What they say here, in turn (a list of COPY.talk's): it goes on where it left off whenever they are back. */
  lines: readonly string[];
  /** Said the first time they talk here. */
  first?: string;
  /** What they say once something has happened here. */
  after?: readonly After[];
}

/**
 * Where a bubble may go: over something with its tail down to it (the nametag, or the
 * head), its tip at height `y` anywhere over `x0..x1`; or to one side of the head, its
 * tip at (`x`, `y`).
 */
type Way = { side: "over"; x0: number; x1: number; y: number } | { side: "left" | "right"; x: number; y: number };

/** A bubble laid out: its lines, its box's size, and its card's bottom left. */
interface Spot {
  lines: string[];
  width: number;
  height: number;
  left: number;
  bottom: number;
}

interface Bubble {
  speaker: Speaker;
  text: string;
  /** The moment it was said; whether it took the place of a line before it; seconds it stays before it fades. */
  at: number;
  again: boolean;
  stay: number;
  /** The moment it was sent off, if it has been. */
  gone: number | null;
  /** How it was put last frame (its way and lines), and where its card's bottom left was from its way's tip. */
  how: string;
  offset: [number, number];
  /** A better way it could go, and since when it could. */
  better: { how: string; since: number } | null;
  /** Its cards, by layout. */
  cards: Map<string, Card>;
  /** Where it was put this frame, if it was. */
  box: Box | null;
}

const corner = new Vector3();

export class Speech {
  /** Each place's lines for each friend, in turn: by the list. */
  private readonly turns = new Map<readonly string[], Turns>();
  private readonly speakers: Speaker[] = [];
  /** Oldest first. */
  private bubbles: Bubble[] = [];

  constructor(
    private readonly camera: PerspectiveCamera,
    private readonly nodes: ReadonlyMap<string, Object3D>,
  ) {}

  /** Someone who talks somewhere. What it returns has them say their next line (on a click). */
  speaker(speaker: Speaker): (time: number) => void {
    this.speakers.push(speaker);
    return (time) => this.say(speaker, time);
  }

  /**
   * Have someone who talks here (as `speaker` was given) say `line` now, because something
   * happened: in place of what they were saying, as a click would. A shout may be shorter
   * than a line said (`stay`, seconds before it fades).
   */
  tell(speaker: Speaker, time: number, line: string, stay = STAY) {
    this.say(speaker, time, line, stay);
  }

  private say(speaker: Speaker, time: number, line?: string, stay = STAY) {
    let turns = this.turns.get(speaker.lines);
    if (!turns) this.turns.set(speaker.lines, (turns = new Turns(speaker.lines, speaker.first)));
    const text = line ?? turns.take(speaker.after);
    if (line) turns.said(line);
    const showing = this.bubbles.find((bubble) => bubble.speaker === speaker && bubble.gone === null);
    // A second click while it shows says the next line in its place, at once (and it is the newest now).
    if (showing) this.bubbles.splice(this.bubbles.indexOf(showing), 1);
    this.bubbles.push({ speaker, text, at: time, again: !!showing, stay, gone: null, how: "", offset: [0, 0], better: null, cards: new Map(), box: null });
  }

  /**
   * Each frame, once the rest of the interface is drawn: every bubble by its speaker,
   * clear of what the interface has drawn (`hud.placed`, which has the nametags among
   * `tips`), of its place's headline and the other heads there, and of the frame above
   * `ceiling` (the bar).
   */
  update(s: number, time: number, dt: number, hud: Hud, draw: Draw, tips: readonly Tip[], ceiling: number) {
    if (!this.bubbles.length) return;
    const taken: Box[] = [...hud.placed];
    const tags = new Map<string, Box>();
    for (const tip of tips) {
      if (!tip.of || tip.alpha <= 0.01) continue;
      const [left, bottom] = hud.where(tip.card, tip.at[0], tip.at[1], { anchor: tip.anchor, inside: 4 });
      tags.set(tip.of, [left, bottom, left + tip.card.width, bottom + tip.card.height]);
    }
    // Heads, and each place's headline, once a frame.
    const heads = new Map<string, Box | null>();
    const headOf = (rig: string) => {
      if (!heads.has(rig)) heads.set(rig, this.headOf(rig, hud));
      return heads.get(rig)!;
    };
    const inSight = this.speakers.filter((one) => s >= one.seen[0] && s < one.seen[1] && (one.here?.() ?? true));
    const headlines = new Map<Speaker["headline"], Box[]>();
    const placed: Bubble[] = [];
    const frame = { width: hud.width, ceiling };

    for (const bubble of this.bubbles) {
      const { speaker } = bubble;
      bubble.box = null;
      // Gone with its place, or with its speaker.
      if (bubble.gone === null && (s < speaker.seen[0] || s >= speaker.seen[1] || !(speaker.here?.() ?? true))) bubble.gone = time;
      const age = time - bubble.at;
      const alpha = clamp01(age / 0.06) * (1 - clamp01((age - bubble.stay) / FADE)) * (bubble.gone === null ? 1 : 1 - clamp01((time - bubble.gone) / GONE));
      if (alpha <= 0) continue;
      const tag = tags.get(speaker.rig) ?? null;
      const head = headOf(speaker.rig);
      if (!tag && !head) continue; // (behind the camera)

      // Where it may go, best first: over the nametag; over the head (between it and a
      // nametag raised over another, or onto a bare head); beside the head, on the side
      // toward the middle of the frame first.
      const ways: Way[][] = [];
      if (tag) ways.push([{ side: "over", x0: tag[0] + 2, x1: tag[2] - 3, y: tag[3] + GAP }]);
      if (head) {
        const over: Way = { side: "over", x0: head[0] + 1, x1: head[2] - 2, y: head[3] + GAP };
        ways.push(tag ? [over] : [over, { ...over, y: over.y - DROP }]);
        const middle = (head[1] + head[3]) / 2;
        const sides: Way[] = [{ side: "right", x: head[2] + GAP, y: middle }, { side: "left", x: head[0] - GAP, y: middle }];
        ways.push((head[0] + head[2]) / 2 > hud.width / 2 ? sides.reverse() : sides);
      }
      let headline = headlines.get(speaker.headline);
      if (!headline) headlines.set(speaker.headline, (headline = this.boxesOf(speaker.headline, hud)));
      // What it must keep off, and what it keeps off if it can: the other friends' faces,
      // and the other bubbles (where it cannot, the older one will go).
      const hard = [...taken, ...headline];
      const others = [...new Set(inSight.map((one) => one.rig))].filter((rig) => rig !== speaker.rig).map(headOf);
      const bubbles = placed.filter((other) => other.gone === null && other.box).map(({ box }): Box => [box![0] - APART, box![1] - APART, box![2] + APART, box![3] + APART]);
      const soft = [...others.filter((box): box is Box => box !== null), ...bubbles];
      // One line where there is room for it, two where only that fits; it would rather
      // cover a face than leave the best way it has.
      const layouts = [[bubble.text], halves(bubble.text, (line) => draw.measure(line))].filter((lines, i) => i === 0 || lines.length === 2);
      const groups = ways.map((group, g) => layouts.flatMap((lines) => group.map((way, k) => ({ way, lines, how: `${g} ${k} ${lines.length}` }))));
      const first = (only?: string) => {
        for (const tries of groups)
          for (const keep of [[...hard, ...soft], hard])
            for (const { way, lines, how } of tries) {
              const spot = only === undefined || how === only ? fit(way, lines, keep, frame, draw) : null;
              if (spot) return { way, spot, how };
            }
        return null;
      };
      // It keeps the way it was put while that still fits, unless a better one has been
      // free for a second: it goes back over a friend who walks clear, but does not hop
      // with one who jumps.
      let found = first();
      const kept = found && bubble.how && found.how !== bubble.how ? first(bubble.how) : null;
      if (found && kept) {
        if (bubble.better?.how !== found.how) bubble.better = { how: found.how, since: time };
        if (time - bubble.better.since < 1) found = kept;
      }
      if (found?.how !== bubble.how || !kept) bubble.better = null;
      // (Nowhere clear at all: over its speaker anyway, inside the frame.)
      found ??= { way: ways[0][0], spot: anyhow(ways[0][0], layouts[0], frame, draw), how: "" };
      const { way, spot } = found;

      // It slides (rather than jumps) as what it keeps off moves; it follows its tip exactly.
      const [tx, ty] = way.side === "over" ? [(way.x0 + way.x1) / 2, way.y] : [way.x, way.y];
      const want: [number, number] = [spot.left - tx, spot.bottom - ty];
      const ease = found.how === bubble.how ? 1 - Math.exp(-dt / 0.08) : 1;
      bubble.how = found.how;
      bubble.offset = [lerp(bubble.offset[0], want[0], ease), lerp(bubble.offset[1], want[1], ease)];
      const over = way.side === "over";
      const cardWidth = spot.width + (over ? 0 : BUBBLE_TAIL);
      const cardHeight = spot.height + (over ? BUBBLE_TAIL : 0);
      // On whole screen pixels, its tail on whole interface pixels of it, so the two meet.
      const snap = (v: number) => Math.round(v * hud.scale) / hud.scale;
      const left = snap(Math.min(Math.max(tx + bubble.offset[0], EDGE), hud.width - EDGE - cardWidth));
      const bottom = snap(Math.max(ty + bubble.offset[1], EDGE));
      // The tail's point: over its target, or level with the head's middle.
      let key: string;
      let pivot: [number, number];
      let make: () => Card;
      if (way.side === "over") {
        const column = Math.min(Math.max(Math.floor(Math.min(Math.max(tx, way.x0), way.x1) - left), 1), spot.width - 2);
        const lean = column < BUBBLE_TAIL ? -1 : column > spot.width - 1 - BUBBLE_TAIL ? 1 : 0; // (too near an end for the whole tail)
        key = `under ${spot.lines.length} ${column} ${lean}`;
        pivot = [left + column + 0.5, bottom];
        make = () => draw.bubble(spot.lines, "under", column, lean);
      } else {
        const row = Math.min(Math.max(Math.floor(ty - bottom), BUBBLE_TAIL - 1), spot.height - BUBBLE_TAIL); // (from the bottom)
        const tail = way.side === "right" ? "left" : "right";
        key = `${tail} ${spot.lines.length} ${row}`;
        pivot = [tail === "left" ? left : left + cardWidth, bottom + row + 0.5];
        make = () => draw.bubble(spot.lines, tail, spot.height - 1 - row);
      }
      let card = bubble.cards.get(key);
      if (!card) bubble.cards.set(key, (card = make()));
      bubble.box = [left, bottom, left + cardWidth, bottom + cardHeight];

      // It pops in from its point (a new line in its place pops less: it was there already).
      const pop = bubble.again ? lerp(0.8, 1, POP(age / 0.16)) : POP(age / 0.2);
      const drawnLeft = pivot[0] - (pivot[0] - left) * pop;
      const drawnBottom = pivot[1] - (pivot[1] - bottom) * pop;
      hud.place(card, drawnLeft - (cardWidth * (1 - pop)) / 2, drawnBottom - (cardHeight * (1 - pop)) / 2, { alpha, size: pop });
      placed.push(bubble);
      // Where two would overlap, the older goes.
      for (const other of placed) if (other !== bubble && other.gone === null && other.box && meet(other.box, bubble.box, APART / 2)) other.gone = time;
    }
    this.bubbles = this.bubbles.filter((bubble) => {
      const age = time - bubble.at;
      return age < bubble.stay + FADE && (bubble.gone === null || time - bubble.gone < GONE);
    });
  }

  /**
   * Where a rig's head shows on the interface, as the box round all of it. (Stood on its
   * head, as Dinnerbone is, the box goes up to its feet: a bubble goes over those.) Null
   * behind the camera.
   */
  private headOf(rig: string, hud: Hud): Box | null {
    const head = this.nodes.get(`${rig}_head`) as Mesh | undefined;
    const root = this.nodes.get(rig);
    if (!head?.geometry || !root?.visible) return null;
    head.updateWorldMatrix(true, false);
    const box = this.boxOf(head, hud);
    if (!box || corner.setFromMatrixPosition(head.matrixWorld).z > root.position.z + 1) return box;
    corner.copy(root.position).setZ(root.position.z + 2.1).project(this.camera);
    return [box[0], box[1], box[2], Math.max(box[3], (corner.y * 0.5 + 0.5) * hud.height)];
  }

  /**
   * Where each piece of a headline in the world shows on the interface this frame (what is
   * not drawn has none), as [left, bottom, right, top]. (A toast keeps off one too: ./interface.ts.)
   */
  boxesOf(things: readonly (Object3D | null)[], hud: Hud): Box[] {
    const boxes: Box[] = [];
    for (const thing of things) {
      if (!thing?.visible) continue;
      thing.updateWorldMatrix(true, true);
      thing.traverseVisible((part) => {
        const box = (part as Mesh).isMesh && !part.name.endsWith("_Scrim") ? this.boxOf(part as Mesh, hud) : null; // (the glow behind the type may be covered)
        if (box) boxes.push(box);
      });
    }
    return boxes;
  }

  /** The box round a mesh as it shows on the interface; null if any of it is behind the camera. */
  private boxOf(mesh: Mesh, hud: Hud): Box | null {
    if (!mesh.geometry.boundingBox) mesh.geometry.computeBoundingBox();
    const box = mesh.geometry.boundingBox as Box3;
    let [left, bottom, right, top] = [Infinity, Infinity, -Infinity, -Infinity];
    for (let k = 0; k < 8; k += 1) {
      corner.set(k & 1 ? box.max.x : box.min.x, k & 2 ? box.max.y : box.min.y, k & 4 ? box.max.z : box.min.z).applyMatrix4(mesh.matrixWorld).project(this.camera);
      if (corner.z >= 1) return null;
      const x = (corner.x * 0.5 + 0.5) * hud.width;
      const y = (corner.y * 0.5 + 0.5) * hud.height;
      [left, bottom, right, top] = [Math.min(left, x), Math.min(bottom, y), Math.max(right, x), Math.max(top, y)];
    }
    return [left, bottom, right, top];
  }
}

/**
 * Where a bubble of these lines can go its way, clear of `keep` and inside the frame
 * (under its ceiling): as near the middle of where its tip may be as there is room for.
 * Null if nowhere.
 */
function fit(way: Way, lines: string[], keep: readonly Box[], frame: { width: number; ceiling: number }, draw: Draw): Spot | null {
  const width = Math.max(...lines.map((line) => draw.measure(line))) + 6;
  const height = lines.length * LINE;
  if (way.side === "over") {
    // Its card from the tip up; the tip somewhere along its bottom, over x0..x1.
    const top = way.y + BUBBLE_TAIL + height;
    if (way.y < EDGE || top > frame.ceiling) return null;
    const shut = keep.filter((box) => box[1] - ROOM < top && box[3] + ROOM > way.y).map((box) => [box[0] - ROOM - width, box[2] + ROOM] as const);
    const left = nearest(Math.max(way.x0 - width + 2, EDGE), Math.min(way.x1 - 1, frame.width - EDGE - width), (way.x0 + way.x1) / 2 - width / 2, shut);
    return left === null ? null : { lines, width, height, left, bottom: way.y };
  }
  // Beside the head, the tail out of its near side, its point level with the box's middle if it can be.
  const span = width + BUBBLE_TAIL;
  const left = way.side === "right" ? way.x : way.x - span;
  if (left < EDGE || left + span > frame.width - EDGE) return null;
  const shut = keep.filter((box) => box[0] - ROOM < left + span && box[2] + ROOM > left).map((box) => [box[1] - ROOM - height, box[3] + ROOM] as const);
  const bottom = nearest(Math.max(way.y - height + BUBBLE_TAIL, EDGE), Math.min(way.y - BUBBLE_TAIL + 1, frame.ceiling - height), way.y - height / 2, shut);
  return bottom === null ? null : { lines, width, height, left, bottom };
}

/** A bubble put its way whatever is there, inside the frame. */
function anyhow(way: Way, lines: string[], frame: { width: number; ceiling: number }, draw: Draw): Spot {
  const width = Math.max(...lines.map((line) => draw.measure(line))) + 6;
  const height = lines.length * LINE;
  const x = way.side === "over" ? (way.x0 + way.x1) / 2 : way.x;
  return { lines, width, height, left: Math.min(Math.max(x - width / 2, EDGE), frame.width - EDGE - width), bottom: Math.min(Math.max(way.y, EDGE), frame.ceiling - height - BUBBLE_TAIL) };
}

/** The place in from..to nearest `best` that no stretch in `shut` takes (their ends are free). */
function nearest(from: number, to: number, best: number, shut: readonly (readonly [number, number])[]): number | null {
  if (from > to) return null;
  let found: number | null = null;
  for (const at of [Math.min(Math.max(best, from), to), from, to, ...shut.flat()]) {
    if (at < from || at > to || shut.some(([a, b]) => at > a && at < b)) continue;
    if (found === null || Math.abs(at - best) < Math.abs(found - best)) found = at;
  }
  return found;
}

/** A line broken in two where the halves come out most even (one without a space stays whole). */
export function halves(text: string, measure: (line: string) => number): string[] {
  const words = text.split(" ");
  let best = [text];
  let widest = Infinity;
  for (let i = 1; i < words.length; i += 1) {
    const two = [words.slice(0, i).join(" "), words.slice(i).join(" ")];
    const wide = Math.max(...two.map(measure));
    if (wide < widest) [best, widest] = [two, wide];
  }
  return best;
}

function meet(a: Box, b: Box, room: number): boolean {
  return a[0] - room < b[2] && a[2] + room > b[0] && a[1] - room < b[3] && a[3] + room > b[1];
}
