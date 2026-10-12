/**
 * The film: the stage, moved by two clocks. Scroll position (in screens) drives the
 * camera and the story; real time drives life and reactions.
 */

import { Quaternion, Vector3, WebGLRenderer, type Object3D } from "three";
import { fieldOfView, shotAt, type CameraState } from "../anim/shot";
import { Timeline } from "../anim/timeline";
import { Post } from "../engine/post";
import type { Stage } from "../engine/stage";
import { setCave, type Cave } from "./cave";
import { makeCast, type Cast } from "./cast";
import { makeContext, type Ctx } from "./context";
import { setDig } from "./dig";
import { setDungeon } from "./dungeon";
import { makeGrade } from "./grade";
import { setInterface } from "./interface";
import { setCaveLife, setMeadowLife } from "./life";
import { setNether, type NameField } from "./nether";
import { setRide } from "./ride";
import { setSurface } from "./surface";
import { Touch } from "./touch";
import { setValley } from "./valley";

/** Seconds over which stepped scrolling (a wheel, the keyboard) is eased. */
const EASE = 0.08;
/**
 * The stretch of scroll over which each big piece of the world can be seen. Outside it
 * the piece is not drawn at all: the frustum alone would keep the valley while the
 * camera is under it.
 */
const SEEN: [string, number, number][] = [
  ["valley.terrain", 0, 3.2],
  ["valley.leaves", 0, 2.3],
  ["valley.water", 0, 2.3],
  ["under.terrain", 1.4, 5.3],
  ["under.water", 2.3, 4.3],
  ["dungeon.", 3.6, 5.3],
  ["nether.terrain", 5.1, 99],
];

/**
 * Where a film that does not fly stands (for a visitor who asked for less motion): the
 * stops, and one look at each band of the dig. It steps from one to the next under a
 * short dip to dark.
 */
const STANDS = [0.12, 1.3, 1.95, 2.36, 3.4, 4.22, 4.62, 6.2];
const DIP = 0.22;
/** A frame that takes longer than this (milliseconds) is a slow one; this many seconds of them, and the film draws less. */
const SLOW = 24;
const PATIENCE = 1.5;
/** How much of the screen's pixels are drawn, step by step down. Bloom goes first. */
const SHARES = [1, 0.8, 0.64, 0.5];

/** Most pixels drawn, whatever the screen: about a 1440p frame. */
const MAX_PIXELS = 2560 * 1440;

export class Film {
  readonly renderer: WebGLRenderer;
  readonly post: Post;
  /** Where the scroll is, and where the picture is (it follows, eased). */
  target = 0;
  s = 0;
  /** Seconds of real time the film has run. */
  time = 0;
  readonly tl = new Timeline();
  readonly ctx: Ctx;
  readonly cast: Cast;
  readonly cave: Cave;
  /** The name field at the end, for the page's input to drive. */
  readonly name: NameField;
  readonly touch = new Touch();
  /** The page's to set: how the pointer should look, and how to take the scroll somewhere at once. */
  onCursor: (cursor: Touch["cursor"]) => void = () => {};
  onJump: (screens: number) => void = () => {};
  /** Enter was pressed on the interface's own key (a hand without a keyboard). */
  onEnter: () => void = () => {};
  /** The cave went to sleep, or woke: whoever pulled the lever. */
  onCave: (awake: boolean) => void = () => {};
  private caveAwake = true;
  private cursor: Touch["cursor"] = "";
  private leap: { at: number; to: number; done: boolean } | null = null;
  /** Less motion: the camera stands at the stops instead of flying between them. */
  calm = false;
  private stand: number | null = null;
  private step: { at: number; to: number } | null = null;
  /** How far the film has turned itself down to keep its pace: 0 is everything on. */
  private level = 0;
  private slow = 0;
  /** How long frames take, smoothed (milliseconds from one to the next), and what the last one drew. */
  readonly pace = { frame: 16.7, calls: 0, triangles: 0 };
  private frame = 0;
  private last = 0;
  private dt = 0;
  private readonly state: CameraState = { eye: new Vector3(), quat: new Quaternion(), lens: 28 };
  private readonly end: number;
  private readonly grade: (s: number) => void;

