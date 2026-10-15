import type { ReactNode } from "react";
import { COPY } from "../lib/landing/copy";

/** A page of legal text, in the landing page's game interface: a bar home, one panel. */
export function LegalPage({ title, lead, updated, children }: { title: string; lead: string; updated: string; children: ReactNode }) {
  return (
    <div className="game-page">
      <header className="game-bar">
        <a className="game-wordmark" href="/">
          <img src="/kurbis.svg" alt="" width={32} height={32} />
          kurbis
        </a>
        <a className="game-button" href="/">
          Back to the world
        </a>
      </header>
      <main className="game-panel">
        <h1>{title}</h1>
        <p className="game-quiet">{lead}</p>
        {children}
        <p className="game-quiet" style={{ marginTop: 36 }}>
          Last updated {updated}. {COPY.footer.disclaimer}
        </p>
      </main>
    </div>
  );
}

/** Something the operator has yet to fill in before the page may go live. */
export function Fill({ children }: { children: string }) {
  return <mark className="game-fill">[[{children}]]</mark>;
}
