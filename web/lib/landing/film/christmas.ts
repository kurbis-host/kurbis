/**
 * Christmas (./dates.ts), as the game keeps it: from December 24 to 26 every chest is a
 * wrapped present. Every chest in the film wears the art's Chest material (the dig's, the
 * cave's); for those days it takes the picture the art made ChestChristmas with. What
 * they say they are stays as it is: the game still calls it a chest.
 */

import type { Mesh, ShaderMaterial } from "three";
import type { Ctx } from "./context";

export function setChristmas(ctx: Ctx) {
  const { stage } = ctx;
  const present = stage.data.materials.ChestChristmas?.image;
  const picture = present ? stage.pictures.named.get(present) : undefined;
  if (!picture) return;
  stage.scene.traverse((object) => {
    const material = (object as Mesh).material as ShaderMaterial | undefined;
    if (material?.name === "Chest" && material.uniforms?.uMap) material.uniforms.uMap.value = picture;
  });
}
