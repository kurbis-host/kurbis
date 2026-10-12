/** Where the film's bundle is served from, and how its files are asked for. */

import { BUNDLE } from "./version";

export const BASE = "/landing";

/**
 * The address of a file of the bundle. It carries the bundle's version, so a browser may
 * keep the file for good (see next.config.ts): a new bundle has new addresses.
 */
export function bundleUrl(path: string): string {
  return `${BASE}/${path}?v=${BUNDLE}`;
}
