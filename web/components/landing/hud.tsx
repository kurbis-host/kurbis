"use client";

import { Cloud, Flame, Gem, Ghost, Lamp, Package, Pickaxe, Shovel, Sprout, type LucideIcon } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { HOTBAR, LAYERS, type LayerKey } from "../../lib/landing/constants";
import { mockStats } from "../../lib/landing/stats";
import { landing, scroll, useLanding } from "../../lib/landing/store";
import { KurbisLogo } from "../kurbis-logo";
import { layerValue, onLandingScroll } from "./use-scroll-world";

export function scrollToLayer(key: LayerKey) {
  document.getElementById(`layer-${key}`)?.scrollIntoView({ block: "center" });
}

const LAYER_NAMES: Record<LayerKey, string> = {
  sky: "Sky",
  surface: "Surface",
  dirt: "Dirt",
  cave: "Cave",
  ores: "Ores",
  spawner: "Spawner",
  chest: "Chest",
  diamonds: "Diamonds",
  bedrock: "Bedrock",
  nether: "Nether",
};

/** Depth gauge: the world as a minimap, the pumpkin as the marker, and the current Y level. */
export function YMeter() {
  const marker = useRef<HTMLSpanElement>(null);
  const readout = useRef<HTMLSpanElement>(null);
  const layer = useLanding((state) => state.layer);

  useEffect(
    () =>
      onLandingScroll(() => {
        if (marker.current) marker.current.style.top = `${layerValue("map", scroll.layerF) * 100}%`;
        if (readout.current) readout.current.textContent = String(Math.round(scroll.yLevel));
      }),
    [],
  );

  return (
    <div className="pointer-events-none fixed top-3 left-3 z-20 flex flex-col items-start gap-2 md:top-1/2 md:-translate-y-1/2">
      <div
        aria-hidden="true"
        className="relative hidden h-[52vh] w-11 rounded-[8px] border-2 border-ink bg-ink shadow-[0_4px_0_rgba(34,36,31,0.55)] md:block"
      >
        <img
          src="/landing/world.png"
          alt=""
          className="size-full rounded-[5px] object-fill opacity-90 [image-rendering:pixelated]"
        />
        <span ref={marker} className="absolute left-1/2 -translate-x-1/2 -translate-y-1/2">
          <KurbisLogo size={26} className="drop-shadow-[0_2px_0_rgba(0,0,0,0.6)]" />
        </span>
      </div>
      <p className="m-0 rounded-[7px] bg-ink/85 px-2 py-1 font-mono text-[12px] leading-none font-bold text-paper">
        <span className="sr-only">Depth: </span>Y <span ref={readout}>{LAYERS[0].y}</span>
        <span className="ml-1.5 text-pumpkin">{LAYER_NAMES[LAYERS[layer]?.key ?? "sky"]}</span>
      </p>
    </div>
  );
}

const SLOT_ICONS: Record<string, LucideIcon> = {
  sky: Cloud,
  surface: Sprout,
  dirt: Shovel,
  cave: Lamp,
  ores: Pickaxe,
  spawner: Ghost,
  chest: Package,
  diamonds: Gem,
  nether: Flame,
};

/** Which hotbar slot a layer belongs to: bedrock shares the diamonds slot. */
function slotFor(key: LayerKey): LayerKey {
  return key === "bedrock" ? "diamonds" : key;
}

function isTyping(target: EventTarget | null): boolean {
  return target instanceof HTMLElement && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable);
}

