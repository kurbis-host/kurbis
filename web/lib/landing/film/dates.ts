/**
 * The days the game itself marks, by the visitor's own calendar. The date is read here,
 * once, and nowhere else; on every other day the film is as it always is.
 *
 *   Halloween   October 31. The game's zombies and skeletons wear pumpkins on their
 *               heads, and its title screen says "OOoooOOOoooo! Spooky!" (./halloween.ts).
 *   Christmas   December 24 to 26. Every chest in the game is a present (./christmas.ts).
 *
 * And the year, which time turned back at the homestead counts back from (./homestead.ts);
 * and the moment a screenshot is taken, which names it (F2: ./film.ts).
 */

export type Day = "halloween" | "christmas";

/** Which of those days `date` is, in local time (null: neither). */
export function dayOf(date: Date): Day | null {
  const month = date.getMonth() + 1;
  const day = date.getDate();
  if (month === 10 && day === 31) return "halloween";
  if (month === 12 && day >= 24 && day <= 26) return "christmas";
  return null;
}

/** Which of them it is today, where the visitor is. */
export function today(): Day | null {
  return dayOf(new Date());
}

/** The year it is, where the visitor is. */
export function thisYear(): number {
  return new Date().getFullYear();
}

/** A screenshot's file name, as the game names one: the moment it was taken, in local time (2026-10-15_14.03.22.png). */
export function screenshotName(date: Date = new Date()): string {
  const two = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${two(date.getMonth() + 1)}-${two(date.getDate())}_${two(date.getHours())}.${two(date.getMinutes())}.${two(date.getSeconds())}.png`;
}
