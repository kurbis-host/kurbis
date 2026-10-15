"use client";

import { useRouter } from "next/navigation";
import { useEffect, useState } from "react";
import { COPY } from "../../lib/landing/copy";
import { useFilm } from "./film-context";
import { useNames } from "./name-field";

/**
 * The cave's lever, as a real button. It works without the film (the page says what
 * state the server is in); with the film, it is the same lever as the one in the cave.
 */
export function LeverControl() {
  const { film } = useFilm();
  const [awake, setAwake] = useState(true);

  useEffect(() => {
    if (!film) return;
    film.onCave = setAwake; // whoever pulls it, in the cave or here
    return () => {
      film.onCave = () => {};
    };
  }, [film]);

  const does = awake ? COPY.cave.lever.awake : COPY.cave.lever.asleep;
  return (
    <>
      <p role="status" className="landing-text">
        {awake ? COPY.cave.now : COPY.cave.asleep.join(" ")}
      </p>
      <button
        type="button"
        className="landing-chip"
        onFocus={() => film?.goTo("cave")}
        onClick={() => {
          if (film) film.pull();
          else setAwake((now) => !now);
        }}
      >
        {does.join(" ")}
      </button>
    </>
  );
}

/**
 * The film's bar, as real links: signing in, and the way to the name field. A link the
 * film draws (its own "Sign in") is followed through the page's router.
 */
export function BarControls() {
  const { film } = useFilm();
  const router = useRouter();
  const back = useNames().status === "mine"; // (a visitor who is back: their server is made)

  useEffect(() => {
    if (!film) return;
    film.onLink = (href) => router.push(href);
    return () => {
      film.onLink = (href) => window.location.assign(href);
    };
  }, [film, router]);

  return (
    <nav aria-label="Kurbis" className="landing-bar">
      <a href="/login" className="landing-chip">
        {COPY.bar.signIn}
      </a>
      <a
        href={back ? "/app" : "#name"}
        className="landing-chip"
        onClick={(event) => {
          if (!film || back) return;
          event.preventDefault();
          film.onGet(); // as the film's own button: the claim card, or the name field in sight
        }}
      >
        {back ? COPY.bar.home : COPY.bar.get}
      </a>
    </nav>
  );
}
