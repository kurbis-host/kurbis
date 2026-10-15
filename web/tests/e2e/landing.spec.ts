import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";
import { randomUUID } from "node:crypto";
import { existsSync } from "node:fs";
import path from "node:path";
import { Pool } from "pg";

// The film's bundle is exported from the art (art/landing/scripts/export_web.py) into
// public/landing. Without it, and without WebGL, the page is its own HTML, which is what
// the last test here checks on every machine.
const bundle = existsSync(path.join(__dirname, "../../public/landing/scene.json"));
/** Where the camera rests on the Nether's name field, and the end of the film (home, by the signs and the lectern). */
const NAME_STOP = 6.2;
const END = 6.8;
/** Where the sky is on the page, in screens: under a copy of the stretch past the credits and the lead-in, where the page comes round (lib/landing/film/ring.ts). */
const SKY = 0.6;
const pool = new Pool({ connectionString: process.env.TEST_DATABASE_URL });
const fixture = process.env.BROWSER_FIXTURE_URL;
// A server that holds the name "steve", so the name check has a taken name to find.
const holder = randomUUID();
/** A name no server has: each run makes up its own, as a run may leave its world behind. */
const freeName = () => `patch-${randomUUID().slice(0, 8)}`;

test.beforeAll(async () => {
  await pool.query("INSERT INTO users(id, display_name, account_status) VALUES($1, 'Landing Holder', 'active')", [holder]);
  await pool.query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, 'steve') ON CONFLICT (subdomain) DO NOTHING", [randomUUID(), holder]);
});

test.afterAll(async () => {
  await pool.query("DELETE FROM servers WHERE owner_id = $1", [holder]);
  await pool.query("DELETE FROM users WHERE id = $1", [holder]);
  await pool.end();
});

// The film needs a GPU to play at speed, and a headless browser draws in software unless
// it is told to use the machine's.
test.use({
  launchOptions: {
    ...(process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : {}),
    args: ["--enable-gpu", "--ignore-gpu-blocklist", ...(process.platform === "darwin" ? ["--use-angle=metal"] : [])],
  },
});

/** Scroll to a place in the film, in screens from the sky; and where the scroll is in it. */
async function scrollTo(page: Page, screens: number) {
  await page.evaluate((top) => window.scrollTo({ top: top * window.innerHeight, behavior: "instant" }), SKY + screens);
  await page.waitForTimeout(900);
}
const scrolled = (page: Page) => page.evaluate((sky) => window.scrollY / window.innerHeight - sky, SKY);

function watch(page: Page): string[] {
  const problems: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") problems.push(message.text());
  });
  page.on("pageerror", (error) => problems.push(String(error)));
  return problems;
}

