// Frames of the page as the browser draws it, at given scroll positions, for laying
// beside the previs (compare_web.py). Run from web/, with the page being served:
//
//   node art/landing/scripts/web_frames.mjs http://localhost:3210 <out dir> [wide|tall] [positions...]
//
// The page is given a few seconds at each position, so what is triggered there has played.

import { chromium } from "@playwright/test";
import { mkdirSync } from "node:fs";
import path from "node:path";

const [origin = "http://localhost:3210", out = "frames", shape = "wide", ...rest] = process.argv.slice(2);
const positions = rest.length ? rest.map(Number) : [0.1, 0.5, 0.8, 1.3, 1.7, 1.9, 2.3, 2.62, 3.1, 3.4, 3.8, 4.22, 4.62, 4.95, 5.5, 6.2];
const size = shape === "tall" ? { width: 405, height: 720 } : { width: 1280, height: 720 };

mkdirSync(out, { recursive: true });
const browser = await chromium.launch({ args: ["--use-angle=metal", "--enable-gpu", "--ignore-gpu-blocklist", "--enable-unsafe-swiftshader"] });
const page = await browser.newPage({ viewport: size, deviceScaleFactor: 2 });
page.on("pageerror", (error) => console.error("page:", String(error)));
await page.goto(origin, { waitUntil: "load" });
await page.waitForSelector('main[data-film="playing"]', { timeout: 120000 });
await page.waitForTimeout(3000); // the Nether is built behind the first frame
let last = -1;
for (const s of positions) {
  // Scroll there as a visitor would (in steps), so what the scroll triggers on the way is triggered.
  for (let at = last < s ? Math.max(last, s - 0.5) : s; at < s; at += 0.05) {
    await page.evaluate((to) => window.scrollTo({ top: to * window.innerHeight, behavior: "instant" }), at);
    await page.waitForTimeout(40);
  }
  await page.evaluate((to) => window.scrollTo({ top: to * window.innerHeight, behavior: "instant" }), s);
  await page.waitForTimeout(2500);
  await page.screenshot({ path: path.join(out, `${shape}_${s.toFixed(2)}.png`) });
  console.log(shape, s.toFixed(2));
  last = s;
}
await browser.close();
