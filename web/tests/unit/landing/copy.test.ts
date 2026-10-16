/**
 * The page says each thing once (lib/landing/copy.ts): as HTML for readers that do not
 * see the film, and as type in the film's world. The type is laid out by the art
 * pipeline, which exports what it set; this holds the two together in both frame shapes.
 * The credits the film rolls at its end are the interface's: this holds them to the HTML.
 */

import { cleanup, render, waitFor } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { createElement } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { FilmContext } from "../../../components/landing/film-context";
import { Mirror } from "../../../components/landing/mirror";
import { COPY } from "../../../lib/landing/copy";
import { roll } from "../../../lib/landing/film/credits";
import { STATS, saidOf } from "../../../lib/landing/stats";

vi.mock("next/navigation", () => ({ useRouter: () => ({ push: () => {} }) }));
afterEach(cleanup);

interface Reference {
  type: Record<string, string[]>;
  constants: { NAMES: string[] };
  points: { ending: { maps: number[][] } };
}

const loud = (words: readonly string[] | string) => (typeof words === "string" ? words : words.join(" ")).toUpperCase();

describe.each(["wide", "tall"])("the copy in the %s film", (aspect) => {
  const ref = JSON.parse(readFileSync(join(__dirname, "fixtures", `reference.${aspect}.json`), "utf8")) as Reference;
  /** Everything a block of type says, its lines in order (a block is one thing, or one thing per line). */
  const set = (block: string) => {
    if (ref.type[block]) return ref.type[block].join(" ");
    const lines = Object.keys(ref.type).filter((name) => new RegExp(`^${block}_\\d+$`).test(name)).sort((a, b) => Number(a.split("_").pop()) - Number(b.split("_").pop()));
    return lines.map((name) => ref.type[name].join(" ")).join(" ");
  };

  it("says in the world what the page says", () => {
    expect(set("Hero_Title")).toBe(loud(COPY.sky.title));
    expect(set("Hero_Caption")).toBe(loud(COPY.sky.caption));
    expect(set("Hero_Cue")).toContain(loud(COPY.sky.cue));
    expect(set("Landing_Caption")).toBe(loud(COPY.surface.caption));
    expect(set("Landing_Host")).toBe(loud(COPY.surface.host));
    expect(set("Landing_Editions")).toBe(loud(COPY.surface.editions));
    COPY.dig.facts.forEach((fact, i) => expect(set(`Band_${i}`)).toBe(loud(fact)));
    expect(set("Rule_Sign_Text")).toBe(loud(COPY.dig.rule.sign));
    expect(set("Cave_Awake")).toBe(loud(COPY.cave.awake));
    expect(set("Cave_Asleep")).toBe(loud(COPY.cave.asleep));
    COPY.homestead.heads.forEach((head, k) => expect(set(`Home_Head_${k}`)).toBe(loud(head)));
    expect(set("Nether_Headline")).toBe(loud(COPY.nether.headline));
    expect(set("Nether_Caption")).toBe(loud(COPY.nether.caption));
    expect(set("Nether_Host")).toBe(loud(COPY.surface.host));
  });

  it("hangs a map for each of the service's figures at the end", () => {
    // (The page draws what they show: film/maps.ts. The page's HTML says it as sentences.)
    expect(ref.points.ending.maps).toHaveLength(STATS.length);
  });

  it("says in the interface what the page says", () => {
    const { NAMES } = ref.constants;
    expect(NAMES).toEqual(COPY.surface.names.map(loud));
  });
});

describe("the credits", () => {
  it("roll in the film as the page's own HTML says them, in the same order", () => {
    render(createElement(Mirror));
    const footer = document.querySelector("footer")!;
    // What the roll says, top to bottom, is what the page's footer says, in the same order: no more, no less.
    const rolled = roll().flatMap((piece) => (piece.kind === "mark" ? [] : piece.kind === "links" ? piece.links.map(([label]) => label) : [piece.text]));
    expect(loud(footer.textContent ?? "")).toBe(loud(rolled.join("")));
    // Its links are the page's, in the same order; those on other sites open in a tab of their own.
    const links = roll().flatMap((piece) => (piece.kind === "links" ? piece.links : []));
    expect([...footer.querySelectorAll("a")].map((a) => [a.textContent, a.getAttribute("href")])).toEqual(links.map(([label, href]) => [label, href]));
    // Kurbis's own place among them: its organisation on GitHub, not a repository (the code itself is private).
    expect(links).toContainEqual(["Kurbis on GitHub", "https://github.com/kurbis-host"]);
    for (const a of footer.querySelectorAll("a[href^='https:']")) {
      expect(a).toHaveAttribute("target", "_blank");
      expect(a).toHaveAttribute("rel", "noopener noreferrer");
    }
  });
});

describe("the splash's tooltip", () => {
  it("says on the page what it says in the film, after the sky's field, and links to Pumpkin in a tab of its own", () => {
    render(createElement(Mirror));
    const sky = document.querySelector("section[aria-labelledby='landing-sky']")!;
    expect(sky.textContent).toContain(COPY.hood.lines.join(" "));
    const [label, href] = COPY.hood.link;
    const link = [...sky.querySelectorAll("a")].find((a) => a.textContent === label);
    expect(link).toHaveAttribute("href", href);
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAttribute("rel", "noopener noreferrer");
  });
});

describe("the wall of maps", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("is said in the page's HTML where the film cannot play: each map's title, then, once the list is in view, a sentence each", async () => {
    let answer: (response: Response) => void = () => {};
    const fetch = vi.fn(() => new Promise<Response>((resolve) => (answer = resolve)));
    vi.stubGlobal("fetch", fetch);
    let inView: () => void = () => {};
    vi.stubGlobal(
      "IntersectionObserver",
      class {
        constructor(private readonly seen: IntersectionObserverCallback) {}
        observe() {
          inView = () => this.seen([{ isIntersecting: true } as IntersectionObserverEntry], this as unknown as IntersectionObserver);
        }
        disconnect() {}
      },
    );
    render(createElement(FilmContext.Provider, { value: { film: null, state: "failed" } }, createElement(Mirror)));
    const ending = document.querySelector("section[aria-labelledby='landing-ending']")!;
    for (const stat of STATS) expect(ending.textContent).toContain(`${saidOf(stat).title}: ${COPY.ending.unknown}`);
    expect(fetch).not.toHaveBeenCalled(); // (nothing is asked for as the page loads)

    inView();
    const GIB = 1024 ** 3;
    const stats = {
      measured_at: "2026-10-16T12:00:30Z",
      servers_created: 1284,
      servers_online: 0,
      players_online: 0,
      cpu_used_millis: null,
      cpu_total_millis: 16000,
      memory_used_bytes: null,
      memory_total_bytes: 64 * GIB,
      tps: null,
      history: [],
      servers_by_day: [1284],
    };
    answer(new Response(JSON.stringify(stats), { status: 200, headers: { "content-type": "application/json" } }));
    await waitFor(() => expect(ending.textContent).toContain("1,284 servers made so far."));
    expect(ending.textContent).toContain("0.0 of 16 CPU cores in use.");
    expect(ending.textContent).toContain(COPY.ending.asleep.said);
    expect(fetch).toHaveBeenCalledWith("/api/v1/stats", expect.anything());
  });
});
