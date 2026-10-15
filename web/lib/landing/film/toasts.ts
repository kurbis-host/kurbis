/**
 * The toasts' corner of the interface: one toast at a time. Each is sent in either as the
 * scroll reaches a place, or by something that happens there (a tree is punched). A new
 * one sends off the one showing; each goes when its time is up, or when the scroll leaves
 * the stretch it belongs to. What an event tells is told once a visit.
 * (How a toast looks, and where the corner is, are the interface's: ./interface.ts.)
 */

import { easeIn, easeOut } from "../anim/easing";

/** Seconds a toast takes to slide in, and to slide out; and how long it stays between. */
export const TRAVEL = 0.3;
const STAY = 2.6;

export interface Toast<C> {
  card: C;
  /** The stretch of scroll it belongs to: outside it, it goes. */
  from: number;
  until: number;
  /** Sent in by the scroll as it passes `from`; else by something that happened. */
  scrolled: boolean;
  /** The moment it was sent in, and the moment it was sent off. */
  shownAt: number | null;
  leftAt: number | null;
}

export class Toasts<C> {
  /** The scrolled toasts, then those events have sent that have not gone yet. */
  readonly list: Toast<C>[];
  /** What events have told this visit. */
  private readonly told = new Set<string>();
  /** The scroll position at the last frame. */
  private before = 0;

  constructor(scrolled: readonly { card: C; from: number; until: number }[]) {
    this.list = scrolled.map(({ card, from, until }) => ({ card, from, until, scrolled: true, shownAt: null, leftAt: null }));
  }

  /**
   * Something happened: its toast comes in now. Not if `key` was told before this visit
   * (unless `again`), nor while the scroll is outside from..until. True if it came.
   */
  send(key: string, card: () => C, time: number, { from, until, again = false }: { from: number; until: number; again?: boolean }): boolean {
    if ((this.told.has(key) && !again) || this.before < from || this.before >= until) return false;
    this.told.add(key);
    this.sendOff(time);
    this.list.push({ card: card(), from, until, scrolled: false, shownAt: time, leftAt: null });
    return true;
  }

  /** Each frame: the toasts that show, and how far out of their place each is (0 in it, 1 out of sight). */
  update(s: number, time: number, jumped: boolean): [C, number][] {
    const showing: [C, number][] = [];
    for (const toast of this.list) {
      if (toast.scrolled && !jumped && this.before < toast.from && s >= toast.from) {
        this.sendOff(time);
        toast.shownAt = time;
        toast.leftAt = null;
      }
      if (toast.shownAt === null) continue;
      if (jumped && (s >= toast.until || s < toast.from)) {
        // (The scroll was taken somewhere else at once: the toast does not slide out over the new place.)
        toast.shownAt = null;
        continue;
      }
      const age = time - toast.shownAt;
      if (toast.leftAt === null && (age >= TRAVEL + STAY || s >= toast.until || s < toast.from)) toast.leftAt = time;
      let away = 1 - easeOut(age / TRAVEL, 3);
      if (toast.leftAt !== null) {
        if (time - toast.leftAt > TRAVEL) {
          toast.shownAt = null;
          continue;
        }
        const reached = 1 - easeOut((toast.leftAt - toast.shownAt) / TRAVEL, 3); // how far out it still was when sent off
        away = reached + (1 - reached) * easeIn((time - toast.leftAt) / TRAVEL, 3);
      }
      showing.push([toast.card, away]);
    }
    // (An event's toast that has gone is done with: the scroll does not bring it back.)
    for (let i = this.list.length - 1; i >= 0; i -= 1) if (!this.list[i].scrolled && this.list[i].shownAt === null) this.list.splice(i, 1);
    this.before = s;
    return showing;
  }

  private sendOff(time: number) {
    for (const other of this.list) if (other.shownAt !== null && other.leftAt === null) other.leftAt = time;
  }
}
