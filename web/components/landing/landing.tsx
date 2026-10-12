"use client";

import dynamic from "next/dynamic";
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { aspectNow, begin } from "../../lib/landing/engine/early";
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

  // The film's data is asked for the moment the page is up, while the film's own code is
  // still on its way: but only where the film can play at all.
  useEffect(() => {
    let gl: WebGL2RenderingContext | null = null;
    try {
      gl = document.createElement("canvas").getContext("webgl2");
    } catch {
      gl = null;
    }
    if (!gl) return;
    gl.getExtension("WEBGL_lose_context")?.loseContext();
    begin(aspectNow());
  }, []);
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
