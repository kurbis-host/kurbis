"use client";

import { useEffect, useState } from "react";
import { LAYERS, LAYER_IDS } from "../../lib/landing/constants";
import { landing, scroll } from "../../lib/landing/store";

function lerp(from: number, to: number, t: number) {
  return from + (to - from) * t;
}

/** Piecewise-linear value of a per-layer field at a fractional layer position. */
export function layerValue(field: "worldY" | "y" | "map", layerF: number): number {
  const clamped = Math.min(Math.max(layerF, 0), LAYERS.length - 1);
  const index = Math.min(Math.floor(clamped), LAYERS.length - 2);
  return lerp(LAYERS[index][field], LAYERS[index + 1][field], clamped - index);
}

/**
 * Turns the native scroll position into a fractional layer index: layer `i` is
 * reached exactly when its section is centred in the viewport.
 */
export function useScrollWorld() {
  useEffect(() => {
    const sections = LAYER_IDS.map((id) => document.getElementById(id));
    const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
    let frame = 0;

    const measure = () => {
      frame = 0;
      const middle = window.innerHeight / 2;
      const centres = sections.map((section) => {
        const rect = section?.getBoundingClientRect();
        return rect ? rect.top + rect.height / 2 : 0;
      });
      let layerF = 0;
      for (let index = 0; index < centres.length - 1; index += 1) {
        if (middle < centres[index]) break;
        const span = centres[index + 1] - centres[index];
        layerF = index + (span > 0 ? Math.min((middle - centres[index]) / span, 1) : 0);
      }
      scroll.layerF = layerF;
      scroll.worldY = layerValue("worldY", layerF);
      scroll.yLevel = layerValue("y", layerF);
      scroll.reducedMotion = motion.matches;
      landing.setLayer(Math.round(layerF));
      window.dispatchEvent(new Event("landing:scroll"));
    };

    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(measure);
    };

    measure();
    window.addEventListener("scroll", schedule, { passive: true });
    window.addEventListener("resize", schedule);
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener("scroll", schedule);
      window.removeEventListener("resize", schedule);
    };
  }, []);
}

/** Subscribes a callback to scroll updates without re-rendering React. */
export function onLandingScroll(callback: () => void): () => void {
  window.addEventListener("landing:scroll", callback);
  callback();
  return () => window.removeEventListener("landing:scroll", callback);
}

/** The fractional layer position as React state, rounded so it only re-renders every `1 / steps` of a layer. */
export function useLayerF(steps = 20): number {
  const [value, setValue] = useState(0);
  useEffect(() => onLandingScroll(() => setValue(Math.round(scroll.layerF * steps) / steps)), [steps]);
  return value;
}
