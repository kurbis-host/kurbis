"use client";

import { useRouter } from "next/navigation";
import { createContext, useCallback, useContext, useEffect, useId, useMemo, useRef, useState, type ReactNode, type RefObject } from "react";
import { chosenName, keepChosenName } from "../../lib/chosen-name";
import { COPY } from "../../lib/landing/copy";
import { LONGEST, flawOf, isFree, typed } from "../../lib/landing/name";
import { fetchSkin } from "../../lib/landing/skin";
import { useFilm } from "./film-context";

/** How long the typing has to rest before the name is looked up. */
const PAUSE = 700;
/** How far a finger may move over the page while typing before that is a scroll (which closes the keyboard). */
const SWIPE = 14;

/** A hand without a pointer, which types on a keyboard that covers the screen. */
const touchOnly = () => typeof window !== "undefined" && window.matchMedia("(hover: none) and (pointer: coarse)").matches;
/** How far the film is lifted, for the keyboard. */
const setLift = (px: number) => document.documentElement.style.setProperty("--landing-lift", `${px}px`);

const SAID: Record<Status, (name: string) => string> = {
  checking: () => "",
  free: (name) => `${name}.kurbis.host is free. Press Enter to claim it.`,
  taken: (name) => `${name}.kurbis.host is taken. Try another name.`,
  mine: (name) => `${name}.kurbis.host is yours. Welcome back.`,
  short: () => "Use at least 3 characters.",
  dash: () => "A name cannot start or end with a dash.",
};

/** Where a name field is: in the sky, at the start; or in the Nether, at the end. */
export type Where = "sky" | "nether";
/** What is known of the name that stands: still being checked; free; taken; the visitor's own (taken, by them); or not a name at all. */
export type Status = "checking" | "free" | "taken" | "mine" | "short" | "dash";
type Verdict = Exclude<Status, "checking">;

interface Names {
  name: string;
  setName(name: string): void;
  /** What the page says about the name that stands, for whoever reads it. */
  said: string;
  status: Status | null;
  /** The field the card was opened from, while it is open; it shows the name as it stands. */
  card: Where | null;
  enter(from: Where): void;
  /** The field was left (it lost the keyboard, or the page scrolled on): a free name is taken as if Enter were pressed. */
  leave(from: Where): void;
  signIn(): void;
  close(): void;
  /** Each field's input, so the film's bar (and a key pressed anywhere) can reach one. */
  inputs: Record<Where, RefObject<HTMLInputElement | null>>;
  /** The field last typed in: only it says what the page found (one voice, not two). */
  active: Where;
  use(where: Where): void;
  /** The name is a Java player's too, and the film shows the visitor in that player's skin. */
  skinned: boolean;
}

const NamesContext = createContext<Names | null>(null);

export function useNames(): Names {
  const names = useContext(NamesContext);
  if (!names) throw new Error("landing: a name field needs the names around it");
  return names;
}

/**
 * The one name the page is about: both fields type into it, the film shows it in both,
 * and the claim card shows it as it stands, once it has been claimed. The name kept from
 * an earlier visit comes back; if it is taken now, it is the visitor's own: welcome back.
 */
