/**
 * The page is a ring: its scroll goes round for ever, either way. Scrolled down past "The
 * End", the tail (./credits.ts) takes the frame under the film's cover, and on the other side
 * of the cover a lead-in as short lifts it on the sky. Scrolled up past the sky, the same
 * backwards, onto the end of the credits. Both are scrubbed by the scroll, as the rest is.
 *
 * The page holds the ring once, from the top of the lead-in to the end of the tail, with a
 * copy of the tail over it and a copy of the lead-in under it. So the page opens at the sky
 * with the way up there to be scrolled, and a scroll that goes round has room to carry on, as
 * far as the end of the credits upward or the sky downward. The page's top is the end of the
 * credits; its foot is the sky (screens of scroll, for a film of 6.8):
 *
 *   page   0          0.3           0.6                         8.7      9.0             9.3
 *          | tail copy | lead-in     | the film, the credits     | tail    | lead-in copy  |
 *   s      8.1        8.4 = -0.3    0                           8.1     8.4 = -0.3       0
 *
 * A page in a copy is moved to the same place in the ring proper (`home`): the frame is the
 * same at both, so the move cannot be seen. The page does it once the scroll has come to
 * rest, as moving the scroll stops a phone's momentum; or at once, if the scroll has reached
 * the page's top or foot and has nowhere to go.
 */

import { clamp01 } from "../anim/easing";
import { AGAIN, HOLD, LEAD, ROLL } from "./credits";

/** Where the film ends (the art's END: home's last view), for the page's length before the film's own numbers are in. */
export const FILM = 6.8;

export class Ring {
  /** Where the credits come to rest ("The End"), and where the tail ends under the cover: the ring comes round there, to the top of the lead-in (-LEAD). In screens of scroll from the sky. */
  readonly rolled: number;
  readonly last: number;
  /** Once round. */
  readonly round: number;
  /** Where the ring proper begins on the page (under the tail's copy), and how far the page scrolls in all. */
  readonly top = AGAIN;
  readonly length: number;

  /** The ring of a film that ends `end` screens from the sky. */
  constructor(end: number) {
    this.rolled = end + ROLL;
    this.last = this.rolled + AGAIN;
    this.round = this.last + LEAD;
    this.length = this.top + this.round + LEAD;
  }

  /** The scroll position (screens from the sky, -LEAD up to `last`) at a place on the page (screens from its top). */
  at(page: number): number {
    const along = (page - this.top) % this.round;
    // (To a millionth of a screen, far finer than a pixel: the page's place for "The End" is "The End", not a hair before it.)
    return Math.round(((along < 0 ? along + this.round : along) - LEAD) * 1e6) / 1e6;
  }

  /** The place on the page for a scroll position, in the ring proper. */
  page(s: number): number {
    return this.top + LEAD + s;
  }

  /** The same place in the ring proper, for a place on the page (the place itself, if it is in it). */
  home(page: number): number {
    if (page < this.top) return page + this.round;
    if (page > this.top + this.round) return page - this.round;
    return page;
  }

  /**
   * How much of the frame the film's cover hides at a scroll position: coming over the
   * credits past "The End", all of it either side of where the ring comes round, and lifting
   * on the sky by the end of the lead-in. It comes and goes softly at "The End" and at the
   * sky, and briskly by the seam, so the scroll there is not spent on a frame of cover alone.
   */
  cover(s: number): number {
    return clamp01(s < 0 ? -s / (LEAD - HOLD) : (s - this.rolled) / (AGAIN - HOLD)) ** 2;
  }
}
