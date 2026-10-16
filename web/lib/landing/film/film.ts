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
import { setChat, type Chat } from "./chat";
import { makeCast, type Cast } from "./cast";
import { makeContext, type Ctx } from "./context";
import { setChristmas } from "./christmas";
import { screenshotName, today } from "./dates";
import { setDebug, type Debug } from "./debug";
import { setDig } from "./dig";
import { setHalloween } from "./halloween";
import { setHomestead } from "./homestead";
import { makeGrade } from "./grade";
import { setInterface } from "./interface";
import { setCaveLife, setMeadowLife } from "./life";
import { setNether, type NameField } from "./nether";
import { setEnding } from "./ending";
import { setMaps } from "./maps";
import type { FleetStats } from "../stats";
import { LEAD, setCredits, type Credits } from "./credits";
import { Ring } from "./ring";
import { setRide, type Ride } from "./ride";
import { setSighting } from "./sighting";
import { setSkyField, type SkyField } from "./sky-field";
import { setSurface, type Surface } from "./surface";
import { setVisitor, type Visitor } from "./visitor";
import type { Skin } from "../skin";
import type { Status } from "./interface";
import { Touch } from "./touch";
import { COPY } from "../copy";
import { WHITE } from "../engine/hud";
import { setValley } from "./valley";
import { setWeather } from "./weather";

/** Seconds over which stepped scrolling (a wheel, the keyboard) is eased. */
const EASE = 0.08;
/**
 * The stretch of scroll over which each big piece of the world can be seen. Outside it
 * the piece is not drawn at all: the frustum alone would keep the valley while the
 * camera is under it.
 */
const seen = (home: number): [string, [number, number][]][] => [
  // (The valley again from `home`: through the second frame, the page comes back to it.)
  ["valley.terrain", [[0, 3.2], [home - 0.02, 99]]],
  ["valley.leaves", [[0, 2.3], [home - 0.02, 99]]],
  ["valley.water", [[0, 2.3], [home - 0.02, 99]]],
  ["under.terrain", [[1.4, 5.3]]],
  ["under.water", [[2.3, 4.3]]],
  ["dungeon.", [[3.6, 5.3]]],
  ["nether.terrain", [[5.1, home + 0.02]]],
  ["Ending_", [[home - 0.02, 99]]],
];

/** A frame that takes longer than this (milliseconds) is a slow one; this many seconds of them, and the film draws less. */
const SLOW = 24;
const PATIENCE = 1.5;
/** How much of the screen's pixels are drawn, step by step down. Bloom goes first. */
const SHARES = [1, 0.8, 0.64, 0.5];

/** Most pixels drawn, whatever the screen: about a 1440p frame. */
const MAX_PIXELS = 2560 * 1440;

/** How much of the frame a leap covers, `age` seconds in: covered in 0.3 s, it moves, and uncovers from 0.5 to 0.9 s. */
function leapCover(age: number): number {
  return age < 0.3 ? age / 0.3 : Math.max(1 - Math.max(age - 0.5, 0) / 0.4, 0);
}

