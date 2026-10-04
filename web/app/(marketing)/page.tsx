import {
  Archive,
  ArrowRight,
  BedDouble,
  Blocks,
  Network,
  Power,
} from "lucide-react";
import { redirect } from "next/navigation";
import { KurbisLogo } from "../../components/kurbis-logo";
import { VoxelDemo } from "../../components/voxel-demo";
import { getSessionOrNull } from "../../lib/api/server";

const sectionWidth = "mx-auto w-[min(1180px,calc(100%_-_32px))]";
const cta =
  "inline-flex min-h-12 items-center gap-[9px] rounded-[9px] px-[18px] font-[800] no-underline shadow-[0_5px_0_var(--color-pumpkin-dark),var(--shadow-low)]";
const ctaPrimary = `${cta} bg-pumpkin text-[#25170f]`;
const ctaDark = `${cta} bg-earth-dark text-paper shadow-[0_5px_0_#1b1814]`;

export default async function LandingPage() {
  if (await getSessionOrNull()) redirect("/app");

  return (
    <div className="overflow-hidden bg-[#f5efdf] text-ink">
      <nav
        className={`${sectionWidth} flex h-[74px] items-center justify-between gap-5 max-[560px]:h-16`}
        aria-label="Public navigation"
      >
        <a
          className="flex items-center gap-[11px] font-display text-[22px] leading-none font-[830] tracking-[-0.025em] no-underline"
          href="/"
        >
          <KurbisLogo size={32} className="size-[32px] rounded-[6px] shadow-[0_3px_0_rgba(94,35,10,0.25)]" />
          <span>kurbis</span>
        </a>
        <div className="flex items-center gap-6 text-sm font-[680] [&>a]:no-underline">
          <a className="max-[900px]:hidden" href="#how-it-works">
            How it works
          </a>
          <a className="max-[900px]:hidden" href="#limits">
            Free tier
          </a>
          <a className="max-[900px]:hidden" href="#faq">
            FAQ
          </a>
          <a
            className="inline-flex min-h-[42px] items-center gap-2 rounded-sm bg-[#302b25] px-[15px] text-[#fff7e8] shadow-[0_4px_0_#1f1b17] max-[560px]:text-[13px]"
            href="/login"
          >
            Sign in <ArrowRight size={16} />
          </a>
        </div>
      </nav>

      <main>
        <section className={`${sectionWidth} grid min-h-[670px] grid-cols-[minmax(0,0.86fr)_minmax(480px,1.14fr)] items-center gap-[clamp(28px,6vw,80px)] py-[54px] pb-[76px] max-[900px]:grid-cols-1 max-[900px]:pt-8 max-[560px]:min-h-0 max-[560px]:w-[min(100%_-_28px,1180px)] max-[560px]:gap-6 max-[560px]:py-[38px] max-[560px]:pb-[60px]`}>
          <div>
            <h1 className="m-0 max-w-[9.5ch] text-balance font-display text-[clamp(50px,7vw,92px)] leading-[0.92] font-[860] tracking-[-0.04em] max-[900px]:max-w-[10ch] max-[560px]:text-[clamp(48px,15vw,66px)]">
              Your world waits. Your server doesn’t.
            </h1>
            <p className="mt-6 mb-0 max-w-[58ch] text-[clamp(17px,2vw,20px)] leading-[1.55] text-[#56584f]">
              Kurbis keeps a Minecraft world ready without keeping a machine
              running. Connect to wake it, play on Java or Bedrock, then let it
              save and sleep after everyone leaves.
            </p>
            <div className="mt-7 flex flex-wrap items-center gap-3">
              <a className={ctaPrimary} href="/login">
                Create your world <ArrowRight size={18} />
              </a>
              <a
                className="inline-flex min-h-11 items-center px-2.5 font-[720] text-[#50564c] no-underline"
                href="#how-it-works"
              >
                See the wake cycle
              </a>
            </div>
            <div className="mt-[34px] flex flex-wrap gap-[22px] text-[13px] text-[#5b5d54] max-[560px]:gap-3">
              {["2 GiB RAM", "5 GiB storage", "5-minute idle sleep"].map(
                (item) => (
                  <span className="flex items-center gap-[7px]" key={item}>
                    <i className="size-2 rounded-[2px] bg-grass" />
                    {item}
                  </span>
                ),
              )}
            </div>
          </div>
          <VoxelDemo />
        </section>

        <section
          className="bg-[#302b25] py-[88px] text-[#f7efde]"
          id="how-it-works"
        >
          <div className={sectionWidth}>
            <h2 className="m-0 max-w-[12ch] font-display text-[clamp(36px,5vw,62px)] leading-none font-[830] tracking-[-0.035em]">
              A server lifecycle built around the world.
            </h2>
            <div className="mt-[46px] grid grid-cols-4 border-t border-white/18 max-[900px]:grid-cols-2 max-[560px]:grid-cols-1">
              {[
                [
                  BedDouble,
                  "Sleep",
                  "With nobody online, the five-minute idle timer winds down instead of burning worker capacity.",
                ],
                [
                  Network,
                  "Wake on connect",
                  "The gateway catches the player connection and starts the world before publishing a playable backend.",
                ],
                [
                  Blocks,
                  "Restore",
                  "The latest complete snapshot becomes the world on the next worker. The address stays the same.",
                ],
                [
                  Archive,
                  "Snapshot",
                  "When play ends, Kurbis closes cleanly and captures the world before releasing infrastructure.",
                ],
              ].map(([Icon, title, body], index) => (
                <article
                  className={`border-r border-white/12 py-7 pr-6 last:border-r-0 [&+article]:pl-6 max-[560px]:border-r-0 max-[560px]:border-b max-[560px]:border-white/12 max-[560px]:px-0 max-[560px]:py-6 max-[560px]:last:border-b-0 ${index === 1 ? "max-[900px]:border-r-0" : ""}`}
                  key={String(title)}
                >
                  <div className="grid size-[42px] place-items-center rounded-[10px] bg-[#4b443b] text-[#f0a35f]">
                    <Icon size={21} />
                  </div>
                  <h3 className="mt-5 mb-[7px] text-[17px]">{String(title)}</h3>
                  <p className="m-0 text-sm leading-[1.55] text-[#c8c0b0]">
                    {String(body)}
                  </p>
                </article>
              ))}
            </div>
          </div>
        </section>

        <section className={`${sectionWidth} py-24`}>
          <h2 className="m-0 max-w-[12ch] font-display text-[clamp(36px,5vw,62px)] leading-none font-[830] tracking-[-0.035em]">
            One address. Two editions. Less server babysitting.
          </h2>
          <div className="mt-[46px] grid grid-cols-[1.2fr_0.8fr] gap-[18px] max-[900px]:grid-cols-1">
            <article className="relative min-h-[350px] overflow-hidden rounded-[18px] bg-[#d8e6e9] p-[34px] shadow-[inset_0_0_0_1px_rgba(52,87,93,0.12)] max-[560px]:p-[25px]">
              <h3 className="mt-0 mb-[9px] font-display text-2xl leading-[1.1] font-[800]">
                Share the address, not the hosting ritual.
              </h3>
              <p className="m-0 max-w-[52ch] leading-[1.55] text-[#4c5a59]">
                Friends use the same named world while Kurbis handles placement,
                wake-up, snapshot restore, and clean sleep.
              </p>
              <div className="mt-[50px] grid gap-2.5">
                {["JAVA", "BEDROCK"].map((edition) => (
                  <div
                    className="flex justify-between gap-4 rounded-[9px] bg-paper px-4 py-3.5 shadow-[0_7px_20px_rgba(42,62,63,0.10)] max-[560px]:flex-col"
                    key={edition}
                  >
                    <span className="text-xs font-[800] text-[#56717b]">
                      {edition}
                    </span>
                    <code className="[overflow-wrap:anywhere] text-[#293a40]">
                      your-world.mc.example.com
                    </code>
                  </div>
                ))}
              </div>
            </article>
            <div className="grid gap-[18px]">
              <article className="rounded-[18px] bg-[#eee4cf] p-[30px] shadow-[inset_0_0_0_1px_rgba(95,75,47,0.10)]">
                <h3 className="mt-0 mb-[9px] font-display text-2xl leading-[1.1] font-[800]">
                  Safe between sessions
                </h3>
                <p className="m-0 max-w-[52ch] leading-[1.55] text-[#4c5a59]">
                  Completed snapshots are retained as the recovery history, with
                  manual backups when the runtime is online.
                </p>
              </article>
              <article className="rounded-[18px] bg-[#dfe6cf] p-[30px] shadow-[inset_0_0_0_1px_rgba(95,75,47,0.10)]">
                <h3 className="mt-0 mb-[9px] font-display text-2xl leading-[1.1] font-[800]">
                  Simple first, deeper when needed
                </h3>
                <p className="m-0 max-w-[52ch] leading-[1.55] text-[#4c5a59]">
                  Start with a sensible world preset, then manage logs, players,
                  whitelist, backups, and curated Pumpkin settings from the
                  panel.
                </p>
              </article>
            </div>
          </div>
        </section>

        <section className={`${sectionWidth} py-24`} id="limits">
          <h2 className="m-0 max-w-[12ch] font-display text-[clamp(36px,5vw,62px)] leading-none font-[830] tracking-[-0.035em]">
            The v1 service, plainly stated.
          </h2>
          <div className="mt-11 grid grid-cols-4 border-y border-[#c7bfad] max-[900px]:grid-cols-2 max-[560px]:grid-cols-1">
            {[
              ["2 GiB", "memory per world"],
              ["5 GiB", "persistent world storage"],
              ["5 min", "idle before automatic sleep"],
              ["Java + Bedrock", "through the Kurbis gateway"],
            ].map(([value, label], index) => (
              <div
                className={`border-r border-[#c7bfad] px-6 py-7 last:border-r-0 max-[560px]:border-r-0 max-[560px]:border-b max-[560px]:border-[#c7bfad] max-[560px]:last:border-b-0 ${index === 1 ? "max-[900px]:border-r-0" : ""}`}
                key={value}
              >
                <strong className="block font-display text-[28px] leading-none font-[820]">
                  {value}
                </strong>
                <span className="mt-[7px] block text-[13px] text-ink-soft">
                  {label}
                </span>
              </div>
            ))}
          </div>

          <div
            className="mt-[52px] grid grid-cols-[0.7fr_1.3fr] gap-12 max-[900px]:grid-cols-1"
            id="faq"
          >
            <h3 className="m-0 font-display text-[28px] leading-[1.1] font-[800]">
              Questions before you plant a world?
            </h3>
            <div className="border-t border-[#c7bfad]">
              {[
                [
                  "Does sleeping delete my world?",
                  "No. Sleeping releases the runtime after a completed snapshot; the saved world remains available for the next wake.",
                ],
                [
                  "Do I have to open the panel to start it?",
                  "No. A player connection can trigger the wake path through the gateway, and you can also start it manually from the panel.",
                ],
                [
                  "Can I upload arbitrary server files or plugins?",
                  "Not in v1. Kurbis keeps the surface intentionally curated around lifecycle, settings, players, logs, and backups.",
                ],
              ].map(([question, answer]) => (
                <details
                  className="border-b border-[#c7bfad] py-[18px]"
                  key={question}
                >
                  <summary className="cursor-pointer font-[760]">
                    {question}
                  </summary>
                  <p className="mt-3 mb-[3px] leading-[1.55] text-ink-soft">
                    {answer}
                  </p>
                </details>
              ))}
            </div>
          </div>
        </section>

        <section className="bg-pumpkin-dark px-4 py-[90px] text-center text-paper">
          <h2 className="mx-auto my-0 max-w-[14ch] font-display text-[clamp(36px,5vw,62px)] leading-none font-[830] tracking-[-0.035em]">
            A quiet server for a world that still feels alive.
          </h2>
          <p className="mx-auto mt-[18px] mb-7 max-w-[50ch] leading-[1.55] text-paper">
            Create the address once. Kurbis handles the work between “anyone
            want to play?” and the last person logging off.
          </p>
          <a
            className={ctaDark}
            href="/login"
          >
            Choose a sign-in method <Power size={18} />
          </a>
        </section>
      </main>

      <footer className="flex min-h-[90px] items-center justify-center bg-[#302b25] text-[13px] text-[#cfc6b7]">
        Kurbis v1 · Pumpkin-powered Minecraft hosting
      </footer>
    </div>
  );
}
