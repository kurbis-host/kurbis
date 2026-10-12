/**
 * The name a visitor picks on the landing page: what may be typed, what is wrong with
 * a name, and whether it is free.
 */

import { SUBDOMAIN, isSubdomain } from "../subdomain";

/** Whether the page asks a server if a name is free, or makes the answer up. There is no endpoint yet. */
export const MOCK = true;

export const LONGEST = SUBDOMAIN.longest;

/** What is wrong with a name, if anything: nothing typed, too short, a dash where none may be. */
export type Flaw = "empty" | "short" | "dash" | null;

/** A name as it may be typed: lowercase letters, digits and dashes, and no longer than allowed. */
export function typed(raw: string): string {
  return raw.toLowerCase().replace(/[^a-z0-9-]/g, "").slice(0, LONGEST);
}

export function flawOf(name: string): Flaw {
  if (!name) return "empty";
  if (isSubdomain(name)) return null;
  return name.length < SUBDOMAIN.shortest ? "short" : "dash";
}

const TAKEN = new Set(["steve", "lena", "bigfarm", "alex", "creeper", "admin", "minecraft", "kurbis", "play", "survival", "www", "api"]);

/** Whether a (well-formed) name is free. Made up for now; the signature is what a real check would have. */
export async function isFree(name: string, signal?: AbortSignal): Promise<boolean> {
  if (!MOCK) throw new Error("landing: there is no name check to ask yet");
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(resolve, 380);
    signal?.addEventListener("abort", () => {
      clearTimeout(timer);
      reject(new DOMException("Aborted", "AbortError"));
    });
  });
  return !TAKEN.has(name);
}
