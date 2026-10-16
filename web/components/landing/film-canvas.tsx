"use client";

import { useEffect, useRef, useState } from "react";
import type { Aspect } from "../../lib/landing/engine/bundle";
import { aspectNow } from "../../lib/landing/engine/early";
import { loadStage } from "../../lib/landing/engine/stage";
import { workAt } from "../../lib/landing/engine/world";
import { Film } from "../../lib/landing/film/film";
import { FILM, Ring } from "../../lib/landing/film/ring";

/** A pickaxe for the pointer, from the film's own picture of one: a data URL, or null without it. */
async function pickaxe(source: Blob | null): Promise<string | null> {
  try {
    if (!source) return null;
    const picture = await createImageBitmap(source);
    const canvas = document.createElement("canvas");
    canvas.width = 32;
    canvas.height = 32;
    const context = canvas.getContext("2d");
    if (!context) return null;
    context.imageSmoothingEnabled = false;
    context.drawImage(picture, 0, 0, 32, 32);
    return canvas.toDataURL("image/png");
  } catch {
    return null;
  }
}

/** The canvas the film plays in. It fills its box; the page's scroll drives it. */
export default function FilmCanvas({ worker, onState, onFilm }: { worker: string; onState?: (state: "loading" | "playing" | "failed") => void; onFilm?: (film: Film | null) => void }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const overlay = useRef<HTMLCanvasElement>(null);
  const [aspect, setAspect] = useState<Aspect | null>(null);

  useEffect(() => {
    const pick = () => setAspect(aspectNow());
    pick();
    window.addEventListener("resize", pick);
    return () => window.removeEventListener("resize", pick);
  }, []);

  useEffect(() => {
    const element = canvas.current;
    if (!element || !aspect) return;
    let cancelled = false;
    let cleanup = () => {};
    // Without WebGL2 there is no film, and nothing of it is fetched: the page's own HTML is the page.
    let gl: WebGL2RenderingContext | null = null;
    try {
      gl = element.getContext("webgl2", { antialias: false, alpha: false, powerPreference: "high-performance" });
    } catch {
      gl = null;
    }
    if (!gl) {
      onState?.("failed");
      return;
    }
    const context = gl;
    // A visit begins at the sky. The page's top is the end of the credits, over the stretch where
    // the page comes round onto the sky (../../lib/landing/film/ring.ts): so a page at its top, as it
    // is opened (or as the router leaves it), is taken down to the sky. One the browser has put back
    // where it was is left there.
    const opening = new Ring(FILM);
    if (window.scrollY < opening.top * window.innerHeight) window.scrollTo({ top: opening.page(0) * window.innerHeight, behavior: "instant" });
    workAt(worker);
    onState?.("loading");
    (async () => {
      performance.mark("landing:renderer");
      const stage = await loadStage(aspect);
      performance.mark("landing:staged");
      if (cancelled) {
        stage.dispose();
        return;
      }
      const film = new Film(element, stage, overlay.current, context);
      const { ring } = film;
      // (A window that is not laid out yet, a hidden one say, has no height: it is at the sky.)
      const screens = () => (window.innerHeight > 0 ? window.scrollY / window.innerHeight : ring.page(0));
      // Where the scroll is on the ring. (The sky's place on the page is as near as a pixel can be: a pixel from it is the sky.)
      const where = () => {
        const s = ring.at(screens());
        return Math.abs(s) * window.innerHeight < 1 ? 0 : s;
      };
      // A page in the copy of the tail over the ring, or of the lead-in under it, is moved to the
      // same place in the ring proper, which looks the same: once its scroll has come to rest
      // (moving it would stop a phone's momentum), or at once if it has reached the page's top or
      // foot, and has nowhere left to go.
      const home = () => {
        const page = screens();
        const to = ring.home(page);
        if (to !== page) window.scrollTo({ top: to * window.innerHeight, behavior: "instant" });
      };
      const onScroll = () => {
        if (window.scrollY <= 0 || window.scrollY >= document.documentElement.scrollHeight - window.innerHeight - 1) home();
        film.setScroll(where());
      };
      const onRest = () => home();
      const onResize = () => film.resize();
      const onVisible = () => (document.hidden ? film.stop() : film.start());
      home();
      film.setScroll(where(), true);
      film.onJump = (to) => {
        window.scrollTo({ top: ring.page(to) * window.innerHeight, behavior: "instant" });
        film.setScroll(to, true);
      };
      // The pointer: a hand over what can be touched, a pickaxe over what can be dug up.
      const pick = await pickaxe(stage.pack.has("tex/item_iron_pickaxe.png") ? stage.pack.picture("tex/item_iron_pickaxe.png") : null);
      if (cancelled) {
        // (Torn down while the pointer was made: this film never starts, or two would draw on one interface.)
        film.dispose();
        return;
      }
      film.onCursor = (cursor) => {
        document.documentElement.style.cursor = cursor === "pick" && pick ? `url(${pick}) 25 6, pointer` : cursor === "" ? "" : "pointer";
      };
      let down: [number, number, number] | null = null;
      const at = (event: PointerEvent) => {
        const box = element.getBoundingClientRect();
        film.pointer((event.clientX - box.left) / box.width, (event.clientY - box.top) / box.height, event.pointerType === "touch");
      };
      const onDown = (event: PointerEvent) => {
        at(event);
        down = [event.clientX, event.clientY, window.scrollY];
      };
      const onUp = (event: PointerEvent) => {
        // A press that neither dragged nor scrolled is a click.
        if (down && Math.hypot(event.clientX - down[0], event.clientY - down[1]) < 10 && Math.abs(window.scrollY - down[2]) < 4) {
          at(event);
          film.press();
        }
        down = null;
      };
      const onLeave = () => film.pointer(null);
      // Shift on its own is the game's key for getting out of a cart: the film's ride answers it.
      const onKey = (event: KeyboardEvent) => {
        if (event.key === "Shift" && !event.repeat && !event.ctrlKey && !event.metaKey && !event.altKey) film.sneak();
        // The keys for the ends of the page. Home goes to the sky, where a visit begins, rather than
        // on up past it to where the page comes round (../../lib/landing/film/ring.ts). End goes to
        // the end of the credits, where "The End" is, rather than on past it; pressed there, it goes
        // on, round to the sky. (In a field they are the field's, unless the field's caret is at that
        // end of it already: then they take the page.)
        const field = event.target instanceof HTMLInputElement ? event.target : null;
        const other = !field && event.target instanceof HTMLElement && (event.target.isContentEditable || /^(TEXTAREA|SELECT)$/.test(event.target.tagName));
        if (other || event.defaultPrevented) return;
        if ((event.key === "Home" || (event.key === "ArrowUp" && event.metaKey)) && !(field && field.selectionStart !== 0)) {
          event.preventDefault();
          window.scrollTo({ top: ring.page(0) * window.innerHeight, behavior: "smooth" });
        } else if ((event.key === "End" || (event.key === "ArrowDown" && event.metaKey)) && !(field && field.selectionEnd !== field.value.length) && film.target < film.rolled - 0.01) {
          event.preventDefault();
          window.scrollTo({ top: Math.ceil(ring.page(film.rolled) * window.innerHeight), behavior: "smooth" }); // (not a pixel short of it)
        }
      };
      // The game's own keys, which players press without thinking: F1 puts the interface away,
      // F2 takes a screenshot, F3 shows the debug screen (on letting go, as the game does, unless
      // B was pressed with it, for the hitboxes). What the browser would do with them (help,
      // find) is not wanted here; every other key is left alone, and B without F3 is a "b" in a
      // field. (A Mac's F-keys need fn, as in the game; a keyboard whose media keys send them is heard too.)
      let f3: { combined: boolean } | null = null;
      const onGameKey = (event: KeyboardEvent) => {
        if (event.ctrlKey || event.metaKey || event.altKey) return;
        const key = [event.key, event.code].find((name) => /^F[1-3]$/.test(name)) ?? null;
        if (event.type === "keyup") {
          if (key !== "F3" || !f3) return;
          event.preventDefault();
          if (!f3.combined) film.debug.toggleScreen();
          f3 = null;
        } else if (key === "F3") {
          event.preventDefault();
          if (!event.repeat) f3 = { combined: false };
        } else if (f3 && (event.code === "KeyB" || event.key.toLowerCase() === "b")) {
          event.preventDefault();
          if (event.repeat) return;
          f3.combined = true;
          film.debug.toggleHitboxes(film.time);
        } else if (key === "F1" || key === "F2") {
          event.preventDefault();
          if (event.repeat) return;
          if (key === "F1") film.toggleInterface();
          else film.screenshot();
        }
      };
      const onBlur = () => void (f3 = null); // (F3 let go of where the page cannot hear it)
      // The game's chat (lib/landing/film/chat.ts): "/" opens it with the slash typed, as the game's
      // "/" does, wherever the keyboard is but in a real text field, which keeps its slash. (The name
      // field takes every letter typed on the page, so the game's T cannot open it; it never takes a
      // "/".) While it is open, the keyboard is the chat's, heard before anything else on the page
      // (the name field's typing-anywhere skips a key taken already): what is typed goes into its
      // field, and Space, the arrows and the page keys do not scroll the page. The game's own F-keys
      // are still the game's, and the browser's shortcuts the browser's. A hand without a keyboard
      // never presses "/", and never sees it.
      const texty = (element: Element | null) =>
        (element instanceof HTMLInputElement && !/^(button|checkbox|color|file|hidden|image|radio|range|reset|submit)$/.test(element.type)) ||
        element instanceof HTMLTextAreaElement ||
        element instanceof HTMLSelectElement ||
        (element instanceof HTMLElement && element.isContentEditable);
      const onChatKey = (event: KeyboardEvent) => {
        if (event.isComposing) return;
        const altGraph = event.getModifierState("AltGraph");
        if (!film.chat.keyboard) {
          if (event.key !== "/" || ((event.ctrlKey || event.altKey) && !altGraph) || event.metaKey || event.defaultPrevented || texty(document.activeElement)) return;
          event.preventDefault();
          if (document.activeElement instanceof HTMLElement) document.activeElement.blur(); // (a button the keyboard was on lets go)
          film.chat.start("/", film.time);
          return;
        }
        if (/^F\d{1,2}$/.test(event.key) || (f3 && (event.code === "KeyB" || event.key.toLowerCase() === "b"))) return;
        if (!film.chat.key(event.key, { shift: event.shiftKey, alt: event.altKey, ctrl: event.ctrlKey, meta: event.metaKey, altGraph }, film.time)) return;
        event.preventDefault();
        event.stopPropagation();
      };
      const onPaste = (event: ClipboardEvent) => {
        if (!film.chat.open) return;
        event.preventDefault();
        film.chat.paste(event.clipboardData?.getData("text") ?? "", film.time);
      };
      // A real text field taken while the chat is open (clicked into) has the keyboard instead: the chat closes.
      const onFocus = (event: FocusEvent) => {
        if (film.chat.open && texty(event.target as Element | null)) film.chat.close();
      };
      window.addEventListener("pointermove", at);
      window.addEventListener("pointerdown", onDown);
      window.addEventListener("pointerup", onUp);
      document.documentElement.addEventListener("pointerleave", onLeave);
      window.addEventListener("keydown", onKey);
      window.addEventListener("keydown", onGameKey);
      window.addEventListener("keyup", onGameKey);
      window.addEventListener("keydown", onChatKey, { capture: true });
      window.addEventListener("paste", onPaste);
      window.addEventListener("focusin", onFocus);
      window.addEventListener("blur", onBlur);
      window.addEventListener("scroll", onScroll, { passive: true });
      window.addEventListener("scrollend", onRest);
      window.addEventListener("resize", onResize);
      document.addEventListener("visibilitychange", onVisible);
      film.start();
      performance.mark("landing:playing");
      onState?.("playing");
      onFilm?.(film);
      if (process.env.NODE_ENV !== "production") (window as unknown as { kurbisFilm?: unknown }).kurbisFilm = film;
      cleanup = () => {
        onFilm?.(null);
        window.removeEventListener("pointermove", at);
        window.removeEventListener("pointerdown", onDown);
        window.removeEventListener("pointerup", onUp);
        document.documentElement.removeEventListener("pointerleave", onLeave);
        window.removeEventListener("keydown", onKey);
        window.removeEventListener("keydown", onGameKey);
        window.removeEventListener("keyup", onGameKey);
        window.removeEventListener("keydown", onChatKey, { capture: true });
        window.removeEventListener("paste", onPaste);
        window.removeEventListener("focusin", onFocus);
        window.removeEventListener("blur", onBlur);
        document.documentElement.style.cursor = "";
        window.removeEventListener("scroll", onScroll);
        window.removeEventListener("scrollend", onRest);
        window.removeEventListener("resize", onResize);
        document.removeEventListener("visibilitychange", onVisible);
        film.dispose();
      };
    })().catch((error: unknown) => {
      console.error(error);
      if (cancelled) return;
      onState?.("failed");
      window.scrollTo({ top: 0, behavior: "instant" }); // (the page's own HTML, from its top: the film's had been taken down to the sky)
    });
    return () => {
      cancelled = true;
      cleanup();
    };
  }, [aspect, worker, onState, onFilm]);

  return (
    <>
      <canvas ref={canvas} className="block h-full w-full" />
      {/* The interface (chat, toasts, tooltips), drawn pixel for pixel over the film. */}
      <canvas ref={overlay} className="pointer-events-none absolute inset-0 h-full w-full" aria-hidden="true" />
    </>
  );
}
