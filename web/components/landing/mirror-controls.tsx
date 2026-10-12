"use client";

import { useEffect, useState } from "react";
import { COPY } from "../../lib/landing/copy";
import { useFilm } from "./film-context";

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

/** The shortcut to the end, as a real link. */
export function ShortcutControl() {
  const { film } = useFilm();
  return (
    <a
      href="#name"
      className="landing-chip"
      onClick={(event) => {
        if (!film) return;
        event.preventDefault();
        document.getElementById("name")?.querySelector("input")?.focus({ preventScroll: true }); // (which takes the film there)
      }}
    >
      {COPY.nether.shortcut}
    </a>
  );
}
