"use client";

import { createContext, useContext } from "react";
import type { Film } from "../../lib/landing/film/film";

export type FilmState = "loading" | "playing" | "failed";

/** The film, once it plays, for the page's own controls to drive. Null before, and where it cannot play. */
export const FilmContext = createContext<{ film: Film | null; state: FilmState }>({ film: null, state: "loading" });

export function useFilm() {
  return useContext(FilmContext);
}