export class Film {
  readonly renderer: WebGLRenderer;
  readonly post: Post;
  /**
   * Where the scroll is on the page's ring (./ring.ts), and where the picture is (it follows,
   * eased): in screens from the sky, and before it in the lead-in, where the cover lifts on it.
   * And where the story is: as the picture, but at the sky all through the lead-in.
   */
  target = 0;
  at = 0;
  s = 0;
  /** Seconds of real time the film has run. */
  time = 0;
  readonly tl = new Timeline();
  readonly ctx: Ctx;
  readonly cast: Cast;
  readonly cave: Cave;
  private readonly ride: Ride;
  /** The name field at the end, and the one in the sky, for the page's inputs to drive. */
  readonly name: NameField;
  readonly sky: SkyField;
  private readonly surface: Surface;
  private visitor: string | null = null;
  private readonly you: Visitor;
  readonly touch = new Touch();
  /** The page's to set: how the pointer should look, and how to take the scroll somewhere on the ring at once. */
  onCursor: (cursor: Touch["cursor"]) => void = () => {};
  onJump: (s: number) => void = () => {};
  /** Enter was pressed on the interface's own key (a hand without a keyboard). */
  onEnter: () => void = () => {};
  /** The bar's "Get your server": the page takes the visitor to the name field. */
  onGet: () => void = () => {};
  /** The bar's "Sign in", or another link drawn in the film: the page goes there. */
  onLink: (href: string) => void = (href) => window.location.assign(href);
  /** A link to somebody else's page (in the credits): it opens in a tab of its own. */
  onOpen: (href: string) => void = (href) => void window.open(href, "_blank", "noopener,noreferrer");
  /** The claim card's buttons: copy the address (true if it was), or sign in with the name. */
  onCopy: (address: string) => Promise<boolean> = async () => false;
  onClaimSignIn: () => void = () => {};
  /** The claim card's "Open panel", for a visitor who is back with a server of their own. */
  onPanel: () => void = () => {};
  /** The claim card's own close button was pressed. */
  onClaimClose: () => void = () => {};
  /** The cave went to sleep, or woke: whoever pulled the lever. */
  onCave: (awake: boolean) => void = () => {};
  /** The service's figures, as the wall of maps at the end has them (./maps.ts): the page's own HTML says them too. */
  onStats: (stats: FleetStats) => void = () => {};
  /** A screenshot was taken (F2): the page saves it, as a download of that name unless it does more. */
  onSave: (picture: Blob, name: string) => void = (picture, name) => {
    const link = document.createElement("a");
    link.href = URL.createObjectURL(picture);
    link.download = name;
    link.click();
    window.setTimeout(() => URL.revokeObjectURL(link.href), 1000);
  };
  /** The game's debug screen and hitboxes (F3, F3+B: ./debug.ts). */
  readonly debug: Debug;
  /** The game's chat, which "/" opens (./chat.ts): the page hands it the keyboard while it is open. */
  readonly chat: Chat;
  /** The interface is put away (F1); a screenshot is wanted (F2), of the next frame drawn. */
  private bare = false;
  private shoot = false;
  private caveAwake = true;
  private cursor: Touch["cursor"] = "";
  private leap: { at: number; to: number; done: boolean } | null = null;
  /** How far the film has turned itself down to keep its pace: 0 is everything on. */
  private level = 0;
  private slow = 0;
  /** How long frames take, smoothed (milliseconds from one to the next), and what the last one drew. */
  readonly pace = { frame: 16.7, calls: 0, triangles: 0 };
  private frame = 0;
  private drawn = false;
  private last = 0;
  private dt = 0;
  private readonly state: CameraState = { eye: new Vector3(), quat: new Quaternion(), lens: 28 };
  /** The page's ring; where the tail past the credits ends (and the ring comes round), and where the credits come to rest, in screens. */
  readonly ring: Ring;
  readonly end: number;
  readonly rolled: number;
  private readonly credits: Credits;
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
    this.ring = new Ring(stage.data.constants.END);
    this.rolled = this.ring.rolled;
    this.end = this.ring.last;
    this.grade = makeGrade(stage, this.post);
    this.ctx = makeContext(stage, this.tl, this.touch, makeCast);
    this.cast = this.ctx.cast;
    setValley(this.ctx);
    this.sky = setSkyField(this.ctx);
    setSighting(this.ctx);
    this.surface = setSurface(this.ctx);
    setMeadowLife(this.ctx);
    setDig(this.ctx);
    this.cave = setCave(this.ctx);
    this.ctx.cave = this.cave;
    setCaveLife(this.ctx);
    this.ride = setRide(this.ctx);
    setHomestead(this.ctx);
    this.name = setNether(this.ctx);
    this.ctx.go = (href) => this.onLink(href);
    this.ctx.open = (href) => this.onOpen(href);
    setEnding(this.ctx);
    setMaps(this.ctx, (stats) => this.onStats(stats));
    this.credits = setCredits(this.ctx);
    // On the days the game itself marks, what it does on them; on every other day, nothing.
    const day = today();
    if (day === "halloween") setHalloween(this.ctx, this.sky);
    if (day === "christmas") setChristmas(this.ctx);
    // The chat's commands: night and rain over the open air (laid over the grade, after every part has lit its stop), and the rest.
    this.chat = setChat(this.ctx, setWeather(this.ctx, this.post), (to) => this.onJump(to));
    this.ctx.chat = this.chat;
    const SEEN = seen(stage.data.constants.HOME);
    const pieces: { count: number; list: [Object3D, [number, number][]][] } = { count: -1, list: [] };
    this.tl.always(({ s, time }) => {
      if (pieces.count !== stage.nodes.size) {
        // (The world below the valley arrives after the hero is up.)
        pieces.count = stage.nodes.size;
        pieces.list = [];
        for (const [name, group] of stage.nodes) {
          const found = SEEN.find(([prefix]) => name.startsWith(prefix));
          if (found) pieces.list.push([group, found[1]]);
        }
      }
      for (const [group, spans] of pieces.list) group.visible = spans.some(([s0, s1]) => s >= s0 && s <= s1);
      const { sleeper } = this.cave;
      this.cast.pumpkin.update(s, time);
      this.cast.nether.update(s, time);
      stage.lamps.set("Kurbis_Candle", this.cast.candle(s) * sleeper.candle);
      stage.lamps.set("Kurbis_Back", this.cast.back(s));
      stage.lamps.set("Kurbis_Nether_Candle", this.cast.netherCandle(s));
    });
    // The bar: the mark goes back to the sky; the ways in are the page's to follow.
    this.touch.button("home", () => this.goTo("sky"));
    this.touch.button("get", () => this.onGet());
    this.touch.button("sign-in", () => this.onLink("/login"));
    this.touch.button("enter", () => this.onEnter());
    this.touch.button("claim-copy", (time) => {
      const name = this.ctx.ui?.claimed;
      if (name) void this.onCopy(`${name}${COPY.surface.host}`).then((done) => done && this.ctx.ui?.copied(time));
    });
    this.touch.button("claim-sign-in", () => this.onClaimSignIn());
    this.touch.button("claim-close", () => {
      this.unclaim();
      this.onClaimClose();
    });
    this.touch.button("claim-panel", () => this.onPanel());
    this.tl.always(() => {
      const awake = this.cave.awake();
      if (awake !== this.caveAwake) {
        this.caveAwake = awake;
        this.onCave(awake);
      }
    });
    this.tl.always((frame) => {
      // (Under the film's cover, where the page comes round, nothing of the world can be pointed at or pressed.)
      if ((this.ctx.ui?.cover ?? 0) > 0.5) this.touch.screen = true;
      this.touch.update(frame, stage.camera, this.ctx.ui, window.devicePixelRatio || 1, [this.canvas.clientWidth, this.canvas.clientHeight]);
      if (this.touch.cursor !== this.cursor) {
        this.cursor = this.touch.cursor;
        this.onCursor(this.cursor);
      }
    });
    this.you = setVisitor(this.ctx);
    this.debug = setDebug(this.ctx, { renderer: this.renderer, end: this.end }); // (once every creature is in)
    if (overlay) {
      void setInterface(this.ctx, overlay).then((ui) => {
        this.ctx.ui = ui;
        ui.hidden = this.bare; // (put away before it was in)
        ui.named(this.visitor); // (a name picked before the interface was in)
        ui.renamed();
        this.resize();
        this.tl.always((frame) => ui.update(frame)); // last, once every part has had its say
      });
    }
    this.resize();
  }

  /**
   * Hold the film where it is, whatever the page's scroll does (a phone's keyboard is
   * open over a name field: the browser scrolls to make room, and the film must not fly
   * off). Returns where it was held, for the page to scroll back to.
   */
  hold(on: boolean): number {
    const at = this.held ?? this.target;
    this.held = on ? this.target : null;
    return at;
  }
  private held: number | null = null;

  /** Where the scroll is on the ring, in screens from the sky, from the page (which knows where that is on it: ./ring.ts). */
  setScroll(s: number, instant = false) {
    if (!Number.isFinite(s)) return; // (a window with no height yet has no scroll position to give)
    const to = Math.min(Math.max(s, -LEAD), this.end);
    if (this.held !== null) {
      if (!instant) return; // (only a jump of the film's own moves it while it is held)
      this.held = to;
    }
    this.target = to;
    if (instant) {
      this.place(to);
      this.tl.settle(this.s);
    }
  }

  /** The picture is there on the ring; the story is there too, or at the sky while the picture is before it. */
  private place(at: number) {
    this.at = at;
    this.s = Math.max(at, 0);
    this.ctx.lead = Math.max(-at, 0);
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
      if (this.shoot) {
        this.shoot = false;
        this.save();
      }
      if (!this.drawn) {
        this.drawn = true;
        performance.mark("landing:frame"); // (the first one, which is where the shaders are compiled)
      }
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
    const was = this.at;
    this.follow(dt);
    // Gone round the ring under the film's cover (./ring.ts): from the end of the credits to the
    // sky, or back. Nothing on either side plays catch-up for the other.
    if (Math.abs(this.at - was) > this.ring.round / 2) {
      this.ctx.again = this.time;
      this.tl.settle(this.s);
    }
  }

  /** The picture follows the scroll: eased, the short way round the ring. */
  private follow(dt: number) {
    // (The short way is over the seam, where the tail's end and the lead-in's top are one place, all cover.)
    const { round, last } = this.ring;
    let ahead = this.target - this.at;
    if (Math.abs(ahead) > round / 2) ahead -= Math.sign(ahead) * round;
    let at = Math.abs(ahead) < 1e-5 ? this.target : this.at + ahead * (1 - Math.exp(-dt / EASE));
    if (at >= last) at -= round;
    else if (at < -LEAD) at += round;
    this.place(at);
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

  /** For looking into the film while making it (never set in production): a camera of its own, from `eye` toward `at`. */
  inspect: { eye: Vector3; at: Vector3; lens: number } | null = null;

  draw() {
    const { stage, state } = this;
    const { camera, globals } = stage;
    shotAt(stage.shots, this.s).at(this.s, state);
    if (this.inspect) {
      state.eye.copy(this.inspect.eye);
      camera.position.copy(this.inspect.eye);
      camera.up.set(0, 0, 1);
      camera.lookAt(this.inspect.at);
      state.quat.copy(camera.quaternion);
      state.lens = this.inspect.lens;
    }
    camera.position.copy(state.eye);
    camera.quaternion.copy(state.quat);
    camera.fov = fieldOfView(state.lens, camera.aspect, stage.tall);
    camera.updateProjectionMatrix();
    camera.updateMatrixWorld();
    globals.uEye.value.copy(state.eye);
    globals.uTime.value = this.time;
    this.grade(this.s);
    if (this.leap) {
      // Cover the frame, move under the cover, uncover.
      const age = this.time - this.leap.at;
      if (age >= 0.3 && !this.leap.done) {
        this.leap.done = true;
        this.onJump(this.leap.to);
      }
      this.post.covers.swirl = Math.max(this.post.covers.swirl, leapCover(age));
      if (age > 0.9) this.leap = null;
    }
    // Past "The End" the film's cover comes over the credits as the scroll goes on, and over
    // the lead-in on the other side of it, it lifts on the sky (./ring.ts). Scrolled up, the
    // same backwards.
    const cover = this.ring.cover(this.at);
    if (this.ctx.ui) this.ctx.ui.cover = Math.max(this.ctx.ui.cover, cover);
    this.tl.update(this.s, this.time, this.dt);
    // The interface sits behind the portal's swirl: as it fills the frame, the interface goes under it.
    const under = this.ctx.ui ? String(Math.round((1 - this.post.covers.swirl) * 100) / 100) : "";
    if (this.ctx.ui && this.ctx.ui.hud.canvas.style.opacity !== under) this.ctx.ui.hud.canvas.style.opacity = under;
    // Under the credits' night, or the cover, nothing of the world shows: it is not drawn at all.
    if (this.ctx.ui && !this.inspect && (cover >= 1 || this.credits.dark(this.s) >= 1)) {
      this.pace.calls = 0;
      this.pace.triangles = 0;
      return;
    }
    stage.scene.updateMatrixWorld();
    stage.lamps.gather(globals, state.eye);
    this.renderer.info.reset();
    this.post.render(stage.scene, camera, this.time);
    this.pace.calls = this.renderer.info.render.calls;
    this.pace.triangles = this.renderer.info.render.triangles;
  }

  /** Take the film to one of its stops at once, under the portal's swirl (nothing, if it is there). */
  goTo(stop: "sky" | "cave" | "nether") {
    const to = stop === "sky" ? 0 : stop === "cave" ? 3.4 : this.stage.data.constants.NAME_STOP;
    // A leap still under its cover goes to the new place instead; one that has landed may be followed by another.
    if (this.leap && !this.leap.done) {
      this.leap.to = to;
      return;
    }
    if (Math.abs(this.target - to) < 0.12) return;
    // (Following one that is still uncovering, it covers again from as far as that one still covers.)
    const covered = this.leap ? leapCover(this.time - this.leap.at) : 0;
    this.leap = { at: this.time - covered * 0.3, to, done: false };
  }

  /** The visitor's own name, once it is found free: the film calls their server by it from then on (null: its own names). */
  named(name: string | null) {
    this.visitor = name;
    this.ctx.visitor = name;
    this.surface.name(name, this.time);
    this.ctx.ui?.named(name);
  }

  /** The visitor as a player in the film, in the skin of the Java player by their name (null: the friends as they were). */
  wear(name: string | null, skin: Skin | null): Promise<void> {
    return this.you.wear(name, skin);
  }

  /** Show the claim card for the name as it stands, and what is known of it (it opens if it was not open); or close it. */
  claim(name: string, status: Status) {
    this.ctx.ui?.claim(name, this.time, status);
  }

  unclaim() {
    this.ctx.ui?.unclaim(this.time);
  }

  /** The visitor is back, and the name is theirs already: the bar offers their server. */
  home(back: boolean) {
    this.ctx.home = back;
  }

  /** A hand on the cave's lever, from the page's own button: the film goes there first. */
  pull() {
    if (this.s < 3.3 || this.s > 3.55) {
      this.goTo("cave");
      window.setTimeout(() => this.cave.pull(this.time), 1100);
    } else this.cave.pull(this.time);
  }

  /** Shift was pressed, the game's key for getting out of a cart (the ride answers it while the pumpkin is in one). */
  sneak() {
    this.ride.sneak(this.time);
  }

  /** F1: the interface put away, for the film alone, or brought back. A click still reaches the world. */
  toggleInterface() {
    this.bare = !this.bare;
    if (this.ctx.ui) this.ctx.ui.hidden = this.bare;
  }

  /** F2: a screenshot, of the next frame as soon as it is drawn. */
  screenshot() {
    this.shoot = true;
  }

  /**
   * The frame as it is seen, the interface over the film (unless F1 put it away), saved as
   * the game saves a screenshot: a PNG named for the moment; and the chat says so, as the
   * game's does. The film's picture is read back now, just after it was drawn and before the
   * browser shows it and lets it go. Keeping every frame's picture for later instead
   * (`preserveDrawingBuffer`) would cost a copy on every frame, for a key seldom pressed.
   */
  private save() {
    const hud = this.ctx.ui?.hud.canvas ?? null;
    const shot = document.createElement("canvas");
    shot.width = hud?.width || this.canvas.width;
    shot.height = hud?.height || this.canvas.height;
    const paint = shot.getContext("2d");
    if (!paint) return;
    paint.drawImage(this.canvas, 0, 0, shot.width, shot.height);
    if (hud) {
      // (As much of the interface as shows: it goes under the portal's swirl.)
      paint.globalAlpha = hud.style.opacity === "" ? 1 : Number(hud.style.opacity);
      paint.drawImage(hud, 0, 0, shot.width, shot.height);
    }
    const name = screenshotName();
    shot.toBlob((picture) => {
      if (!picture) return;
      this.onSave(picture, name);
      this.ctx.ui?.say([{ text: `${COPY.keys.screenshot} ` }, { text: name, underline: true }], WHITE, this.time, {});
    }, "image/png");
  }

  /** The pointer is at (x, y), shares of the canvas from its top left (null: it left). */
  pointer(x: number | null, y = 0, touch = false) {
    this.touch.move(x, y, touch);
  }

  /** A click, or a tap, where the pointer is. True if something answered. */
  press(): boolean {
    return this.touch.press(this.time, this.s, this.stage.camera, this.ctx.ui, window.devicePixelRatio || 1, [this.canvas.clientWidth, this.canvas.clientHeight]);
  }

  /** Whether a tooltip with a link is up (a free name's splash, pointed at or tapped: ./splash.ts). */
  get reading(): boolean {
    return !!this.touch.hovered?.link;
  }

  dispose() {
    this.stop();
    this.post.dispose();
    this.stage.dispose();
    this.renderer.dispose();
  }
}
