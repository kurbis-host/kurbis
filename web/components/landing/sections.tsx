"use client";

import { ArrowDown, ArrowUp, CloudRain, DoorOpen, Moon, Sun } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { IDLE_SECONDS, ROOT_DOMAIN, type LayerKey } from "../../lib/landing/constants";
import { ADDRESS_EXAMPLES, BACKUP_ROWS, BURIED, CHAT_LINES, JOURNAL, ORES, SPAWNER_LINES } from "../../lib/landing/copy";
import { landing, useLanding } from "../../lib/landing/store";
import { KurbisLogo } from "../kurbis-logo";
import { scrollToLayer } from "./hud";
import { NameClaim } from "./name-claim";
import { useLayerF } from "./use-scroll-world";

const shell = "relative flex min-h-[100svh] px-4 md:items-center md:py-24 md:pl-[84px]";
/** On phones a card sits at the bottom and leaves the top third of the screen to the world. */
const sectionShell = `${shell} max-md:items-end max-md:pt-[36svh] max-md:pb-20`;
/** The hero is the other way round: text first, the pumpkin on its cloud underneath. */
const heroShell = `${shell} max-md:items-start max-md:pt-16`;
const card =
  "pointer-events-auto w-full max-w-[470px] rounded-[14px] border-2 border-ink bg-paper p-6 text-ink shadow-[0_6px_0_rgba(34,36,31,0.85)] max-md:p-5";
const heading = "m-0 font-display text-[clamp(26px,3.3vw,40px)] leading-[1.05] font-[850] tracking-[-0.03em] text-balance";
const body = "mt-3 mb-0 text-[16px] leading-[1.55] text-ink-soft";
const tag = "m-0 mb-2 font-mono text-[11px] font-bold tracking-[0.08em] text-pumpkin-dark uppercase";
const primary =
  "inline-flex min-h-12 cursor-pointer items-center gap-2 rounded-[9px] border-0 bg-pumpkin px-[18px] text-[15px] font-[800] text-[#25170f] no-underline shadow-[0_5px_0_var(--color-pumpkin-dark)] disabled:cursor-not-allowed disabled:opacity-50";
const secondary =
  "inline-flex min-h-12 cursor-pointer items-center gap-2 rounded-[9px] border-0 bg-[#eee7d8] px-[18px] text-[15px] font-[800] text-ink no-underline shadow-[0_5px_0_#cfc5ae] disabled:cursor-not-allowed disabled:opacity-50";
const chipShape = "inline-flex min-h-9 cursor-pointer items-center gap-1.5 rounded-[7px] border-2 border-ink px-2.5 text-[13px] font-[750]";
const chip = `${chipShape} bg-white text-ink`;
const chipActive = `${chipShape} bg-ink text-paper`;
const console_ = "rounded-[9px] bg-ink p-3.5 font-mono text-[13px] leading-[1.6] text-paper";

function jumpTo(key: LayerKey) {
  return (event: React.MouseEvent) => {
    event.preventDefault();
    scrollToLayer(key);
  };
}

function Layer({ layer, yLabel, title, children }: { layer: LayerKey; yLabel: string; title: string; children?: ReactNode }) {
  return (
    <section id={`layer-${layer}`} aria-labelledby={`${layer}-title`} className={sectionShell}>
      <div className={card}>
        <p className={tag}>{yLabel}</p>
        <h2 id={`${layer}-title`} className={heading}>
          {title}
        </h2>
        {children}
      </div>
    </section>
  );
}

