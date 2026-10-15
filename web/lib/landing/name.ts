/**
 * The name a visitor picks on the landing page: what may be typed, what is wrong with
 * a name, and whether it is free.
 */

import type { components } from "../api/generated/schemas";
import { SUBDOMAIN, isSubdomain, typedSubdomain } from "../subdomain";

export const LONGEST = SUBDOMAIN.longest;

/** What is wrong with a name, if anything: nothing typed, too short, a dash where none may be. */
export type Flaw = "empty" | "short" | "dash" | null;

export const typed = typedSubdomain;

export function flawOf(name: string): Flaw {
  if (!name) return "empty";
  if (isSubdomain(name)) return null;
  return name.length < SUBDOMAIN.shortest ? "short" : "dash";
}

/**
 * Whether a (well-formed) name is free: the control plane is asked, with no session needed.
 * A name already claimed is not free, whoever claimed it (the visitor too).
 */
export async function isFree(name: string, signal?: AbortSignal): Promise<boolean> {
  // Plain fetch, not the panel's API client: the landing page carries as little as it can.
  const response = await fetch(`/api/v1/server-addresses/${encodeURIComponent(name)}`, { signal, headers: { accept: "application/json" } });
  if (!response.ok) throw new Error(`landing: the name check answered ${response.status}`);
  const answer = (await response.json()) as components["schemas"]["ServerAddress"];
  return answer.available;
}
