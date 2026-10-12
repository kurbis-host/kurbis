/**
 * The bundle's small files (pictures, mostly), which come as one: fifty requests of a few
 * hundred bytes each would cost far more in asking than in bytes.
 *
 *   'KPK1', the header's length (uint32), the header (JSON, padded to four bytes):
 *   {"files": {name: [offset, length]}}, then the files' bytes end to end.
 */

export interface Pack {
  has(name: string): boolean;
  bytes(name: string): Uint8Array;
  /** A picture of the pack, as something a browser can decode. */
  picture(name: string): Blob;
}

export function readPack(buffer: ArrayBuffer): Pack {
  const view = new DataView(buffer);
  if (view.getUint32(0, false) !== 0x4b504b31) throw new Error("landing: not a pack");
  const length = view.getUint32(4, true);
  const { files } = JSON.parse(new TextDecoder().decode(new Uint8Array(buffer, 8, length))) as { files: Record<string, [number, number]> };
  const bytes = (name: string) => {
    const entry = files[name];
    if (!entry) throw new Error(`landing: the pack has no ${name}`);
    return new Uint8Array(buffer, 8 + length + entry[0], entry[1]);
  };
  return { has: (name) => name in files, bytes, picture: (name) => new Blob([bytes(name) as Uint8Array<ArrayBuffer>], { type: "image/png" }) };
}