function SkySection() {
  const night = useLanding((state) => state.night);
  return (
    <section
      id="layer-sky"
      aria-labelledby="sky-title"
      className={`${heroShell} transition-colors duration-500 ${night ? "text-paper" : "text-ink"}`}
    >
      <a href="/login" className="pointer-events-auto absolute top-4 right-5 text-[15px] font-[750] text-inherit">
        Sign in
      </a>
      <div className="pointer-events-auto w-full max-w-[560px]">
        <p className="m-0 flex items-center gap-2.5 font-display text-[22px] leading-none font-[830] tracking-[-0.025em]">
          <KurbisLogo size={32} /> kurbis
        </p>
        <h1 id="sky-title" className="mt-8 mb-0 font-display max-md:mt-5 text-[clamp(40px,6.4vw,76px)] leading-[0.98] font-[850] tracking-[-0.045em] text-balance">
          Your world is waiting below.
        </h1>
        <p className="mt-5 mb-0 max-w-[440px] text-[19px] leading-[1.5]">Free Minecraft servers for you and your friends. Java and Bedrock.</p>
        <p className="relative mt-6 mb-0 inline-block rounded-[10px] border-2 border-ink bg-paper px-3 py-2 text-[14px] font-[700] text-ink after:absolute after:top-full after:left-5 after:border-8 after:border-transparent after:border-t-ink after:content-['']">
          Hi. Kürbis means pumpkin.
        </p>
        <div className="mt-7 flex flex-wrap items-center gap-x-5 gap-y-3">
          <a href="#layer-surface" onClick={jumpTo("surface")} className={primary}>
            Dig down <ArrowDown size={17} aria-hidden="true" />
          </a>
          <a href="#layer-nether" onClick={jumpTo("nether")} className="text-[15px] font-[750] text-inherit">
            Impatient? Jump straight in.
          </a>
        </div>
        <div className="mt-6 flex flex-wrap gap-2 max-md:mt-4">
          <button type="button" className={chip} aria-pressed={night} onClick={landing.toggleNight}>
            {night ? <Sun size={15} aria-hidden="true" /> : <Moon size={15} aria-hidden="true" />}
            {night ? "Make it day" : "Make it night"}
          </button>
          <button type="button" className={chip} onClick={landing.rain}>
            <CloudRain size={15} aria-hidden="true" /> Poke a cloud
          </button>
        </div>
      </div>
    </section>
  );
}

/** Types the example addresses one after another. */
function TypedAddress() {
  const [text, setText] = useState(ADDRESS_EXAMPLES[0]);

  useEffect(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    let name = 0;
    let length = ADDRESS_EXAMPLES[0].length;
    let hold = 14;
    let erasing = true;
    const timer = setInterval(() => {
      if (hold > 0) {
        hold -= 1;
        return;
      }
      const word = ADDRESS_EXAMPLES[name];
      length += erasing ? -1 : 1;
      if (erasing && length === 0) {
        erasing = false;
        name = (name + 1) % ADDRESS_EXAMPLES.length;
      } else if (!erasing && length === word.length) {
        erasing = true;
        hold = 16;
      }
      setText(ADDRESS_EXAMPLES[name].slice(0, length));
    }, 95);
    return () => clearInterval(timer);
  }, []);

  return (
    <p className="m-0 font-mono text-[clamp(16px,2.2vw,20px)] font-bold break-all">
      <span className="sr-only">
        {ADDRESS_EXAMPLES.map((name) => `${name}.${ROOT_DOMAIN}`).join(", ")}
      </span>
      <span aria-hidden="true">
        {text}
        <span className="animate-[pulse-status_1s_steps(1)_infinite] text-pumpkin-dark">|</span>.{ROOT_DOMAIN}
      </span>
    </p>
  );
}

function SurfaceSection() {
  return (
    <Layer layer="surface" yLabel="Y 64 · Surface" title="The address is up here. The world is down there.">
      <div className="mt-4 rounded-[9px] border-2 border-[#6b4a2c] bg-[#e9cf9f] px-4 py-3 shadow-[0_4px_0_#6b4a2c]">
        <TypedAddress />
        <p className="mt-1.5 mb-0 flex flex-wrap items-center justify-between gap-2 text-[14px] font-[700] text-[#4a3320]">
          You choose the name.
          <a href="#layer-nether" onClick={jumpTo("nether")} className="text-inherit">
            Claim yours ↓
          </a>
        </p>
      </div>
      <ul className={`${console_} mt-4 mb-0 list-none`}>
        {CHAT_LINES.map((line, index) => (
          <li key={index}>
            <span className={line.who === "You" ? "text-[#ffb37a]" : "text-[#9fd0e3]"}>&lt;{line.who}&gt;</span> {line.text}
          </li>
        ))}
      </ul>
      <p className={body}>
        <strong className="text-ink">Different editions. Same pond.</strong> Java on one bank, Bedrock on the other.
      </p>
      <ul className="mt-3 mb-0 flex list-none flex-wrap gap-2 p-0 font-mono text-[13px] font-bold">
        <li className="rounded-[6px] bg-[#dcebc8] px-2.5 py-1 text-[#35501d]">Ads: 0</li>
        <li className="rounded-[6px] bg-[#dcebc8] px-2.5 py-1 text-[#35501d]">Credit cards asked: 0</li>
      </ul>
    </Layer>
  );
}

