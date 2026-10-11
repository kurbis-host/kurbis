import { useSyncExternalStore } from "react";
import { LAYERS } from "./constants";
import { SUN_JOKE, type BuriedId, type OreId } from "./copy";

export type CaveState = "dark" | "awake" | "countdown" | "snapshot";

export const TORCH_COUNT = 5;
const TORCH_STAGGER_MS = 220;
const COUNTDOWN_MS = 3200;
const SNAPSHOT_MS = 1100;
const TOAST_MS = 3800;

export interface Toast {
  id: number;
  text: string;
}

export interface LandingState {
  layer: number;
  visited: readonly number[];
  night: boolean;
  raining: boolean;
  joke: string | null;
  cave: CaveState;
  torches: readonly boolean[];
  /** 1 when the countdown starts, 0 when the snapshot is taken. */
  countdown: number;
  /** The cave went dark because the page sat idle, so any input relights it. */
  idleSlept: boolean;
  ore: OreId | null;
  found: readonly BuriedId[];
  lastFound: BuriedId | null;
  chestOpen: boolean;
  bumps: number;
  f3: boolean;
  toasts: readonly Toast[];
}

const noTorches: readonly boolean[] = Array.from({ length: TORCH_COUNT }, () => false);

const initial: LandingState = {
  layer: 0,
  visited: [0],
  night: false,
  raining: false,
  joke: null,
  cave: "dark",
  torches: noTorches,
  countdown: 1,
  idleSlept: false,
  ore: null,
  found: [],
  lastFound: null,
  chestOpen: false,
  bumps: 0,
  f3: false,
  toasts: [],
};

let state = initial;
const listeners = new Set<() => void>();

function set(patch: Partial<LandingState>) {
  state = { ...state, ...patch };
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function getLanding(): LandingState {
  return state;
}

export function useLanding<T>(selector: (current: LandingState) => T): T {
  return useSyncExternalStore(
    subscribe,
    () => selector(state),
    () => selector(initial),
  );
}

/**
 * Per-frame values the 3D scene reads without going through React.
 * `layerF` is the layer index plus the fraction scrolled toward the next one.
 */
export const scroll = {
  layerF: 0,
  worldY: LAYERS[0].worldY,
  yLevel: LAYERS[0].y,
  reducedMotion: false,
};

let toastId = 0;
let jokeTimer: ReturnType<typeof setTimeout> | undefined;
let rainTimer: ReturnType<typeof setTimeout> | undefined;
let caveTimers: ReturnType<typeof setTimeout>[] = [];
let caveInterval: ReturnType<typeof setInterval> | undefined;

function clearCaveTimers() {
  for (const timer of caveTimers) clearTimeout(timer);
  caveTimers = [];
  clearInterval(caveInterval);
  caveInterval = undefined;
}

export const landing = {
  setLayer(layer: number) {
    if (layer === state.layer) return;
    const first = !state.visited.includes(layer);
    set({ layer, visited: first ? [...state.visited, layer] : state.visited });
    const toast = LAYERS[layer]?.toast;
    if (first && toast) landing.toast(`Achievement get: ${toast}${/[.!?]$/.test(toast) ? "" : "."}`);
    if (first && LAYERS[layer]?.key === "cave" && state.cave === "dark") landing.playerJoins();
  },

  toast(text: string) {
    const id = ++toastId;
    set({ toasts: [...state.toasts.slice(-2), { id, text }] });
    setTimeout(() => landing.dismissToast(id), TOAST_MS);
  },

  dismissToast(id: number) {
    if (state.toasts.some((toast) => toast.id === id)) set({ toasts: state.toasts.filter((toast) => toast.id !== id) });
  },

  joke(text: string | null, holdMs = 2600) {
    clearTimeout(jokeTimer);
    if (text === state.joke) return;
    set({ joke: text });
    if (text) jokeTimer = setTimeout(() => set({ joke: null }), holdMs);
  },

  toggleNight() {
    const night = !state.night;
    set({ night });
    landing.joke(night ? SUN_JOKE : null, 4200);
  },

  rain() {
    clearTimeout(rainTimer);
    set({ raining: true });
    rainTimer = setTimeout(() => set({ raining: false }), 1400);
  },

  playerJoins() {
    clearCaveTimers();
    set({ cave: "awake", countdown: 1, idleSlept: false });
    for (let index = 0; index < TORCH_COUNT; index += 1) {
      if (state.torches[index]) continue;
      caveTimers.push(
        setTimeout(() => {
          set({ torches: state.torches.map((lit, torch) => lit || torch === index) });
        }, index * TORCH_STAGGER_MS),
      );
    }
  },

  everyoneLeaves(fromIdle = false) {
    if (state.cave !== "awake") return;
    clearCaveTimers();
    set({ cave: "countdown", countdown: 1, torches: state.torches.map(() => true) });
    const started = performance.now();
    caveInterval = setInterval(() => {
      const left = 1 - (performance.now() - started) / COUNTDOWN_MS;
      if (left > 0) {
        set({ countdown: left });
        return;
      }
      clearCaveTimers();
      set({ cave: "snapshot", countdown: 0 });
      caveTimers.push(setTimeout(() => set({ cave: "dark", torches: noTorches, idleSlept: fromIdle }), SNAPSHOT_MS));
    }, 50);
  },

  setOre(ore: OreId | null) {
    if (ore !== state.ore) set({ ore });
  },

  dig(id: BuriedId) {
    set({ lastFound: id, found: state.found.includes(id) ? state.found : [...state.found, id] });
  },

  toggleChest() {
    set({ chestOpen: !state.chestOpen });
  },

  bumpBedrock() {
    set({ bumps: state.bumps + 1 });
  },

  toggleF3() {
    set({ f3: !state.f3 });
  },

  /** Test hook. */
  reset() {
    clearCaveTimers();
    clearTimeout(jokeTimer);
    clearTimeout(rainTimer);
    state = initial;
    for (const listener of listeners) listener();
  },
};
