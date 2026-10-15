/**
 * A Java player's skin, by name: the visitor whose server name is also their Minecraft
 * name sees themselves in the film. The control plane asks Mojang (a browser may not);
 * here the skin is put into the current 64 by 64 layout, as the game does with an old one.
 */

export type Model = "classic" | "slim";

export interface Skin {
  /** The skin as a 64 by 64 PNG. */
  picture: Blob;
  model: Model;
}

/** The skin of the Java player with this name, or null if there is none to show (or no telling just now). */
export async function fetchSkin(name: string, signal?: AbortSignal): Promise<Skin | null> {
  const response = await fetch(`/api/v1/skins/${encodeURIComponent(name)}`, { signal });
  if (!response.ok) return null;
  const model: Model = response.headers.get("x-skin-model") === "slim" ? "slim" : "classic";
  const picture = await response.blob();
  const bitmap = await createImageBitmap(picture);
  if (bitmap.width !== 64 || (bitmap.height !== 64 && bitmap.height !== 32)) return null;
  if (bitmap.height === 64) return { picture, model };
  return { picture: await modernised(bitmap), model };
}

/**
 * An old skin (64 by 32) in the current layout: the left arm and leg are the right ones
 * mirrored, and a hat layer with no see-through pixel was never meant as one.
 */
async function modernised(old: ImageBitmap): Promise<Blob> {
  const canvas = document.createElement("canvas");
  canvas.width = 64;
  canvas.height = 64;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("landing: no 2D canvas for the skin");
  context.imageSmoothingEnabled = false;
  context.drawImage(old, 0, 0);
  const hat = context.getImageData(32, 0, 32, 16);
  if (hat.data.every((value, i) => i % 4 !== 3 || value === 255)) context.clearRect(32, 0, 32, 16);
  // Each limb's faces, mirrored one by one into the left limb's place: [x, y, w, h] from, and to.
  const limb = (fx: number, fy: number, tx: number, ty: number) => {
    const faces: [number, number, number, number, number, number][] = [
      [4, 0, 4, 4, 4, 0], // top
      [8, 0, 4, 4, 8, 0], // bottom
      [0, 4, 4, 12, 8, 4], // the outer side becomes the inner one
      [4, 4, 4, 12, 4, 4], // front
      [8, 4, 4, 12, 0, 4], // the inner side becomes the outer one
      [12, 4, 4, 12, 12, 4], // back
    ];
    for (const [x, y, w, h, dx, dy] of faces) {
      context.save();
      context.translate(tx + dx + w, ty + dy);
      context.scale(-1, 1);
      context.drawImage(canvas, fx + x, fy + y, w, h, 0, 0, w, h);
      context.restore();
    }
  };
  limb(0, 16, 16, 48); // leg
  limb(40, 16, 32, 48); // arm
  return await new Promise<Blob>((resolve, reject) => canvas.toBlob((blob) => (blob ? resolve(blob) : reject(new Error("landing: the skin would not encode"))), "image/png"));
}
