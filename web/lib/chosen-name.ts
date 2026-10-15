/**
 * The name a visitor picked on the landing page, kept in their browser for good: the
 * claim page offers it after sign-in, and the landing page has it in its fields again on
 * their next visit. Storage can be refused (a private window, blocked site data); then the
 * name is simply not kept.
 */

import { isSubdomain } from "./subdomain";

const KEY = "kurbis.name";

export function keepChosenName(name: string) {
  try {
    localStorage.setItem(KEY, name);
  } catch {}
}

export function chosenName(): string | null {
  try {
    const name = localStorage.getItem(KEY);
    return name && isSubdomain(name) ? name : null;
  } catch {
    return null;
  }
}
