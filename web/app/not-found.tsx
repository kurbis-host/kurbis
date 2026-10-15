import type { Metadata } from "next";

export const metadata: Metadata = {
  title: "Connection Lost",
};

/**
 * Any address the site has no page for: the game's screen for a connection that failed, on
 * its old dirt background, and the one way back. (Next answers it with a 404, and marks it
 * noindex.) A plain link home, as the legal pages have: the landing page starts its film
 * from its own first bytes, so it is loaded whole.
 */
export default function NotFound() {
  return (
    <main className="game-lost">
      <h1>Connection Lost</h1>
      <p>There is no page at this address.</p>
      <p className="game-lost-code">Error code: 404</p>
      <a className="game-button" href="/">
        Back to Title Screen
      </a>
    </main>
  );
}
