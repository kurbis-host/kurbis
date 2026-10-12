"use client";

import dynamic from "next/dynamic";
import { useMemo, useState, type ReactNode } from "react";
import type { Film } from "../../lib/landing/film/film";
import { FilmContext, type FilmState } from "./film-context";

const FilmCanvas = dynamic(() => import("./film-canvas"), { ssr: false });

/** Screens of scroll the film takes, from the sky to the Nether. */
const END = 6.2;

/**
 * The landing page: a film the scroll plays, and under it the same page as HTML (the
 * children). While the film plays, the HTML is there for readers that do not see; if the
 * film cannot play, the HTML is the page.
 */
export function Landing({ children }: { children: ReactNode }) {
  const [state, setState] = useState<FilmState>("loading");
  const [film, setFilm] = useState<Film | null>(null);
  const value = useMemo(() => ({ film, state }), [film, state]);
  const failed = state === "failed";
  return (
    <FilmContext.Provider value={value}>
      <main className="landing relative bg-[#1b1020]" style={failed ? undefined : { height: `${(END + 1) * 100}lvh` }} data-film={state}>
        {!failed && (
          <div aria-hidden="true" className="fixed inset-0 h-lvh w-full">
            <FilmCanvas onState={setState} onFilm={setFilm} />
            {state === "loading" && <div className="landing-loading" />}
          </div>
        )}
        {children}
      </main>
    </FilmContext.Provider>
  );
}