  constructor(
    private readonly canvas: HTMLCanvasElement,
    readonly stage: Stage,
    /** The canvas the interface is drawn on, over the film. */
    overlay: HTMLCanvasElement | null = null,
    /** The canvas's own WebGL2 context, if the page has made it already (to find out whether it can). */
    context?: WebGL2RenderingContext,
  ) {
    this.renderer = new WebGLRenderer({ canvas, context, antialias: false, alpha: false, powerPreference: "high-performance" });
    this.renderer.autoClear = true;
    this.renderer.info.autoReset = false; // counted over the whole frame, not its last pass
    this.post = new Post(this.renderer, 4);
    this.end = stage.data.constants.END;
    this.grade = makeGrade(stage, this.post);
    this.ctx = makeContext(stage, this.tl, this.touch, makeCast);
    this.cast = this.ctx.cast;
    setValley(this.ctx);
    setSurface(this.ctx);
    setMeadowLife(this.ctx);
    setDig(this.ctx);
    this.cave = setCave(this.ctx);
    setCaveLife(this.ctx);
    setRide(this.ctx);
    setDungeon(this.ctx);
    this.name = setNether(this.ctx);
    const pieces: { count: number; list: [Object3D, number, number][] } = { count: -1, list: [] };
    this.tl.always(({ s, time }) => {
      if (pieces.count !== stage.nodes.size) {
        // (The world below the valley arrives after the hero is up.)
        pieces.count = stage.nodes.size;
        pieces.list = [];
        for (const [name, group] of stage.nodes) {
          const seen = SEEN.find(([prefix]) => name.startsWith(prefix));
          if (seen) pieces.list.push([group, seen[1], seen[2]]);
        }
      }
      for (const [group, s0, s1] of pieces.list) group.visible = s >= s0 && s <= s1;
      const { sleeper } = this.cave;
      this.cast.pumpkin.update(s, time);
      this.cast.nether.update(s, time);
      stage.lamps.set("Kurbis_Candle", this.cast.candle(s) * sleeper.candle);
      stage.lamps.set("Kurbis_Back", this.cast.back(s));
      stage.lamps.set("Kurbis_Nether_Candle", this.cast.netherCandle(s));
    });
    // The shortcut: under the portal's swirl, straight to the end.
    this.touch.button("shortcut", (time) => {
      if (!this.leap) this.leap = { at: time, to: this.end, done: false };
    });
    this.touch.button("enter", () => this.onEnter());
    this.tl.always(() => {
      const awake = this.cave.awake();
      if (awake !== this.caveAwake) {
        this.caveAwake = awake;
        this.onCave(awake);
      }
    });
    this.tl.always((frame) => {
      this.touch.update(frame, stage.camera, this.ctx.ui, window.devicePixelRatio || 1, [this.canvas.clientWidth, this.canvas.clientHeight]);
      if (this.touch.cursor !== this.cursor) {
        this.cursor = this.touch.cursor;
        this.onCursor(this.cursor);
      }
    });
    if (overlay) {
      void setInterface(this.ctx, overlay).then((ui) => {
        this.ctx.ui = ui;
        this.resize();
        this.tl.always((frame) => ui.update(frame)); // last, once every part has had its say
      });
    }
    this.resize();
  }

  /** The scroll position in screens, from the page. */
  setScroll(screens: number, instant = false) {
    if (!Number.isFinite(screens)) return; // (a window with no height yet has no scroll position to give)
    this.target = Math.min(Math.max(screens, 0), this.end);
    if (instant) {
      this.s = this.target;
      this.stand = null; // (a film that stands takes its place afresh)
      this.tl.settle(this.s);
    }
  }

  resize() {
    const width = this.canvas.clientWidth || 1;
    const height = this.canvas.clientHeight || 1;
    const ratio = Math.min(window.devicePixelRatio || 1, 2, Math.sqrt(MAX_PIXELS / (width * height))) * SHARES[Math.max(this.level - 1, 0)];
    this.renderer.setPixelRatio(ratio);
    this.renderer.setSize(width, height, false);
    this.post.setSize(Math.round(width * ratio), Math.round(height * ratio));
    this.ctx.ui?.hud.resize(width, height, window.devicePixelRatio || 1);
    this.stage.camera.aspect = width / height;
  }

  start() {
    if (this.frame) return;
    this.last = performance.now();
    const tick = (now: number) => {
      this.frame = requestAnimationFrame(tick);
      const dt = Math.min((now - this.last) / 1000, 0.1);
      this.pace.frame += (now - this.last - this.pace.frame) * 0.05;
      this.last = now;
      this.keepPace(dt);
      this.advance(dt);
      this.draw();
    };
    this.frame = requestAnimationFrame(tick);
  }

  stop() {
    cancelAnimationFrame(this.frame);
    this.frame = 0;
  }

