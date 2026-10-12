"use client";

import { useEffect, useRef, useState } from "react";
import type { Aspect } from "../../lib/landing/engine/bundle";
import type { Film } from "../../lib/landing/film/film";

function aspectOf(): Aspect {
  return window.innerWidth < window.innerHeight ? "tall" : "wide";
}

/** A pickaxe for the pointer, from the film's own picture of one: a data URL, or null without it. */
async function pickaxe(): Promise<string | null> {
  try {
    const response = await fetch("/landing/tex/item_iron_pickaxe.png");
    if (!response.ok) return null;
    const picture = await createImageBitmap(await response.blob());
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
export default function FilmCanvas({ onState, onFilm }: { onState?: (state: "loading" | "playing" | "failed") => void; onFilm?: (film: Film | null) => void }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const overlay = useRef<HTMLCanvasElement>(null);
  const [aspect, setAspect] = useState<Aspect | null>(null);

  useEffect(() => {
    const pick = () => setAspect(aspectOf());
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
    onState?.("loading");
    (async () => {
      const [{ loadStage }, { Film }] = await Promise.all([import("../../lib/landing/engine/stage"), import("../../lib/landing/film/film")]);
      const stage = await loadStage(aspect);
      if (cancelled) {
        stage.dispose();
        return;
      }
      const film = new Film(element, stage, overlay.current, context);
      // (A window that is not laid out yet, a hidden one say, has no height: it is at the top.)
      const screens = () => (window.innerHeight > 0 ? window.scrollY / window.innerHeight : 0);
      const onScroll = () => film.setScroll(screens());
      const onResize = () => film.resize();
      const onVisible = () => (document.hidden ? film.stop() : film.start());
      film.setScroll(screens(), true);
      // A visitor who asked for less motion gets a film that stands at the stops.
      const still = window.matchMedia("(prefers-reduced-motion: reduce)");
      const onStill = () => void (film.calm = still.matches);
      onStill();
      still.addEventListener("change", onStill);
      film.onJump = (to) => {
        window.scrollTo({ top: to * window.innerHeight, behavior: "instant" });
        film.setScroll(to, true);
      };
      // The pointer: a hand over what can be touched, a pickaxe over what can be dug up.
      const pick = await pickaxe();
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
      window.addEventListener("pointermove", at);
      window.addEventListener("pointerdown", onDown);
      window.addEventListener("pointerup", onUp);
      document.documentElement.addEventListener("pointerleave", onLeave);
      window.addEventListener("scroll", onScroll, { passive: true });
      window.addEventListener("resize", onResize);
      document.addEventListener("visibilitychange", onVisible);
      film.start();
      onState?.("playing");
      onFilm?.(film);
      if (process.env.NODE_ENV !== "production") (window as unknown as { kurbisFilm?: unknown }).kurbisFilm = film;
      cleanup = () => {
        onFilm?.(null);
        still.removeEventListener("change", onStill);
        window.removeEventListener("pointermove", at);
        window.removeEventListener("pointerdown", onDown);
        window.removeEventListener("pointerup", onUp);
        document.documentElement.removeEventListener("pointerleave", onLeave);
        document.documentElement.style.cursor = "";
        window.removeEventListener("scroll", onScroll);
        window.removeEventListener("resize", onResize);
        document.removeEventListener("visibilitychange", onVisible);
        film.dispose();
      };
    })().catch((error: unknown) => {
      console.error(error);
      if (!cancelled) onState?.("failed");
    });
    return () => {
      cancelled = true;
      cleanup();
    };
  }, [aspect, onState, onFilm]);

  return (
    <>
      <canvas ref={canvas} className="block h-full w-full" />
      {/* The interface (chat, toasts, tooltips), drawn pixel for pixel over the film. */}
      <canvas ref={overlay} className="pointer-events-none absolute inset-0 h-full w-full" aria-hidden="true" />
    </>
  );
}
