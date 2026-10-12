// How long the page takes until the film's first frame, and what it fetches to get
// there, on connections of different speeds. Run from web/, against a production build
// being served (`pnpm build && pnpm start`); give several to lay them side by side:
//
//   node art/landing/scripts/measure_loading.mjs [name=]http://127.0.0.1:3000 [[name=]origin...]
//
// Each page is loaded RUNS times (5) on each connection, and then once more with what the
// browser kept. Times are the medians, in milliseconds from the start of the navigation,
// by the page's own clock: a script that waits for a selector hears of it up to half a
// second late. A page from this branch on also says how its loading went (`marks`).
//
// ONLY names the connections to try ("no limit|9 Mbit/s, 60 ms"). For a slow device, run
// it all at background priority, which on a Mac means the efficiency cores at low speed:
//
//   taskpolicy -b env ONLY="no limit" node art/landing/scripts/measure_loading.mjs <origin>...

import { chromium } from "@playwright/test";
import http from "node:http";

const builds = process.argv.slice(2).map((arg, i) => (arg.includes("=") && !arg.startsWith("http") ? arg.split(/=(.*)/s).slice(0, 2) : [`page ${i + 1}`, arg]));
if (!builds.length) builds.push(["page", "http://127.0.0.1:3000"]);
const RUNS = Number(process.env.RUNS ?? 5);
// Bytes a second, and the delay before each answer.
const every = { "no limit": [Infinity, 0], "9 Mbit/s, 60 ms": [1125000, 60], "1.6 Mbit/s, 150 ms": [200000, 150] };
const lines = Object.fromEntries(Object.entries(every).filter(([name]) => !process.env.ONLY || process.env.ONLY.split("|").includes(name)));

/** The page behind a line of so many bytes a second, shared by everything being sent on it. */
function proxy(target, bytesPerSecond, delayMs) {
  const active = [];
  let pumping = false;
  const slice = 4096;
  // Whatever is being sent takes its turn, a slice at a time, as streams on one connection do.
  const pump = () => {
    if (!active.length) return void (pumping = false);
    pumping = true;
    const job = active.shift();
    const chunk = job.body.subarray(job.sent, job.sent + slice);
    job.response.write(chunk);
    job.sent += chunk.length;
    if (job.sent >= job.body.length) job.response.end();
    else active.push(job);
    setTimeout(pump, (chunk.length / bytesPerSecond) * 1000);
  };
  const server = http.createServer((request, response) => {
    const upstream = http.request(target + request.url, { method: request.method, headers: { ...request.headers, host: new URL(target).host } }, (reply) => {
      const chunks = [];
      reply.on("data", (chunk) => chunks.push(chunk));
      reply.on("end", () => {
        const body = Buffer.concat(chunks);
        setTimeout(() => {
          response.writeHead(reply.statusCode, reply.headers);
          if (!Number.isFinite(bytesPerSecond) || !body.length) return response.end(body);
          active.push({ response, body, sent: 0 });
          if (!pumping) pump();
        }, delayMs);
      });
    });
    request.pipe(upstream);
    upstream.on("error", () => response.destroy());
  });
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server)));
}

// In the page: when the film began to play.
const watch = () => {
  window.__playing = null;
  const look = () => {
    if (window.__playing === null && document.querySelector('main[data-film="playing"]')) window.__playing = performance.now();
  };
  new MutationObserver(look).observe(document, { subtree: true, attributes: true, childList: true });
};
const read = () => ({
  playing: Math.round(window.__playing),
  // (The page's first picture is the film's first frame: until then it shows its background.)
  frame: Math.round(performance.getEntriesByType("paint").find((paint) => paint.name === "first-contentful-paint")?.startTime ?? -1),
  marks: performance
    .getEntriesByType("mark")
    .filter((mark) => mark.name.startsWith("landing:"))
    .map((mark) => [mark.name.slice(8), Math.round(mark.startTime), mark.detail ? Object.fromEntries(Object.entries(mark.detail).map(([key, value]) => [key, Math.round(value)])) : undefined]),
});
const median = (values) => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];

const browser = await chromium.launch({ args: ["--enable-gpu", "--ignore-gpu-blocklist", ...(process.platform === "darwin" ? ["--use-angle=metal"] : [])] });
for (const [line, [rate, delay]] of Object.entries(lines)) {
  for (const [name, target] of builds) {
    const server = await proxy(target.replace(/\/$/, ""), rate, delay);
    const origin = `http://127.0.0.1:${server.address().port}`;
    const runs = [];
    for (let run = 0; run < RUNS; run += 1) {
      const context = await browser.newContext({ viewport: { width: 1280, height: 720 } });
      const page = await context.newPage();
      await page.addInitScript(watch);
      const fetched = [];
      page.on("requestfinished", async (request) => {
        const sizes = await request.sizes().catch(() => null);
        // (What the browser kept and did not ask for again counts as nothing.)
        if (sizes) fetched.push([new URL(request.url()).pathname, Math.max(sizes.responseBodySize + sizes.responseHeadersSize, 0)]);
      });
      const sizes = () => {
        const film = fetched.filter(([path]) => path.startsWith("/landing/"));
        const sum = (list) => list.reduce((total, [, size]) => total + size, 0);
        return { requests: fetched.length, bytes: sum(fetched), filmRequests: film.length, filmBytes: sum(film) };
      };
      await page.goto(origin + "/", { waitUntil: "commit" });
      await page.waitForFunction(() => window.__playing !== null, null, { timeout: 120000, polling: 50 });
      await page.waitForTimeout(3000); // what is not needed for the first frame follows
      const first = { ...(await page.evaluate(read)), ...sizes() };
      // And again, with what the browser kept.
      fetched.length = 0;
      await page.reload({ waitUntil: "commit" });
      await page.waitForFunction(() => window.__playing !== null, null, { timeout: 120000, polling: 50 });
      await page.waitForTimeout(3000);
      runs.push({ first, again: { ...(await page.evaluate(read)), ...sizes() } });
      await context.close();
    }
    server.close();
    const typical = [...runs].sort((a, b) => a.first.frame - b.first.frame)[Math.floor(RUNS / 2)];
    const row = (which) => ({
      playing: median(runs.map((run) => run[which].playing)),
      frame: median(runs.map((run) => run[which].frame)),
      requests: typical[which].requests,
      bytes: typical[which].bytes,
      filmRequests: typical[which].filmRequests,
      filmBytes: typical[which].filmBytes,
    });
    console.log(JSON.stringify({ line, page: name, first: row("first"), again: row("again"), marks: typical.first.marks }));
  }
}
await browser.close();