  advance(dt: number) {
    this.dt = dt;
    this.time += dt;
    this.stage.globals.uCalm.value = this.calm ? 1 : 0;
    if (!this.calm) {
      this.stand = null;
      this.step = null;
      this.s += (this.target - this.s) * (1 - Math.exp(-dt / EASE));
      if (Math.abs(this.target - this.s) < 1e-5) this.s = this.target;
      return;
    }
    // Standing: at the stop the scroll is nearest to. Nothing plays catch-up on the way.
    const to = STANDS.reduce((best, stop) => (Math.abs(stop - this.target) < Math.abs(best - this.target) ? stop : best));
    if (this.stand === null) {
      this.stand = to;
      this.s = to;
      this.tl.settle(to);
    } else if (to !== this.stand && !this.step) {
      this.step = { at: this.time, to };
    }
    if (this.step) {
      const age = this.time - this.step.at;
      if (age >= DIP && this.stand !== this.step.to) {
        this.stand = this.step.to;
        this.s = this.stand;
        this.tl.settle(this.s);
      }
      if (age >= 2 * DIP) this.step = null;
    }
  }

  /** Turn the film down a step if frames have been slow for a while: bloom first, then pixels. */
  private keepPace(dt: number) {
    if (this.time < 4 || this.level >= SHARES.length) return;
    this.slow = this.pace.frame > SLOW ? this.slow + dt : 0;
    if (this.slow < PATIENCE) return;
    this.slow = 0;
    this.pace.frame = 16.7;
    this.level += 1;
    this.post.enabled = false;
    if (this.level > 1) this.resize();
  }

  draw() {
    const { stage, state } = this;
    const { camera, globals } = stage;
    shotAt(stage.shots, this.s).at(this.s, state);
    camera.position.copy(state.eye);
    camera.quaternion.copy(state.quat);
    camera.fov = fieldOfView(state.lens, camera.aspect, stage.tall);
    camera.updateProjectionMatrix();
    camera.updateMatrixWorld();
    globals.uEye.value.copy(state.eye);
    globals.uTime.value = this.time;
    this.grade(this.s);
    if (this.calm) {
      // No flight through the cloud, no bending lens, no swirl: the dip stands in for all three.
      const age = this.step ? this.time - this.step.at : 2 * DIP;
      this.post.covers.dark = Math.max(1 - Math.abs(age - DIP) / DIP, 0);
      this.post.covers.white = 0;
      this.post.covers.wobble = 0;
      this.post.covers.swirl = 0;
    } else this.post.covers.dark = 0;
    if (this.leap) {
      // Cover the frame, move under the cover, uncover.
      const age = this.time - this.leap.at;
      if (age >= 0.3 && !this.leap.done) {
        this.leap.done = true;
        this.onJump(this.leap.to);
      }
      const cover = age < 0.3 ? age / 0.3 : 1 - Math.max(age - 0.5, 0) / 0.4;
      if (this.calm) this.post.covers.dark = Math.max(this.post.covers.dark, cover);
      else this.post.covers.swirl = Math.max(this.post.covers.swirl, cover);
      if (age > 0.9) this.leap = null;
    }
    this.tl.update(this.s, this.time, this.dt);
    stage.scene.updateMatrixWorld();
    stage.lamps.gather(globals, state.eye);
    this.renderer.info.reset();
    this.post.render(stage.scene, camera, this.time);
    this.pace.calls = this.renderer.info.render.calls;
    this.pace.triangles = this.renderer.info.render.triangles;
  }

  /** Take the film to one of its stops at once, under the portal's swirl (nothing, if it is there). */
  goTo(stop: "cave" | "nether") {
    const to = stop === "cave" ? 3.4 : this.end;
    if (Math.abs(this.target - to) < 0.12 || this.leap) return;
    this.leap = { at: this.time, to, done: false };
  }

  /** A hand on the cave's lever, from the page's own button: the film goes there first. */
  pull() {
    if (this.s < 3.3 || this.s > 3.55) {
      this.goTo("cave");
      window.setTimeout(() => this.cave.pull(this.time), 1100);
    } else this.cave.pull(this.time);
  }

  /** The pointer is at (x, y), shares of the canvas from its top left (null: it left). */
  pointer(x: number | null, y = 0, touch = false) {
    this.touch.move(x, y, touch);
  }

  /** A click, or a tap, where the pointer is. True if something answered. */
  press(): boolean {
    return this.touch.press(this.time, this.s, this.stage.camera, this.ctx.ui, window.devicePixelRatio || 1, [this.canvas.clientWidth, this.canvas.clientHeight]);
  }

  dispose() {
    this.stop();
    this.post.dispose();
    this.stage.dispose();
    this.renderer.dispose();
  }
}
