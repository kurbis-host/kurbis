"use client";

import { useEffect, useRef, useState } from "react";
import { COPY } from "../../lib/landing/copy";
import { STATS, fleetStats, saidOf, sentenceOf, type FleetStats } from "../../lib/landing/stats";
import { useFilm } from "./film-context";

/**
 * What the wall of maps at the end of the film says, as sentences. While the film plays
 * they are the figures the film asked for when it got there; where it cannot play, they
 * are asked for once the list comes into view. Nothing is asked for as the page loads.
 * Until the figures are in, each line names its map and no figure.
 */
export function FleetFigures() {
  const { film, state } = useFilm();
  const [stats, setStats] = useState<FleetStats | null>(null);
  const list = useRef<HTMLUListElement>(null);

  useEffect(() => {
    if (!film) return;
    film.onStats = setStats;
    return () => {
      film.onStats = () => {};
    };
  }, [film]);

  useEffect(() => {
    const element = list.current;
    if (state !== "failed" || !element || typeof IntersectionObserver === "undefined") return;
    const question = new AbortController();
    const seen = new IntersectionObserver((entries) => {
      if (!entries.some((entry) => entry.isIntersecting)) return;
      seen.disconnect();
      fleetStats(question.signal).then(setStats, () => {});
    });
    seen.observe(element);
    return () => {
      seen.disconnect();
      question.abort();
    };
  }, [state]);

  return (
    <ul ref={list} aria-live="polite">
      {STATS.map((stat) => (
        <li key={stat}>{sentenceOf(stat, stats) ?? `${saidOf(stat).title}: ${COPY.ending.unknown}`}</li>
      ))}
    </ul>
  );
}
