"use client";

import { useMemo, useState, type ReactNode } from "react";
import type { Film } from "../../lib/landing/film/film";
import { FILM, Ring } from "../../lib/landing/film/ring";
import FilmCanvas from "./film-canvas";
import { FilmContext, type FilmState } from "./film-context";

/**
 * Screens of scroll the page takes: the film, from the sky to home (the art's END), the
 * credits after it, and where the page comes round, with a copy over it and under it
 * (../../lib/landing/film/ring.ts). And where on it the sky is.
 */
const RING = new Ring(FILM);
const SKY = RING.page(0);

/**
 * The landing page: a film the scroll plays, and under it the same page as HTML (the
 * children). While the film plays, the HTML is there for readers that do not see; if the
 * film cannot play, the HTML is the page.
 */
export function Landing({ worker, children }: { /** Where the script of the worker that builds the world is. */ worker: string; children: ReactNode }) {
  const [state, setState] = useState<FilmState>("loading");
  const [film, setFilm] = useState<Film | null>(null);
  const value = useMemo(() => ({ film, state }), [film, state]);
  const failed = state === "failed";
  return (
    <FilmContext.Provider value={value}>
      {/* (The HTML begins at the sky's place on the page: whatever takes the page to a part of it, a link or a reader's cursor, takes it to the sky.) */}
      <main className="landing relative bg-[#1b1020]" style={failed ? undefined : { height: `${(RING.length + 1) * 100}lvh`, paddingTop: `${SKY * 100}lvh` }} data-film={state}>
        {!failed && (
          // (Lifted while a phone's keyboard covers the bottom of the screen: see name-field.tsx.)
          <div aria-hidden="true" className="fixed inset-0 h-lvh w-full" style={{ transform: "translateY(calc(-1 * var(--landing-lift, 0px)))" }}>
            <FilmCanvas worker={worker} onState={setState} onFilm={setFilm} />
            {state === "loading" && <div className="landing-loading" />}
          </div>
        )}
        {children}
      </main>
    </FilmContext.Provider>
  );
}