export function Hotbar() {
  const layer = useLanding((state) => state.layer);
  const selected = slotFor(LAYERS[layer]?.key ?? "sky");

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey || isTyping(event.target)) return;
      if (event.key === "F3") {
        event.preventDefault();
        landing.toggleF3();
        return;
      }
      const slot = HOTBAR[Number(event.key) - 1];
      if (slot) scrollToLayer(slot.layer);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <nav
      aria-label="Layers"
      className="fixed bottom-3 left-1/2 z-20 flex -translate-x-1/2 gap-1 rounded-[10px] border-2 border-ink bg-ink/85 p-1 shadow-[0_4px_0_rgba(0,0,0,0.35)]"
    >
      {HOTBAR.map((slot, index) => {
        const Icon = SLOT_ICONS[slot.layer];
        const active = slot.layer === selected;
        return (
          <a
            key={slot.layer}
            href={`#layer-${slot.layer}`}
            aria-label={slot.label}
            aria-current={active ? "location" : undefined}
            title={`${slot.label} (${index + 1})`}
            onClick={(event) => {
              event.preventDefault();
              scrollToLayer(slot.layer);
            }}
            className={`relative grid size-9 place-items-center rounded-[6px] border-2 no-underline transition-colors max-[400px]:size-8 sm:size-11 ${
              active ? "border-paper bg-pumpkin text-[#25170f]" : "border-[#55584f] bg-[#3a3c36] text-paper hover:bg-[#4a4d45]"
            }`}
          >
            <Icon size={18} aria-hidden="true" />
            <span aria-hidden="true" className="absolute top-0 left-0.5 font-mono text-[9px] leading-none opacity-70 max-sm:hidden">
              {index + 1}
            </span>
          </a>
        );
      })}
    </nav>
  );
}

export function Toasts() {
  const toasts = useLanding((state) => state.toasts);
  const joke = useLanding((state) => state.joke);

  return (
    <>
      <div role="status" aria-live="polite" className="pointer-events-none fixed top-3 right-3 z-30 grid w-[min(300px,calc(100vw-24px))] gap-2 max-md:top-11">
        {toasts.map((toast) => (
          <div
            key={toast.id}
            className="flex animate-[toast-in_0.35s_ease-out] items-center gap-3 rounded-[8px] border-2 border-[#55584f] bg-ink px-3 py-2.5 text-paper shadow-[0_4px_0_rgba(0,0,0,0.35)]"
          >
            <KurbisLogo size={28} />
            <span className="text-[13px] leading-snug font-[700]">{toast.text}</span>
          </div>
        ))}
      </div>
      {joke ? (
        <p
          role="status"
          className="pointer-events-none fixed top-14 left-1/2 z-30 m-0 w-max max-w-[calc(100vw-32px)] -translate-x-1/2 animate-[fade-in_0.2s_ease-out] rounded-[8px] border-2 border-ink bg-paper px-3 py-2 text-[14px] font-[700] text-ink shadow-[0_4px_0_rgba(34,36,31,0.6)]"
        >
          {joke}
        </p>
      ) : null}
    </>
  );
}

function useFps(active: boolean): number {
  const [fps, setFps] = useState(60);
  useEffect(() => {
    if (!active) return;
    let frames = 0;
    let since = performance.now();
    let frame = requestAnimationFrame(function tick(now) {
      frames += 1;
      if (now - since >= 500) {
        setFps(Math.round((frames * 1000) / (now - since)));
        frames = 0;
        since = now;
      }
      frame = requestAnimationFrame(tick);
    });
    return () => cancelAnimationFrame(frame);
  }, [active]);
  return fps;
}

/** The debug screen, with server numbers. */
export function F3Overlay() {
  const open = useLanding((state) => state.f3);
  const layer = useLanding((state) => state.layer);
  const fps = useFps(open);
  const [tick, setTick] = useState(0);
  const [y, setY] = useState<number>(LAYERS[0].y);

  useEffect(() => {
    if (!open) return;
    const stop = onLandingScroll(() => setY(Math.round(scroll.yLevel)));
    const timer = setInterval(() => setTick((value) => value + 1), 2000);
    return () => {
      stop();
      clearInterval(timer);
    };
  }, [open]);

  if (!open) return null;
  const stats = mockStats(tick);
  const lines = [
    "kurbis (dig down)",
    `${fps} fps`,
    `XYZ: 9.500 / ${y}.000 / 0.500`,
    `Layer: ${LAYER_NAMES[LAYERS[layer]?.key ?? "sky"]}`,
    "",
    `Servers awake: ${stats.awake}`,
    `Servers asleep: ${stats.sleeping}`,
    `Players online: ${stats.players}`,
    `Snapshots kept: ${stats.snapshots}`,
  ];

  return (
    <pre className="pointer-events-none fixed top-12 left-3 z-30 m-0 grid gap-0.5 font-mono text-[12px] leading-tight text-white md:top-3 md:left-[72px]">
      {lines.map((line, index) => (
        <span key={index} className={line ? "w-max bg-black/55 px-1.5 py-0.5" : "h-2"}>
          {line}
        </span>
      ))}
    </pre>
  );
}
