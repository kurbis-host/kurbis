/**
 * Rails from the cave to the dungeon, the cart on them, and a few torches in the tunnel.
 * As the pumpkin gets in, the action bar says what the game says on getting into a cart;
 * on the way through the tunnel comes the game's old achievement for riding one.
 */

import { Vector3 } from "three";
import { linear } from "../anim/easing";
import { COPY } from "../copy";
import { railAt, railHeading, type Ctx } from "./context";

export interface Ride {
  /** The visitor pressed Shift, the game's key for getting out: in the cart, the pumpkin tries. */
  sneak(time: number): void;
}

/** Where the pumpkin leaves the cave floor for the cart (./cast.ts). Back before it, it is out of the cart. */
const HOP = 3.57;
/** Seconds after the line on getting in before another pass over the hop may bring it again. */
const AGAIN = 2;
/**
 * The toast for the ride: how long after getting in it comes (so the two do not arrive at
 * once), the stretch it belongs to (the tunnel: the cave's headline is gone, and it is gone
 * before the homestead's first wants the corner), and the seconds it must have there, at
 * the pace the scroll goes, to be read at all.
 */
const RAIL = { after: 0.6, from: 3.7, until: 3.94, read: 1 };
/** The picture on the toast (a key of the art's HUD): the pack's minecart, as the old achievement had. */
const MINECART = "minecart";

export function setRide(ctx: Ctx): Ride {
  const { tl, C, points, ride, stage } = ctx;
  const cart = ctx.node("Cart");
  cart.rotation.order = "ZYX";
  const heading = new Vector3();
  const torches = linear([[3.2, 0], [3.3, 120], [4.2, 120], [4.3, 0]]);
  const dismount = (time: number) => ctx.ui?.actionBar(COPY.ride.dismount, time, { from: HOP, until: C.ARRIVE });
  // Getting in is told once a pass: the scroll must go back before the hop, and a little time pass, for the line to come again.
  let before = 0;
  let armed = true;
  let said = -Infinity;
  let boarded: number | null = null; // when the scroll put it in the cart, on a pass forward that is not through the tunnel yet
  tl.during(3.1, 4.4, ({ s, time, dt, jumped }) => {
    const far = ride.value(s);
    railAt(points, far, cart.position);
    railHeading(points, far, heading);
    cart.rotation.set(Math.asin(Math.max(Math.min(heading.z, 1), -1)), 0, Math.atan2(-heading.x, heading.y));
    for (let i = 0; i < 3; i += 1) stage.lamps.set(`Ride_TorchLight_${i}`, torches(s));

    if (s < HOP) armed = true;
    if (s >= C.BOARD && before < C.BOARD && !jumped) {
      boarded = time;
      if (armed && time - said >= AGAIN) {
        said = time;
        dismount(time);
      }
      armed = false;
    } else if (s < C.BOARD || s >= RAIL.until) boarded = null; // (coming back into the tunnel from the homestead is no ride)
    // Where the scroll will be in a moment, at the pace it goes: not if that is out of the stretch (it would be sent off before it could be read).
    const ahead = s + (dt > 0 ? (s - before) / dt : 0) * RAIL.read;
    if (boarded !== null && time - boarded >= RAIL.after && s >= RAIL.from && ahead >= RAIL.from && ahead < RAIL.until) {
      ctx.ui?.toast(COPY.toasts.events.rail, MINECART, time, { from: RAIL.from, until: RAIL.until });
    }
    before = s;
  });

  return {
    sneak(time) {
      const { s } = tl.frame;
      if (s < C.BOARD || s >= C.ARRIVE) return;
      // The scroll has it in the cart, so it cannot get out: a little hop where it sits, and the line again.
      ctx.cast.pumpkin.jump(time, 0.3, true);
      dismount(time);
    },
  };
}