test.describe("the landing film", () => {
  test.skip(!bundle, "the film's bundle is not here");
  test.setTimeout(120000);

  test.beforeEach(async ({ page }) => {
    // Drawn in software, a frame of the film takes seconds: that tests the machine, not the page.
    const renderer = await page.evaluate(() => {
      const gl = document.createElement("canvas").getContext("webgl2");
      const info = gl?.getExtension("WEBGL_debug_renderer_info");
      return gl ? String(gl.getParameter(info ? info.UNMASKED_RENDERER_WEBGL : gl.RENDERER)) : "none";
    });
    test.skip(/swiftshader|llvmpipe|software|none/i.test(renderer), `no GPU to draw the film with (${renderer})`);
  });

  test("plays from the sky to the Nether, with nothing blocked and nothing broken", async ({ page }) => {
    const problems = watch(page);
    await page.setViewportSize({ width: 960, height: 540 });
    await page.goto("/");
    await expect(page.getByRole("main")).toHaveAttribute("data-film", "playing", { timeout: 60000 });
    expect(await scrolled(page)).toBeCloseTo(0, 2); // (it opens at the sky)
    const canvas = page.locator("canvas").first();
    const frames: Buffer[] = [];
    for (const stop of [0.12, 1.3, 2.0, 3.4, 4.18, 4.38, 4.6, NAME_STOP, 6.62, END]) {
      await scrollTo(page, stop);
      const frame = await canvas.screenshot();
      // A blank canvas compresses to next to nothing; a frame of the film does not.
      expect(frame.length, `the frame at ${stop}`).toBeGreaterThan(40000);
      frames.push(frame);
    }
    for (let i = 1; i < frames.length; i += 1) expect(frames[i].equals(frames[i - 1]), `stops ${i - 1} and ${i} look different`).toBe(false);
    // Nothing the page needed was refused by its own Content-Security-Policy, and nothing threw.
    expect(problems).toEqual([]);
  });

  test("takes a name, says it is free, and carries it through sign-in to setup", async ({ page, request, context }) => {
    await context.grantPermissions(["clipboard-read", "clipboard-write"]);
    // A new person, with no world yet.
    const subject = `landing-${randomUUID()}`;
    await request.post(`${fixture}/fixture/identity`, { data: { subject } });
    const name = freeName();
    await page.setViewportSize({ width: 1280, height: 720 });
    await page.goto("/");
    await expect(page.getByRole("main")).toHaveAttribute("data-film", "playing", { timeout: 60000 });
    await scrollTo(page, NAME_STOP);
    const field = page.getByRole("textbox", { name: /Pick a name for your server/ }).last();
    await field.click();
    await field.pressSequentially("Steve");
    await expect(field).toHaveValue("steve");
    await expect(page.getByText("steve.kurbis.host is taken. Try another name.")).toBeAttached();
    await field.fill(name);
    await expect(page.getByText(`${name}.kurbis.host is free. Press Enter to claim it.`)).toBeAttached();
    await field.press("Enter");
    // The claim card: what a keyboard reaches next after the field.
    await page.keyboard.press("Tab");
    await expect(page.getByRole("button", { name: "Copy address" })).toBeFocused();
    await page.keyboard.press("Enter");
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(`${name}.kurbis.host`);
    await page.keyboard.press("Tab");
    await expect(page.getByRole("button", { name: "Or sign in" })).toBeFocused();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/login$/);
    await page.getByRole("link", { name: "Continue with Google" }).click();
    await expect(page).toHaveURL(/\/app\/setup$/);
    await expect(page.getByPlaceholder("friends-world")).toHaveValue(name);
    await request.post(`${fixture}/fixture/identity`, { data: { subject: "browser-user" } });
    const users = await pool.query("SELECT u.id FROM users u JOIN identities i ON i.user_id = u.id WHERE i.subject = $1", [subject]);
    await pool.query("DELETE FROM users WHERE id = ANY($1)", [users.rows.map((row) => row.id)]);
  });

  test("takes a name in the sky: typed anywhere, checked, and held in the claim card", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto("/");
    await expect(page.getByRole("main")).toHaveAttribute("data-film", "playing", { timeout: 60000 });
    await scrollTo(page, 0.12);
    // No need to find the field: typing on the page types into it.
    await page.keyboard.type("steve");
    const field = page.getByRole("textbox", { name: /Pick a name for your server/ }).first();
    await expect(field).toBeFocused();
    await expect(field).toHaveValue("steve");
    await expect(page.getByText("steve.kurbis.host is taken. Try another name.")).toBeAttached();
    const name = freeName();
    await field.fill(name);
    await expect(page.getByText(`${name}.kurbis.host is free. Press Enter to claim it.`)).toBeAttached();
    await field.press("Enter");
    await expect.poll(() => page.evaluate(() => (window as unknown as { kurbisFilm: { ctx: { ui: { claimed: string } } } }).kurbisFilm.ctx.ui.claimed)).toBe(name);
    // Still at the start: the claim needs no trip to the end.
    expect(await scrolled(page)).toBeCloseTo(0.12, 1);
    await page.keyboard.press("Tab");
    await expect(page.getByRole("button", { name: "Copy address" })).toBeFocused();
  });

  test("has a bar along the top: home, sign in, and the way to a name", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto("/");
    await expect(page.getByRole("main")).toHaveAttribute("data-film", "playing", { timeout: 60000 });
    await scrollTo(page, 1.3);
    // (The bar is drawn on the film's interface; at this size it sits along the top right.)
    await page.mouse.click(1340, 30);
    await expect(page.getByRole("textbox", { name: /Pick a name for your server/ }).last()).toBeFocused();
    await expect.poll(() => scrolled(page)).toBeCloseTo(NAME_STOP, 1);
    await page.mouse.click(30, 30);
    await expect.poll(() => scrolled(page)).toBeCloseTo(0, 2); // (the sky, not the page's top)
    await page.mouse.click(1170, 30);
    await expect(page).toHaveURL(/\/login$/);
  });

  test("can be used from the keyboard, and read without seeing it", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 720 });
    await page.goto("/");
    await expect(page.getByRole("main")).toHaveAttribute("data-film", "playing", { timeout: 60000 });
    await expect(page.getByRole("heading", { level: 1 })).toHaveText("Your world is waiting below");
    await expect(page.getByRole("heading", { level: 2 })).toHaveCount(11); // (the last three, the credits')
    // The page's own controls show while they have the keyboard, as a skip link does.
    await page.keyboard.press("Tab");
    await expect(page.getByRole("link", { name: "Sign in" })).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(page.getByRole("link", { name: "Get your server" })).toBeFocused();
    await expect(page.getByRole("link", { name: "Get your server" })).toBeInViewport();
    await page.keyboard.press("Tab");
    await expect(page.getByRole("textbox", { name: /Pick a name for your server/ }).first()).toBeFocused();
    await page.keyboard.press("Tab");
    const lever = page.getByRole("button", { name: "Pull it: everyone leaves, and the server stops." });
    await expect(lever).toBeFocused();
    await page.waitForTimeout(1500); // the film goes to the cave
    await page.keyboard.press("Enter");
    await expect(page.getByRole("button", { name: "Pull it: someone joins, and the server starts." })).toBeFocused();
    await expect(page.getByText("Nobody on. Server off. No phantoms.")).toBeAttached();
    const audit = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21aa", "wcag22aa"]).analyze();
    expect(audit.violations).toEqual([]);
  });

  test("stands at the stops for a visitor who asked for less motion", async ({ page }) => {
    const problems = watch(page);
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto("/");
    await expect(page.getByRole("main")).toHaveAttribute("data-film", "playing", { timeout: 60000 });
    const canvas = page.locator("canvas").first();
    await scrollTo(page, 1.3);
    const at = await canvas.screenshot();
    // A little further on is still the same stop: the camera has not moved.
    await scrollTo(page, 1.36);
    await page.waitForTimeout(600);
    expect(at.length).toBeGreaterThan(40000);
    await scrollTo(page, 3.4);
    expect((await canvas.screenshot()).equals(at)).toBe(false);
    expect(problems).toEqual([]);
  });
});

