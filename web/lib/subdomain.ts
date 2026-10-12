/**
 * What a server's name (its subdomain) must be. Kept apart from the form schemas
 * (./schemas.ts), which are built on it: the landing page checks names too, and should
 * not have to carry the schema library for one rule.
 */

export const SUBDOMAIN = {
  shortest: 3,
  longest: 32,
  /** Lowercase letters, digits and dashes, with no dash at either end. */
  pattern: /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/,
} as const;

/** Whether a name may be a subdomain. */
export function isSubdomain(name: string): boolean {
  return name.length >= SUBDOMAIN.shortest && name.length <= SUBDOMAIN.longest && SUBDOMAIN.pattern.test(name);
}
