/**
 * A curve through keys [s, value]: monotone cubic, so it never overshoots and comes to
 * rest wherever two neighbouring keys agree. Flat before the first and after the last
 * key. With ends "free" it keeps moving through the first and last key.
 */

export interface TrackData {
  t: number[];
  v: number[][];
  ends?: string;
}

export type Key = readonly [number, number | readonly number[]];

export class Track {
  readonly t: number[];
  readonly v: number[][];
  private readonly m: number[][];
  readonly size: number;

  constructor(keys: readonly Key[], ends: "rest" | "free" = "rest") {
    const sorted = [...keys].sort((a, b) => a[0] - b[0]);
    this.t = sorted.map((key) => key[0]);
    this.v = sorted.map((key) => (typeof key[1] === "number" ? [key[1]] : [...key[1]]));
    this.size = this.v[0].length;
    const n = this.t.length;
    this.m = this.v.map(() => new Array<number>(this.size).fill(0));
    if (n < 2) return;
    const h: number[] = [];
    const d: number[][] = [];
    for (let i = 0; i < n - 1; i += 1) {
      h.push(this.t[i + 1] - this.t[i]);
      d.push(this.v[i].map((value, c) => (this.v[i + 1][c] - value) / h[i]));
    }
    for (let i = 1; i < n - 1; i += 1) {
      const w1 = 2 * h[i] + h[i - 1];
      const w2 = h[i] + 2 * h[i - 1];
      for (let c = 0; c < this.size; c += 1) {
        const a = d[i - 1][c];
        const b = d[i][c];
        this.m[i][c] = a * b > 0 ? (w1 + w2) / (w1 / a + w2 / b) : 0;
      }
    }
    if (ends === "free") {
      this.m[0] = [...d[0]];
      this.m[n - 1] = [...d[n - 2]];
    }
  }

  static from(data: TrackData): Track {
    return new Track(
      data.t.map((t, i) => [t, data.v[i]] as const),
      data.ends === "free" ? "free" : "rest",
    );
  }

  /** The value at s, written into `out` (or a new array). */
  at(s: number, out: number[] = new Array<number>(this.size)): number[] {
    const { t, v, m } = this;
    const n = t.length;
    if (n === 1 || s <= t[0]) {
      for (let c = 0; c < this.size; c += 1) out[c] = v[0][c];
      return out;
    }
    if (s >= t[n - 1]) {
      for (let c = 0; c < this.size; c += 1) out[c] = v[n - 1][c];
      return out;
    }
    let i = 0;
    while (t[i + 1] <= s) i += 1;
    const h = t[i + 1] - t[i];
    const u = (s - t[i]) / h;
    const u2 = u * u;
    const u3 = u2 * u;
    const h00 = 2 * u3 - 3 * u2 + 1;
    const h10 = u3 - 2 * u2 + u;
    const h01 = -2 * u3 + 3 * u2;
    const h11 = u3 - u2;
    for (let c = 0; c < this.size; c += 1) {
      out[c] = h00 * v[i][c] + h10 * h * m[i][c] + h01 * v[i + 1][c] + h11 * h * m[i + 1][c];
    }
    return out;
  }

  /** The value at s of a track with one number per key. */
  value(s: number): number {
    return this.at(s, scratch)[0];
  }

  /** Where a rising one-value track reaches `value` (by bisection). */
  when(value: number): number {
    let lo = this.t[0];
    let hi = this.t[this.t.length - 1];
    for (let i = 0; i < 40; i += 1) {
      const mid = (lo + hi) / 2;
      if (this.value(mid) < value) lo = mid;
      else hi = mid;
    }
    return hi;
  }
}

const scratch: number[] = [];
