/** Small easing helpers, the same ones the previs is written with. */

export function clamp01(u: number): number {
  return Math.min(Math.max(u, 0), 1);
}

export function lerp(a: number, b: number, u: number): number {
  return a + (b - a) * u;
}

export function smooth(u: number): number {
  const c = clamp01(u);
  return c * c * (3 - 2 * c);
}

export function easeIn(u: number, power = 2): number {
  return clamp01(u) ** power;
}

export function easeOut(u: number, power = 2): number {
  return 1 - (1 - clamp01(u)) ** power;
}

export function backOut(u: number, s = 1.7): number {
  const c = clamp01(u) - 1;
  return 1 + c * c * ((s + 1) * c + s);
}

/** Straight ramps between points [x, value], flat outside them. */
export function linear(points: readonly (readonly [number, number])[]): (x: number) => number {
  return (x) => {
    if (x <= points[0][0]) return points[0][1];
    const last = points[points.length - 1];
    if (x >= last[0]) return last[1];
    let i = 0;
    while (points[i + 1][0] <= x) i += 1;
    const [x0, v0] = points[i];
    const [x1, v1] = points[i + 1];
    return x1 > x0 ? v0 + ((v1 - v0) * (x - x0)) / (x1 - x0) : v1;
  };
}

function shape(keys: readonly (readonly [number, number])[]): (u: number) => number {
  return linear(keys);
}

/** Scale in from nothing with a little overshoot, over u in 0..1. */
export const POP = shape([[0, 0], [0.55, 1.14], [0.8, 0.97], [1, 1]]);
/** A small swell, then gone. */
export const UNPOP = shape([[0, 1], [0.3, 1.1], [1, 0]]);

/**
 * A value that loops for ever: smooth steps through keys [seconds into the loop, value],
 * and from the last key round to the first. Returns fn(seconds, out).
 */
export function cycle(period: number, keys: readonly (readonly [number, readonly number[]])[], phase = 0): (t: number, out: number[]) => number[] {
  const sorted = [...keys].sort((a, b) => a[0] - b[0]);
  const last = sorted[sorted.length - 1];
  const times = [last[0] - period, ...sorted.map((key) => key[0]), sorted[0][0] + period];
  const values = [last[1], ...sorted.map((key) => key[1]), sorted[0][1]];
  return (t, out) => {
    const u = (((t + phase) % period) + period) % period;
    let i = 0;
    while (i < times.length - 2 && times[i + 1] <= u) i += 1;
    const k = smooth((u - times[i]) / (times[i + 1] - times[i]));
    for (let c = 0; c < values[i].length; c += 1) out[c] = values[i][c] + (values[i + 1][c] - values[i][c]) * k;
    return out;
  };
}

/** A flame is never quite steady: a factor near 1 that wanders. */
export function flicker(time: number, phase: number): number {
  return 1 + 0.09 * Math.sin(time * 13.1 + phase) + 0.06 * Math.sin(time * 29.3 + 2.3 * phase) + 0.04 * Math.sin(time * 5.7 + 0.7 * phase);
}

/** A stream of numbers in 0..1 that is the same on every visit. */
export function seeded(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** The remainder that is never negative. */
export function wrap(value: number, period: number): number {
  return ((value % period) + period) % period;
}
