/**
 * The sign-in provider a visitor picked last, kept in their browser so the login page can
 * point them back to it. Storage can be refused (a private window, blocked site data); then
 * nothing is kept and no provider is marked.
 */

export type SignInProvider = "google" | "discord";

const KEY = "kurbis.sign-in";

export function keepSignInProvider(provider: SignInProvider) {
  try {
    localStorage.setItem(KEY, provider);
  } catch {}
}

export function lastSignInProvider(): SignInProvider | null {
  try {
    const provider = localStorage.getItem(KEY);
    return provider === "google" || provider === "discord" ? provider : null;
  } catch {
    return null;
  }
}
