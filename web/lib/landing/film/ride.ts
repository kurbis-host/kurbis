/** Rails from the cave to the dungeon, the cart on them, and a few torches in the tunnel. */

import { Vector3 } from "three";
import { linear } from "../anim/easing";
import { railAt, railHeading, type Ctx } from "./context";

export function setRide(ctx: Ctx) {
  const { tl, points, ride, stage } = ctx;
  const cart = ctx.node("Cart");
  cart.rotation.order = "ZYX";
  const heading = new Vector3();
  const torches = linear([[3.2, 0], [3.3, 120], [4.2, 120], [4.3, 0]]);
  tl.during(3.1, 4.4, ({ s }) => {
    const far = ride.value(s);
    railAt(points, far, cart.position);
    railHeading(points, far, heading);
    cart.rotation.set(Math.asin(Math.max(Math.min(heading.z, 1), -1)), 0, Math.atan2(-heading.x, heading.y));
    for (let i = 0; i < 3; i += 1) stage.lamps.set(`Ride_TorchLight_${i}`, torches(s));
  });
}