export function NameProvider({ children }: { children: ReactNode }) {
  const { film } = useFilm();
  const router = useRouter();
  const [name, setTyped] = useState("");
  // The name picked on an earlier visit is in the fields again (after the first render, which the server made without it).
  const kept = useRef<string | null>(null);
  useEffect(() => {
    kept.current = chosenName();
    if (kept.current) setTyped((now) => now || kept.current!);
  }, []);
  // What was found out, and about which name: it only counts while that name still stands.
  const [found, setFound] = useState<{ name: string; verdict: Verdict } | null>(null);
  const [card, setCard] = useState<Where | null>(null);
  const [active, use] = useState<Where>("sky");
  const sky = useRef<HTMLInputElement>(null);
  const nether = useRef<HTMLInputElement>(null);
  const status: Status | null = !name ? null : found?.name === name ? found.verdict : "checking";
  const said = status ? SAID[status](name) : "";
  /** Whether the name is the visitor's to use: free, or their own already. */
  const ours = status === "free" || status === "mine";

  // Look the name up once the typing rests; the film shows it, and the answer, in both fields.
  useEffect(() => {
    const fields = film ? [film.sky, film.name] : [];
    for (const field of fields) {
      field.type(name, film!.time);
      field.judge(null, film!.time);
    }
    const flaw = flawOf(name);
    if (flaw === "empty") return;
    const abort = new AbortController();
    const timer = window.setTimeout(() => {
      const tell = (verdict: Verdict) => {
        for (const field of fields) field.judge(verdict, film!.time);
        setFound({ name, verdict });
      };
      if (flaw) tell(flaw);
      // (Taken, and the name they claimed before: it is theirs.)
      else isFree(name, abort.signal).then((free) => tell(free ? "free" : name === kept.current ? "mine" : "taken"), () => {});
    }, PAUSE);
    return () => {
      window.clearTimeout(timer);
      abort.abort();
    };
  }, [film, name]);

  // Once the name is the visitor's to use, the film calls their server by it, and greets them back if it is already theirs.
  useEffect(() => {
    film?.named(ours ? name : null);
    film?.home(status === "mine");
  }, [film, ours, status, name]);

  // And if it is a Java player's name too, the film shows them as that player.
  const [skinned, setSkinned] = useState(false);
  useEffect(() => {
    if (!film) return;
    setSkinned(false);
    if (!ours) {
      void film.wear(null, null);
      return;
    }
    const abort = new AbortController();
    fetchSkin(name, abort.signal).then(
      async (skin) => {
        if (abort.signal.aborted) return;
        await film.wear(skin ? name : null, skin);
        if (!abort.signal.aborted) setSkinned(skin !== null);
      },
      () => {}, // (no skin to be had just now: the friends stay themselves)
    );
    return () => abort.abort();
  }, [film, ours, name]);

  // (The field left while its name was still being checked, and the name whose card was closed.)
  const left = useRef<Where | null>(null);
  const dismissed = useRef<string | null>(null);
  const named = useRef(name);
  named.current = name;

  const open = useCallback((from: Where) => {
    setCard(from);
    // A phone's keyboard would cover the card along the bottom: done typing, it closes.
    if (touchOnly() && document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }, []);

  const enter = useCallback(
    (from: Where) => {
      if (ours) open(from);
    },
    [ours, open],
  );

  const close = useCallback(() => {
    setCard(null);
    dismissed.current = named.current; // (leaving the field again does not bring it back; Enter does)
  }, []);

  // Leaving the field takes the name as Enter does, once it is known to be the visitor's (now,
  // or when the check comes back). A card closed for this name stays closed.
  const leave = useCallback(
    (from: Where) => {
      if (!name || dismissed.current === name || card) return;
      if (ours) open(from);
      else if (status === "checking") left.current = from;
    },
    [name, card, ours, status, open],
  );
  useEffect(() => {
    const from = left.current;
    if (!from || status === "checking") return;
    left.current = null;
    if (ours && dismissed.current !== name && !card) open(from);
  }, [status, ours, name, card, open]);

  // Back with the name they claimed, which is theirs now: the card greets them, once.
  const greeted = useRef(false);
  useEffect(() => {
    if (status !== "mine" || greeted.current || name !== kept.current) return;
    greeted.current = true;
    open("sky");
  }, [status, name, open]);

  // The open card shows the name as it stands, and what is known of it; a free name in it is the one kept.
  // (Emptied, the field's card steps away until something is typed again.)
  useEffect(() => {
    if (!card || !name) {
      film?.unclaim();
      return;
    }
    if (status === "free") keepChosenName(name);
    film?.claim(name, status ?? "checking");
  }, [film, card, name, status]);

  const signIn = useCallback(() => {
    // At the Nether's field, the pumpkin leaps through the lit frame first; then we follow.
    if (film?.name.enter(film.time)) window.setTimeout(() => router.push("/login"), 950);
    else router.push("/login");
  }, [film, router]);
  const panel = useCallback(() => router.push("/app"), [router]);

  useEffect(() => {
    if (!film) return;
    film.onEnter = () => enter("nether"); // the film's own Enter key, by the Nether's field
    film.onClaimSignIn = signIn;
    film.onPanel = panel;
    film.onCopy = copy;
    film.onClaimClose = close; // (the card's own close button)
    // The bar's "Get your server": the card for a name that is theirs; their panel if they are back; else the field in sight
    // (the sky's while the hero is up), and the Nether's from anywhere else.
    film.onGet = () => {
      if (status === "mine") panel();
      else if (ours) open(film.sky.open ? "sky" : "nether");
      else (film.sky.open ? sky : nether).current?.focus({ preventScroll: true });
    };
    return () => {
      film.onEnter = () => {};
      film.onClaimSignIn = () => {};
      film.onPanel = () => {};
      film.onCopy = async () => false;
      film.onClaimClose = () => {};
      film.onGet = () => {};
    };
  }, [film, enter, signIn, panel, close, open, ours, status]);

  // While the sky's field is in sight, typing anywhere types into it: no need to find it first.
  useEffect(() => {
    if (!film) return;
    const onKey = (event: KeyboardEvent) => {
      // (A key the film took already is not typed: B held with F3, film-canvas.tsx.)
      if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey || event.key.length !== 1 || !/[a-z0-9-]/i.test(event.key)) return;
      if (!film.sky.open || (document.activeElement && document.activeElement !== document.body)) return;
      sky.current?.focus({ preventScroll: true }); // (the key then lands in it)
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [film]);

  const value = useMemo<Names>(
    () => ({
      name,
      setName: (next) => {
        left.current = null;
        setTyped(typed(next));
      },
      said,
      status,
      card,
      enter,
      leave,
      signIn,
      close,
      inputs: { sky, nether },
      active,
      use,
      skinned,
    }),
    [name, said, status, card, enter, leave, signIn, close, active, skinned],
  );
  return <NamesContext.Provider value={value}>{children}</NamesContext.Provider>;
}

/**
 * A name field. The field you see is in the world; this is the real input laid over it,
 * which takes the keyboard. What is typed shows as type in the world, so the input itself
 * shows nothing: no text, no caret. Where the film cannot play, it is an ordinary field.
 */
export function NameField({ where }: { where: Where }) {
  const { film, state } = useFilm();
  const names = useNames();
  const { name, said, card, enter, close } = names;
  const id = useId();
  const input = names.inputs[where];
  const form = useRef<HTMLFormElement>(null);
  const field = film ? (where === "sky" ? film.sky : film.name) : null;

  // A phone's keyboard, while this field has it: the film holds still (the browser scrolls
  // the page to make room, and must not fly the camera off), and lifts the field clear.
  const [typing, setTyping] = useState(false);
  // A press on a free name's splash, to read what it says, takes the keyboard from the field without being
  // done with it: the field is left (and a free name taken) once the splash's tooltip has gone.
  const reading = useRef(false);
  const leave = useRef(names.leave);
  leave.current = names.leave;

  // Keep the input over the field, wherever the camera has it.
  useEffect(() => {
    const element = form.current;
    if (!field || !element) return;
    let frame = 0;
    let lift = 0;
    let seen = false; // (the field was in sight while it had the keyboard)
    const place = () => {
      frame = requestAnimationFrame(place);
      // Away from the field it keeps a place in the tab order, out of the pointer's way:
      // a keyboard that reaches it is taken to the field.
      const shown = field.box();
      const box = shown ?? { left: 0, top: 0, width: 0, height: 0 };
      // Scrolled away from while it has the keyboard (a wheel does not take the focus off it): it lets go.
      const focused = document.activeElement === input.current;
      if (focused && shown) seen = true;
      else if (focused && seen && !shown) input.current?.blur();
      if (!focused) seen = false;
      if (reading.current && !film?.reading) {
        reading.current = false;
        leave.current(where);
      }
      // Over the keyboard: what of the page can be seen is the visual viewport, and the field's foot goes just above its bottom.
      const view = window.visualViewport;
      let want = 0;
      if (typing && view && box.width > 0) {
        const height = window.innerHeight;
        // (And what stands under it: the line that says whether the name is free; in the Nether, the host's line first.)
        const foot = (box.top + box.height) * height + (where === "sky" ? 0.11 : 0.18) * height;
        const top = box.top * height;
        want = Math.max(0, Math.min(foot - (view.offsetTop + view.height), top - view.offsetTop - 60));
      }
      lift += (want - lift) * 0.35;
      if (Math.abs(want - lift) < 0.5) lift = want;
      if (typing || lift > 0) setLift(lift);
      element.style.pointerEvents = box.width > 0 ? "auto" : "none";
      element.style.left = `${box.left * 100}vw`;
      element.style.top = `calc(${box.top * 100}lvh - ${lift}px)`;
      element.style.width = box.width > 0 ? `${box.width * 100}vw` : "1px";
      element.style.height = box.width > 0 ? `${box.height * 100}lvh` : "1px";
    };
    frame = requestAnimationFrame(place);
    return () => cancelAnimationFrame(frame);
  }, [film, field, typing, where, input]);

  useEffect(() => {
    if (!typing || !film) return;
    // A finger that drags over the page while typing means to scroll: the keyboard closes first.
    let from: [number, number] | null = null;
    const down = (event: TouchEvent) => void (from = [event.touches[0].clientX, event.touches[0].clientY]);
    const move = (event: TouchEvent) => {
      if (!from || event.target === input.current) return;
      if (Math.hypot(event.touches[0].clientX - from[0], event.touches[0].clientY - from[1]) > SWIPE) input.current?.blur();
    };
    window.addEventListener("touchstart", down, { passive: true });
    window.addEventListener("touchmove", move, { passive: true });
    return () => {
      window.removeEventListener("touchstart", down);
      window.removeEventListener("touchmove", move);
    };
  }, [typing, film, input]);

  const plain = state === "failed";
  return (
    <>
      <form
        ref={form}
        className={plain ? "landing-form" : "pointer-events-none fixed top-0 left-0 z-10 h-px w-px overflow-hidden"}
        onSubmit={(event) => {
          event.preventDefault();
          enter(where);
        }}
      >
        <label htmlFor={id} className={plain ? "" : "sr-only"}>
          Pick a name for your server: your Minecraft name works best. It becomes your address: name.kurbis.host
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
          onChange={(event) => names.setName(event.target.value)}
          onFocus={() => {
            names.use(where);
            field?.focus(true);
            reading.current = false;
            if (film && touchOnly()) {
              film.hold(true);
              setTyping(true);
            }
            // A keyboard that tabs here from elsewhere is taken to the field (the sky's, unless it is in sight already).
            if (film && !(where === "sky" && film.sky.open)) film.goTo(where);
          }}
          onBlur={() => {
            field?.focus(false);
            if (film?.reading) reading.current = true;
            else names.leave(where);
            if (!film || !typing) return;
            setTyping(false);
            setLift(0);
            // Back where the film was held: the browser's scroll for the keyboard is undone.
            const at = film.hold(false);
            window.scrollTo({ top: film.ring.page(at) * window.innerHeight, behavior: "instant" });
          }}
          onKeyDown={(event) => {
            if (event.key === "Escape" && card) close();
          }}
        />
        <p id={`${id}-said`} role="status" aria-live="polite" className={plain ? "landing-said" : "sr-only"}>
          {names.active === where ? said : ""}
        </p>
        {plain && <button type="submit">Continue</button>}
      </form>
      {card === where && name && <ClaimControls />}
    </>
  );
}

/** Put the address on the clipboard; false if the browser would not. */
async function copy(address: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(address);
    return true;
  } catch {
    return false;
  }
}

/**
 * The claim card as the page's own: the address, a button to copy it, and signing in.
 * Where the film plays, the film draws the card and these are what a keyboard reaches
 * (next after the field); where it cannot, this is the card.
 */
function ClaimControls() {
  const { film } = useFilm();
  const { name, status, signIn, close, skinned } = useNames();
  const router = useRouter();
  const address = `${name}${COPY.surface.host}`;
  const [copied, setCopied] = useState<string | null>(null);
  const ours = status === "free" || status === "mine";
  const said =
    status === "free" ? COPY.claim.said : status === "mine" ? COPY.claim.home : status === "taken" ? COPY.claim.taken : status === "short" ? COPY.claim.short : status === "dash" ? COPY.claim.dash : COPY.claim.checking;
  return (
    <section className="landing-claim" aria-label={address}>
      <p className="landing-text" aria-live="polite">
        <strong>{address}</strong> {said}
      </p>
      {skinned && status === "free" && <p className="landing-text">{COPY.claim.you}</p>}
      {ours && (
        <button
          type="button"
          className="landing-chip"
          onClick={async () => {
            if (!(await copy(address))) return;
            setCopied(address);
            if (film) film.ctx.ui?.copied(film.time);
          }}
        >
          {copied === address ? COPY.claim.done : COPY.claim.copy}
        </button>
      )}
      {status === "free" && (
        <button type="button" className="landing-chip" onClick={signIn}>
          {COPY.claim.signIn}
        </button>
      )}
      {status === "mine" && (
        <button type="button" className="landing-chip" onClick={() => router.push("/app")}>
          {COPY.claim.panel}
        </button>
      )}
      <button type="button" className="landing-chip" onClick={close}>
        {COPY.claim.close}
      </button>
      <p role="status" className="landing-text">
        {copied === address ? COPY.claim.copied : ""}
      </p>
    </section>
  );
}
