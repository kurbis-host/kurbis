"use client";

import { useCallback, useEffect, useState } from "react";
import { IDLE_SECONDS, LAYERS } from "../../lib/landing/constants";
import { getLanding, landing, useLanding } from "../../lib/landing/store";
import { F3Overlay, Hotbar, Toasts, YMeter } from "./hud";
import { Sections } from "./sections";
import { useScrollWorld } from "./use-scroll-world";
import { WorldCanvas } from "./world-canvas";

/** Wheel distance past the bottom of the page before the visitor falls back to the sky. */
const FALL_THRESHOLD = 900;

/** The cave sleeps when the page sits idle and wakes on the next input, like a server does. */
function useIdleCave() {
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout>;
    const active = () => {
      clearTimeout(timer);
      timer = setTimeout(() => landing.everyoneLeaves(true), IDLE_SECONDS * 1000);
      if (getLanding().idleSlept) landing.playerJoins();
    };
    const events = ["pointermove", "pointerdown", "keydown", "wheel", "touchstart", "scroll"] as const;
    for (const name of events) window.addEventListener(name, active, { passive: true });
    active();
    return () => {
      clearTimeout(timer);
      for (const name of events) window.removeEventListener(name, active);
    };
  }, []);
}

export function LandingPage() {
  const layer = useLanding((state) => state.layer);
  const [falling, setFalling] = useState(false);
  useScrollWorld();
  useIdleCave();

  const fall = useCallback(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      window.scrollTo({ top: 0, behavior: "instant" });
      return;
    }
    setFalling(true);
    setTimeout(() => window.scrollTo({ top: 0, behavior: "instant" }), 320);
    setTimeout(() => setFalling(false), 900);
  }, []);

  useEffect(() => {
    let past = 0;
    const onWheel = (event: WheelEvent) => {
      const atBottom = window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 2;
      past = atBottom && event.deltaY > 0 ? past + event.deltaY : 0;
      if (past > FALL_THRESHOLD) {
        past = 0;
        fall();
      }
    };
    window.addEventListener("wheel", onWheel, { passive: true });
    return () => window.removeEventListener("wheel", onWheel);
  }, [fall]);

  useEffect(() => {
    document.documentElement.classList.toggle("landing-pick", LAYERS[layer]?.key === "ores");
    return () => document.documentElement.classList.remove("landing-pick", "landing-hover");
  }, [layer]);

  return (
    <div className="relative">
      <WorldCanvas />
      <main className="pointer-events-none relative z-10">
        <Sections onFall={fall} />
      </main>
      <YMeter />
      <Hotbar />
      <Toasts />
      <F3Overlay />
      {falling ? <div aria-hidden="true" className="pointer-events-none fixed inset-0 z-50 animate-[fall-fade_0.9s_ease-in-out_forwards] bg-[#9ad0ea]" /> : null}
    </div>
  );
}
