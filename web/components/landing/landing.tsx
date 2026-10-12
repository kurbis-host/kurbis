"use client";

import { useMemo, useState, type ReactNode } from "react";
import type { Film } from "../../lib/landing/film/film";
import FilmCanvas from "./film-canvas";
import { FilmContext, type FilmState } from "./film-context";

/** Screens of scroll the film takes, from the sky to the Nether. */
const END = 6.2;

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
      <main className="landing relative bg-[#1b1020]" style={failed ? undefined : { height: `${(END + 1) * 100}lvh` }} data-film={state}>
        {!failed && (
          <div aria-hidden="true" className="fixed inset-0 h-lvh w-full">
            <FilmCanvas worker={worker} onState={setState} onFilm={setFilm} />
            {state === "loading" && <div className="landing-loading" />}
          </div>
        )}
        {children}
      </main>
    </FilmContext.Provider>
  );
}
