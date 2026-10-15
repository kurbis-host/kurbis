/**
 * The visitor in the film. When their server name is also their Minecraft name, they
 * take the place of one of the two friends: Steve, for a classic skin; Alex, for a slim
 * one (her arms are a pixel thinner). That friend wears their skin everywhere they
 * appear (the pond, the cave, the homestead) and goes by their name in the chat and on
 * their nametag; the other friend is the one who asks for the address. Called Dinnerbone
 * or Grumm, they stand on their head wherever they are, as in the game (./eggs.ts).
 */

import type { ShaderMaterial, Texture } from "three";
import { loadPicture } from "../engine/bundle";
import { fineCard, type Card } from "../engine/hud";
import type { Skin } from "../skin";
import { playersOf } from "../anim/player";
import type { Ctx, Friend } from "./context";
import { eggOf } from "./eggs";

/** Each friend's rigs, wherever they stand in the film; their skins' materials are named after them. */
export const RIGS: Record<Friend, string[]> = {
  Steve: ["Steve", "Cave_Steve", "Home_Steve"],
  Alex: ["Alex", "Cave_Alex", "Home_Alex"],
};

export interface Visitor {
  /** The visitor as a player in the film, wearing this skin (null: the friends are themselves again). */
  wear(name: string | null, skin: Skin | null): Promise<void>;
}

export function setVisitor(ctx: Ctx): Visitor {
  const { stage } = ctx;
  const looks = new Map<string, ShaderMaterial[]>();
  stage.scene.traverse((object) => {
    const material = (object as { material?: ShaderMaterial }).material;
    if (!material?.name || !material.uniforms?.uMap) return;
    const list = looks.get(material.name) ?? [];
    if (!list.includes(material)) list.push(material);
    looks.set(material.name, list);
  });
  const own = new Map<ShaderMaterial, Texture>();
  let worn: Texture | null = null;
  let asked = 0;

  const dress = (friend: Friend | null, texture: Texture | null, over: boolean) => {
    for (const [who, rigs] of Object.entries(RIGS) as [Friend, string[]][]) {
      for (const rig of rigs) {
        // A rig that acts has its own copy of its skin (anim/player.ts), painted with its face; one that does not wears the material's.
        const player = playersOf(stage).get(rig);
        player?.wear(who === friend && texture ? (texture.image as ImageBitmap) : null);
        player?.upsideDown(who === friend && over);
        for (const material of looks.get(`${rig}_skin`) ?? []) {
          if (!own.has(material)) own.set(material, material.uniforms.uMap.value as Texture);
          material.uniforms.uMap.value = who === friend && texture ? texture : own.get(material)!;
        }
      }
    }
  };

  return {
    async wear(name, skin) {
      const turn = ++asked;
      const texture = name && skin ? await loadPicture(skin.picture) : null;
      if (turn !== asked) {
        texture?.dispose(); // (a later name has come since)
        return;
      }
      const friend: Friend | null = texture && skin ? (skin.model === "slim" ? "Alex" : "Steve") : null;
      const over = friend !== null && !!name && eggOf(name) === "upsideDown";
      dress(friend, texture, over);
      worn?.dispose();
      worn = texture;
      ctx.players = { Steve: "Steve", Alex: "Alex", ...(friend && name ? { [friend]: name } : {}) };
      ctx.wearer = friend;
      const face = friend && skin ? await head(skin.picture, over) : null;
      if (turn !== asked) return; // (and a later name has its own head)
      ctx.face = face;
      ctx.ui?.renamed();
    },
  };
}

/** The head on a skin, as the game draws it in a list of players: the face, with the hat layer over it (upside down, `over`). */
async function head(picture: Blob, over: boolean): Promise<Card | null> {
  try {
    const skin = await createImageBitmap(picture);
    const canvas = document.createElement("canvas");
    canvas.width = 8;
    canvas.height = 8;
    const context = canvas.getContext("2d");
    if (!context) return null;
    context.imageSmoothingEnabled = false;
    if (over) context.setTransform(-1, 0, 0, -1, 8, 8); // (half round about its middle)
    context.drawImage(skin, 8, 8, 8, 8, 0, 0, 8, 8);
    context.drawImage(skin, 40, 8, 8, 8, 0, 0, 8, 8);
    return fineCard(canvas, 16, 16, 1);
  } catch {
    return null;
  }
}
