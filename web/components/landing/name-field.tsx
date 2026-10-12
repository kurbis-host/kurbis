"use client";

import { useRouter } from "next/navigation";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { LONGEST, flawOf, isFree, typed } from "../../lib/landing/name";
import { useFilm } from "./film-context";

/** How long the typing has to rest before the name is looked up. */
const PAUSE = 700;

const SAID: Record<string, (name: string) => string> = {
  free: (name) => `${name}.kurbis.host is free. Press Enter to step through.`,
  taken: (name) => `${name}.kurbis.host is taken. Try another name.`,
  short: () => "Use at least 3 characters.",
  dash: () => "A name cannot start or end with a dash.",
};

/**
 * The name field at the end of the film. The field you see is in the world; this is the
 * real input laid over it, which takes the keyboard. What is typed shows as type in the
 * world, so the input itself shows nothing: no text, no caret.
 */
export function NameField() {
  const { film, state } = useFilm();
  const router = useRouter();
  const id = useId();
  const input = useRef<HTMLInputElement>(null);
  const form = useRef<HTMLFormElement>(null);
  const [name, setName] = useState("");
  // What was found out, and about which name: it only counts while that name still stands.
  const [found, setFound] = useState<{ name: string; verdict: "free" | "taken" | "short" | "dash" } | null>(null);
  const said = found && found.name === name ? SAID[found.verdict](name) : "";

  // Keep the input over the field, wherever the camera has it.
  useEffect(() => {
    const element = form.current;
    if (!film || !element) return;
    let frame = 0;
    const place = () => {
      frame = requestAnimationFrame(place);
      // Away from the field it keeps a place in the tab order, out of the pointer's way:
      // a keyboard that reaches it is taken to the field.
      const box = film.name.box() ?? { left: 0, top: 0, width: 0, height: 0 };
      element.style.pointerEvents = box.width > 0 ? "auto" : "none";
      element.style.left = `${box.left * 100}vw`;
      element.style.top = `${box.top * 100}lvh`;
      element.style.width = box.width > 0 ? `${box.width * 100}vw` : "1px";
      element.style.height = box.width > 0 ? `${box.height * 100}lvh` : "1px";
    };
    frame = requestAnimationFrame(place);
    return () => cancelAnimationFrame(frame);
  }, [film]);

  // Look the name up once the typing rests.
  useEffect(() => {
    film?.name.type(name, film.time);
    film?.name.judge(null, film.time);
    const flaw = flawOf(name);
    if (flaw === "empty") return;
    const abort = new AbortController();
    const timer = window.setTimeout(() => {
      const tell = (verdict: "free" | "taken" | "short" | "dash") => {
        film?.name.judge(verdict, film.time);
        setFound({ name, verdict });
      };
      if (flaw) tell(flaw);
      else isFree(name, abort.signal).then((free) => tell(free ? "free" : "taken"), () => {});
    }, PAUSE);
    return () => {
      window.clearTimeout(timer);
      abort.abort();
    };
  }, [film, name]);

  const free = found?.name === name && found.verdict === "free";
  const enter = useCallback(() => {
    if (!film) {
      // No film to play the leap: a free name goes straight on.
      if (free) router.push("/login");
      return;
    }
    if (!film.name.enter(film.time)) return;
    // The pumpkin leaps through first; then we follow.
    window.setTimeout(() => router.push("/login"), 950);
  }, [film, free, router]);

  useEffect(() => {
    if (!film) return;
    film.onEnter = enter;
    return () => {
      film.onEnter = () => {};
    };
  }, [film, enter]);

  // Where the film cannot play, the field is an ordinary one.
  const plain = state === "failed";
  return (
    <form
      ref={form}
      className={plain ? "landing-form" : "pointer-events-none fixed top-0 left-0 z-10 h-px w-px overflow-hidden"}
      onSubmit={(event) => {
        event.preventDefault();
        enter();
      }}
    >
      <label htmlFor={id} className={plain ? "" : "sr-only"}>
        Pick a name for your server. It becomes your address: name.kurbis.host
      </label>
      <input
        ref={input}
        id={id}
        name="name"
        type="text"
        value={name}
        maxLength={LONGEST}
        autoCapitalize="none"
        autoComplete="off"
        autoCorrect="off"
        spellCheck={false}
        enterKeyHint="go"
        aria-describedby={`${id}-said`}
        className={plain ? "landing-input" : "block h-full w-full cursor-text border-0 bg-transparent p-0 text-[16px] text-transparent caret-transparent outline-none selection:bg-transparent"}
        onChange={(event) => setName(typed(event.target.value))}
        onFocus={() => {
          film?.name.focus(true);
          film?.goTo("nether"); // a keyboard that tabs here from further up is taken to the field
        }}
        onBlur={() => film?.name.focus(false)}
      />
      <p id={`${id}-said`} role="status" aria-live="polite" className={plain ? "landing-said" : "sr-only"}>
        {said}
      </p>
      {plain && <button type="submit">Continue</button>}
    </form>
  );
}