test("the film's bundle may be kept for good, where it is asked for by its version", async ({ request }) => {
  test.skip(!bundle, "the film's bundle is not here");
  // (The page asks for every file by the bundle's version, so what an address names never changes.)
  const versioned = await request.get("/landing/scene.json?v=0123456789abcdef");
  expect(versioned.headers()["cache-control"]).toBe("public, max-age=31536000, immutable");
  const plain = await request.get("/landing/scene.json");
  expect(plain.headers()["cache-control"] ?? "").not.toContain("immutable");
});

test("the bundle's packed files are sent as they are, and said to be gzip", async ({ request }) => {
  test.skip(!bundle, "the film's bundle is not here");
  // The browser unpacks them on the way in: the page reads plain bytes and unpacks nothing itself.
  for (const file of ["valley.grid.gz", "nether.grid.gz", "small.pack.gz", "models.wide.glb.gz"]) {
    const response = await request.get(`/landing/${file}`);
    expect(response.status(), file).toBe(200);
    expect(response.headers()["content-encoding"], file).toBe("gzip");
    // (Said to be gzip in its type as well, a file is left packed by some browsers.)
    expect(response.headers()["content-type"], file).toBe("application/octet-stream");
    const body = await response.body();
    expect([body[0], body[1]], `${file} arrives unpacked`).not.toEqual([0x1f, 0x8b]);
  }
});

test("without WebGL the landing page is its own HTML, and fetches nothing of the film", async ({ page }) => {
  const fetched: string[] = [];
  page.on("request", (request) => {
    if (new URL(request.url()).pathname.startsWith("/landing/")) fetched.push(request.url());
  });
  await page.addInitScript(() => {
    const original = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function (this: HTMLCanvasElement, kind: string, ...rest: unknown[]) {
      return kind === "webgl2" ? null : (original as (...args: unknown[]) => unknown).call(this, kind, ...rest);
    } as typeof original;
  });
  await page.goto("/");
  await expect(page.getByRole("main")).toHaveAttribute("data-film", "failed");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await expect(page.getByText("No card. No ads.")).toBeVisible();
  await page.getByRole("button", { name: "Pull it: everyone leaves, and the server stops." }).click();
  await expect(page.getByText("Nobody on. Server off. No phantoms.")).toBeVisible();
  const field = page.getByRole("textbox", { name: /Pick a name for your server/ }).first();
  const name = freeName();
  await field.fill(name);
  await expect(page.getByText(`${name}.kurbis.host is free. Press Enter to claim it.`)).toBeVisible();
  await page.getByRole("button", { name: "Continue" }).first().click();
  const card = page.getByRole("region", { name: `${name}.kurbis.host` });
  await expect(card).toBeVisible();
  await card.getByRole("button", { name: "Or sign in" }).click();
  await expect(page).toHaveURL(/\/login$/);
  expect(fetched).toEqual([]);
});
