/**
 * An item as the game draws one in the world, dropped or flying to whoever fished it up:
 * its picture made solid, a pixel thick, as the art makes the held ones (`item` in
 * art/landing/scripts/kurbis/boxmodel.py). Each pixel that is there is a little block:
 * a face in front and one behind, and an edge wherever the pixel beside it is clear.
 *
 * Its picture may come in layers, as the game's leather does: each laid over the one
 * before, and multiplied by a colour of its own if it has one (the leather's).
 */

import { BufferAttribute, BufferGeometry, DataTexture, NearestFilter, SRGBColorSpace } from "three";
import { pixelsOf } from "./bundle";

/** One layer of an item's picture: a picture of the pack, and what it is multiplied by (0 to 255 a channel), if anything. */
export interface Layer {
  picture: Blob;
  tint?: readonly number[];
}

export interface Item {
  geometry: BufferGeometry;
  /** Its picture, as the models' shader takes one: nearest pixels, rows bottom first. */
  texture: DataTexture;
}

/**
 * An item `size` blocks across, standing up: across along X, up along Z, its front toward
 * -Y, its middle at the origin. (The layers' pictures are all the same size, as the game has them.)
 */
export async function makeItem(layers: readonly Layer[], size: number): Promise<Item> {
  const images = await Promise.all(layers.map(({ picture }) => pixelsOf(picture)));
  const { width, height } = images[0];
  // Each pixel is there or not, as in the game's cut-out pictures; a layer's covers the one under it.
  const pixels = new Uint8Array(width * height * 4); // (top row first, as the pictures are)
  images.forEach((image, k) => {
    const tint = layers[k].tint ?? [255, 255, 255];
    for (let i = 0; i < width * height; i += 1) {
      if (image.data[i * 4 + 3] < 128) continue;
      for (let c = 0; c < 3; c += 1) pixels[i * 4 + c] = Math.round((image.data[i * 4 + c] * tint[c]) / 255);
      pixels[i * 4 + 3] = 255;
    }
  });
  const there = (x: number, y: number) => x >= 0 && y >= 0 && x < width && y < height && pixels[(y * width + x) * 4 + 3] > 0;

  const px = size / width;
  const positions: number[] = [];
  const normals: number[] = [];
  const uvs: number[] = [];
  const index: number[] = [];
  /** A face of pixel (x, y)'s block, given as its corners in the pixel's own units (0..1 across, 0..1 up, -0.5..0.5 deep). */
  const face = (x: number, y: number, corners: readonly (readonly [number, number, number])[], normal: readonly [number, number, number]) => {
    const first = positions.length / 3;
    const at = corners.map(([a, b, d]) => [(x + a) * px - size / 2, d * px, size / 2 - (y + 1 - b) * px]);
    corners.forEach(([a, b], k) => {
      positions.push(...at[k]);
      normals.push(...normal);
      // Every face of it shows its own pixel, from well inside it (v counted from the top, as the models' shader takes it).
      uvs.push((x + 0.25 + 0.5 * a) / width, (y + 0.75 - 0.5 * b) / height);
    });
    // Wound to face out.
    const [p, q, r] = at;
    const [u, v] = [q.map((c, i) => c - p[i]), r.map((c, i) => c - p[i])];
    const out = (u[1] * v[2] - u[2] * v[1]) * normal[0] + (u[2] * v[0] - u[0] * v[2]) * normal[1] + (u[0] * v[1] - u[1] * v[0]) * normal[2];
    if (out > 0) index.push(first, first + 1, first + 2, first, first + 2, first + 3);
    else index.push(first, first + 2, first + 1, first, first + 3, first + 2);
  };
  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      if (!there(x, y)) continue;
      face(x, y, [[0, 0, -0.5], [1, 0, -0.5], [1, 1, -0.5], [0, 1, -0.5]], [0, -1, 0]);
      face(x, y, [[0, 0, 0.5], [1, 0, 0.5], [1, 1, 0.5], [0, 1, 0.5]], [0, 1, 0]);
      if (!there(x - 1, y)) face(x, y, [[0, 0, -0.5], [0, 1, -0.5], [0, 1, 0.5], [0, 0, 0.5]], [-1, 0, 0]);
      if (!there(x + 1, y)) face(x, y, [[1, 0, -0.5], [1, 1, -0.5], [1, 1, 0.5], [1, 0, 0.5]], [1, 0, 0]);
      if (!there(x, y - 1)) face(x, y, [[0, 1, -0.5], [1, 1, -0.5], [1, 1, 0.5], [0, 1, 0.5]], [0, 0, 1]);
      if (!there(x, y + 1)) face(x, y, [[0, 0, -0.5], [1, 0, -0.5], [1, 0, 0.5], [0, 0, 0.5]], [0, 0, -1]);
    }
  }
  const geometry = new BufferGeometry();
  geometry.setAttribute("position", new BufferAttribute(new Float32Array(positions), 3));
  geometry.setAttribute("normal", new BufferAttribute(new Float32Array(normals), 3));
  geometry.setAttribute("uv", new BufferAttribute(new Float32Array(uvs), 2));
  geometry.setIndex(index);

  // Rows bottom first, as the bundle's pictures are uploaded.
  const rows = new Uint8Array(pixels.length);
  for (let y = 0; y < height; y += 1) rows.set(pixels.subarray(y * width * 4, (y + 1) * width * 4), (height - 1 - y) * width * 4);
  const texture = new DataTexture(rows, width, height);
  texture.colorSpace = SRGBColorSpace;
  texture.magFilter = NearestFilter;
  texture.minFilter = NearestFilter;
  texture.generateMipmaps = false;
  texture.needsUpdate = true;
  return { geometry, texture };
}