function DirtSection() {
  const layerF = useLayerF(20);
  const found = useLanding((state) => state.found);
  const lastFound = useLanding((state) => state.lastFound);
  const mined = Math.max(0, Math.round((layerF - 1.3) * 34));
  const line = BURIED.find((thing) => thing.id === lastFound)?.line;

  return (
    <Layer layer="dirt" yLabel="Y 50 · Dirt" title="Explorer's journal">
      <ol className="mt-4 mb-0 grid list-none gap-1 p-0 font-mono text-[14px]">
        {JOURNAL.map((entry, index) => (
          <li key={entry} className={`transition-opacity duration-500 ${layerF >= 1.55 + index * 0.17 ? "opacity-100" : "opacity-0"}`}>
            {entry}
          </li>
        ))}
      </ol>
      <p className="mt-4 mb-0 inline-block -rotate-2 rounded-[6px] border-2 border-dashed border-pumpkin-dark bg-[#fff1dc] px-3 py-1.5 text-[14px] font-[800] text-pumpkin-dark">
        Cost: nothing. Hidden fees: dirt.
      </p>
      <p className={`${body} font-mono text-[14px]`}>
        Blocks mined: <span className="font-bold text-ink tabular-nums">{mined}</span>. Nobody&apos;s counting.
      </p>
      <p className="mt-4 mb-2 text-[13px] font-[750] text-ink-soft">Things in the dirt ({found.length}/{BURIED.length}):</p>
      <div className="flex flex-wrap gap-2">
        {BURIED.map((thing) => (
          <button
            key={thing.id}
            type="button"
            className={thing.id === lastFound ? chipActive : chip}
            aria-pressed={found.includes(thing.id)}
            onClick={() => landing.dig(thing.id)}
          >
            {found.includes(thing.id) ? thing.label : "???"}
            <span className="sr-only"> (dig up {thing.label.toLowerCase()})</span>
          </button>
        ))}
      </div>
      <p role="status" className="mt-2 mb-0 min-h-[22px] text-[14px] font-[700]">
        {line}
      </p>
      <p className="mt-3 mb-0 border-t-2 border-[#e4dac4] pt-3 text-[15px] font-[750]">Free. Java and Bedrock. Same address. Keep digging.</p>
    </Layer>
  );
}

