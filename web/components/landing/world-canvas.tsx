"use client";

import dynamic from "next/dynamic";
import { useEffect, useRef, useState } from "react";
import { LAYERS } from "../../lib/landing/constants";
import { scroll, useLanding } from "../../lib/landing/store";
import { layerValue, onLandingScroll } from "./use-scroll-world";

const WorldScene = dynamic(() => import("./world/world-scene"), { ssr: false });

/** Flat colour behind each layer: shown while the 3D world loads and when it can't run. */
const BACKDROP = ["#9ad0ea", "#9ad0ea", "#5f4630", "#17171a", "#2a2a2c", "#24262a", "#262320", "#1c1f24", "#111114", "#4a1414"];

function hasWebGL(): boolean {
  try {
    const canvas = document.createElement("canvas");
    return Boolean(canvas.getContext("webgl2") ?? canvas.getContext("webgl"));
  } catch {
    return false;
  }
}

/** The reference cross-section, scrolled as a flat picture, for browsers without WebGL. */
function FlatWorld() {
  const ref = useRef<HTMLImageElement>(null);

  useEffect(
    () =>
      onLandingScroll(() => {
        if (ref.current) ref.current.style.transform = `translateY(${-layerValue("map", scroll.layerF) * 100}%)`;
      }),
    [],
  );

  return (
    <div className="absolute inset-y-0 right-0 w-[min(60vw,640px)] overflow-hidden max-md:w-full max-md:opacity-60">
      <img
        ref={ref}
        src="/landing/world.png"
        alt=""
        className="absolute top-1/2 left-0 w-full will-change-transform [image-rendering:pixelated]"
      />
    </div>
  );
}

export function WorldCanvas() {
  const layer = useLanding((state) => state.layer);
  const [mode, setMode] = useState<"pending" | "3d" | "flat">("pending");

  useEffect(() => {
    setMode(hasWebGL() ? "3d" : "flat");
  }, []);

  return (
    <div
      aria-hidden="true"
      className="landing-canvas fixed inset-0 z-0 transition-[background-color] duration-700"
      style={{ backgroundColor: BACKDROP[layer] ?? BACKDROP[0] }}
      data-world={mode}
      data-layer={LAYERS[layer]?.key}
    >
      {mode === "3d" ? <WorldScene /> : mode === "flat" ? <FlatWorld /> : null}
    </div>
  );
}