function clock(left: number): string {
  const seconds = Math.ceil(left * 300);
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

function CaveSection() {
  const cave = useLanding((state) => state.cave);
  const torches = useLanding((state) => state.torches);
  const countdown = useLanding((state) => state.countdown);
  const status =
    cave === "awake"
      ? "Awake. Someone's home."
      : cave === "countdown"
        ? `Everyone left. Lights out in ${clock(countdown)}.`
        : cave === "snapshot"
          ? "Snapshot taken. World saved."
          : "Asleep. Dark isn't empty: the world is all still here.";

  return (
    <Layer layer="cave" yLabel="Y 20 · Cave" title="It only glows when someone's home.">
      <p className={body}>A server is awake only while someone is connected. While it sleeps, the world is kept as a snapshot.</p>
      <div aria-hidden="true" className="mt-4 flex gap-2.5">
        {torches.map((lit, index) => (
          <span
            key={index}
            className={`h-8 w-3.5 rounded-[3px] transition-all duration-300 ${
              lit ? "bg-linear-to-b from-[#ffd27a] to-pumpkin shadow-[0_0_18px_5px_rgba(255,170,70,0.55)]" : "bg-[#3a3a3a]"
            }`}
          />
        ))}
      </div>
      <div className="mt-3 h-2 overflow-hidden rounded-full bg-[#e4dac4]" aria-hidden="true">
        <span
          className="block h-full rounded-full bg-pumpkin"
          style={{ width: `${cave === "countdown" ? countdown * 100 : cave === "awake" ? 100 : 0}%` }}
        />
      </div>
      <p role="status" className="mt-3 mb-0 min-h-[44px] text-[15px] font-[750]">
        {status}
      </p>
      <div className="mt-2 flex flex-wrap gap-3">
        <button type="button" className={primary} disabled={cave === "awake"} onClick={landing.playerJoins}>
          A player joins
        </button>
        <button type="button" className={secondary} disabled={cave !== "awake"} onClick={() => landing.everyoneLeaves()}>
          Everyone leaves
        </button>
      </div>
      <p className="mt-4 mb-0 text-[13px] leading-[1.5] text-ink-soft">
        Sped up: a few seconds here stand in for 5 minutes. Leave this tab alone for {IDLE_SECONDS} seconds and the cave goes dark on its own.
      </p>
    </Layer>
  );
}

function OresSection() {
  const active = useLanding((state) => state.ore);
  const ore = ORES.find((entry) => entry.id === active);

  return (
    <Layer layer="ores" yLabel="Y 0 · Ores" title="Pickaxe's in your cursor.">
      <div className="mt-4 flex flex-wrap gap-2">
        {ORES.map((entry) => (
          <button
            key={entry.id}
            type="button"
            aria-pressed={entry.id === active}
            onClick={() => landing.setOre(entry.id)}
            onPointerEnter={() => landing.setOre(entry.id)}
            onFocus={() => landing.setOre(entry.id)}
            className={entry.id === active ? chipActive : chip}
          >
            <span className={`size-3 rounded-[2px] border border-white/40 ${ORE_SWATCH[entry.id]}`} aria-hidden="true" />
            {entry.label}
          </button>
        ))}
      </div>
      <p role="status" className="mt-4 mb-0 min-h-[58px] rounded-[9px] bg-[#f1e9d6] px-3.5 py-3 text-[15px] leading-[1.45]">
        {ore ? (
          <>
            <strong>{ore.topic}.</strong> {ore.line}
          </>
        ) : (
          <span className="text-ink-soft">Hover an ore in the wall. Each one is a part of your server&apos;s panel.</span>
        )}
      </p>
    </Layer>
  );
}

const ORE_SWATCH: Record<(typeof ORES)[number]["id"], string> = {
  redstone: "bg-[#e03a2c]",
  coal: "bg-[#26262b]",
  gold: "bg-[#f0bf32]",
  lapis: "bg-[#2c53c4]",
  copper: "bg-[#d67f4c]",
};

function SpawnerSection() {
  return (
    <Layer layer="spawner" yLabel="Y -10 · Spawner" title="Ask the spawner anything.">
      <ul className={`${console_} mt-4 mb-0 grid list-none gap-2`}>
        {SPAWNER_LINES.map((line) => (
          <li key={line.cmd}>
            <span className="text-[#ffb37a]">{line.cmd}</span>
            <br />
            <span className="text-[#b9c2b0]">→ {line.reply}</span>
          </li>
        ))}
      </ul>
      <p className={body}>Operators only, with their own permissions. We cover the AI. No sign-in, no keys.</p>
    </Layer>
  );
}

function ChestSection() {
  const open = useLanding((state) => state.chestOpen);
  return (
    <Layer layer="chest" yLabel="Y -20 · Chest" title="Creeper took the house? Ctrl+Z for the whole world.">
      <p className={body}>Backups and restore.</p>
      <button type="button" className={`${secondary} mt-4`} aria-expanded={open} aria-controls="chest-rows" onClick={landing.toggleChest}>
        {open ? "Close the chest" : "Open the chest"}
      </button>
      <div id="chest-rows" hidden={!open} className="mt-4">
        <p className="m-0 mb-1.5 font-mono text-[11px] font-bold tracking-[0.08em] text-ink-soft uppercase">Example</p>
        <ul className="m-0 grid list-none gap-1.5 p-0">
          {BACKUP_ROWS.map((row) => (
            <li key={row.when} className="flex items-center justify-between gap-3 rounded-[8px] bg-[#f1e9d6] px-3 py-2 text-[14px]">
              <span className="font-mono font-bold">{row.when}</span>
              <span className="text-ink-soft">{row.note}</span>
            </li>
          ))}
        </ul>
      </div>
    </Layer>
  );
}

function DiamondsSection() {
  return (
    <Layer layer="diamonds" yLabel="Y -40 · Diamonds" title="Checked it. Twice.">
      <p className={body}>Everyone leaves and it&apos;s lights-out after 5 minutes. We timed it.</p>
      <p className="mt-3 mb-0 text-[13px] text-ink-soft">
        Press <kbd className="rounded-[5px] border border-[#cfc5ae] bg-white px-1.5 py-0.5 font-mono text-[12px] font-bold text-ink">F3</kbd> for
        the numbers.{" "}
        <button type="button" className="cursor-pointer border-0 bg-transparent p-0 font-[750] text-ink underline" onClick={landing.toggleF3}>
          Or tap here.
        </button>
      </p>
    </Layer>
  );
}

function BedrockSection() {
  const bumps = useLanding((state) => state.bumps);
  return (
    <Layer layer="bedrock" yLabel="Y -64 · Bedrock" title="Unbreakable. So is our list of limits.">
      <p className={`${body} font-mono text-[14px]`}>2 GiB · 5 GiB · one region · one server each.</p>
      <div className="mt-4 flex flex-wrap items-center gap-3">
        <button type="button" className={secondary} onClick={landing.bumpBedrock}>
          Try to break it
        </button>
        <span role="status" className="text-[14px] font-[750]">
          {bumps > 0 ? `Clonk. ×${bumps}. Still bedrock.` : ""}
        </span>
      </div>
    </Layer>
  );
}

function NetherSection({ onFall }: { onFall: () => void }) {
  const [shimmer, setShimmer] = useState(false);

  function stepThrough(event: React.MouseEvent<HTMLAnchorElement>) {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    event.preventDefault();
    const href = event.currentTarget.href;
    setShimmer(true);
    setTimeout(() => window.location.assign(href), 650);
  }

  return (
    <Layer layer="nether" yLabel="The Nether" title="Pick a name. That's the whole signup.">
      <div className="mt-4">
        <NameClaim />
      </div>
      <div className="mt-5 rounded-[10px] bg-[#2a1840] p-4 text-paper">
        <p className="m-0 text-[16px] font-[800]">Already in the game? Step through.</p>
        <a
          href="/login"
          onClick={stepThrough}
          className="mt-3 inline-flex min-h-11 items-center gap-2 rounded-[9px] bg-[#a45cff] px-4 text-[15px] font-[800] text-white no-underline shadow-[0_5px_0_#5b2ca3]"
        >
          <DoorOpen size={17} aria-hidden="true" /> Open dashboard
        </a>
        <p className="mt-3 mb-0 text-[13px] text-[#cdbfe6]">Sign in with Google / Discord.</p>
      </div>
      <div className="mt-4 flex flex-wrap items-center justify-between gap-3 text-[12px] text-ink-soft">
        <span>Not affiliated with Mojang or Microsoft.</span>
        <button type="button" className="inline-flex cursor-pointer items-center gap-1 border-0 bg-transparent p-0 font-[750] text-ink underline" onClick={onFall}>
          Fall back to the sky <ArrowUp size={13} aria-hidden="true" />
        </button>
      </div>
      {shimmer ? <div aria-hidden="true" className="fixed inset-0 z-50 animate-[portal-shimmer_0.65s_ease-in_forwards] bg-[#a45cff]" /> : null}
    </Layer>
  );
}

export function Sections({ onFall }: { onFall: () => void }) {
  return (
    <>
      <SkySection />
      <SurfaceSection />
      <DirtSection />
      <CaveSection />
      <OresSection />
      <SpawnerSection />
      <ChestSection />
      <DiamondsSection />
      <BedrockSection />
      <NetherSection onFall={onFall} />
    </>
  );
}
