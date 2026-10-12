import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";
import { existsSync } from "node:fs";
import path from "node:path";

// The film's bundle is exported from the art on the machine that has the art
// (art/landing/scripts/export_web.py) and is not in the repository. Without it the page
// is its own HTML, which is what the last test here checks on every machine.
const bundle = existsSync(path.join(__dirname, "../../public/landing/scene.json"));
const END = 6.2;

// The film needs a GPU to play at speed, and a headless browser draws in software unless
// it is told to use the machine's.
test.use({
  launchOptions: {
    ...(process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : {}),
    args: ["--enable-gpu", "--ignore-gpu-blocklist", ...(process.platform === "darwin" ? ["--use-angle=metal"] : [])],
  },
});

async function scrollTo(page: Page, screens: number) {
  await page.evaluate((s) => window.scrollTo({ top: s * window.innerHeight, behavior: "instant" }), screens);
  await page.waitForTimeout(900);
}

function watch(page: Page): string[] {
  const problems: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") problems.push(message.text());
  });
  page.on("pageerror", (error) => problems.push(String(error)));
  return problems;
}

test.describe("the landing film", () => {
  test.skip(!bundle, "the film's bundle is not exported on this machine");
  test.setTimeout(120000);

  test("plays from the sky to the Nether, with nothing blocked and nothing broken", async ({ page }) => {
    const problems = watch(page);
    await page.setViewportSize({ width: 960, height: 540 });
    await page.goto("/");
    await expect(page.getByRole("main")).toHaveAttribute("data-film", "playing", { timeout: 60000 });
    const canvas = page.locator("canvas").first();
    const frames: Buffer[] = [];
    for (const stop of [0.12, 1.3, 2.0, 3.4, 4.22, 4.62, END]) {
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

  test("takes a name, says it is free, and steps through to sign in", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 720 });
    await page.goto("/");
    await expect(page.getByRole("main")).toHaveAttribute("data-film", "playing", { timeout: 60000 });
    await scrollTo(page, END);
    const field = page.getByRole("textbox", { name: /Pick a name for your server/ });
    await field.click();
    await field.pressSequentially("Steve");
    await expect(field).toHaveValue("steve");
    await expect(page.getByText("steve.kurbis.host is taken. Try another name.")).toBeAttached();
    await field.fill("pumpkin-patch");
    await expect(page.getByText("pumpkin-patch.kurbis.host is free. Press Enter to step through.")).toBeAttached();
    await field.press("Enter");
    await expect(page).toHaveURL(/\/login$/);
  });

  test("can be used from the keyboard, and read without seeing it", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 720 });
    await page.goto("/");
    await expect(page.getByRole("main")).toHaveAttribute("data-film", "playing", { timeout: 60000 });
    await expect(page.getByRole("heading", { level: 1 })).toHaveText("Your world is waiting below");
    await expect(page.getByRole("heading", { level: 2 })).toHaveCount(6);
    // The page's own controls show while they have the keyboard, as a skip link does.
    await page.keyboard.press("Tab");
    await expect(page.getByRole("link", { name: "Jump to portal" })).toBeFocused();
    await expect(page.getByRole("link", { name: "Jump to portal" })).toBeInViewport();
    await page.keyboard.press("Tab");
    const lever = page.getByRole("button", { name: "Pull it: everyone leaves, and the server sleeps." });
    await expect(lever).toBeFocused();
    await page.waitForTimeout(1500); // the film goes to the cave
    await page.keyboard.press("Enter");
    await expect(page.getByRole("button", { name: "Pull it: someone joins, and the server wakes." })).toBeFocused();
    await expect(page.getByText("Nobody home. It sleeps. Every block saved.")).toBeAttached();
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
  await page.getByRole("button", { name: "Pull it: everyone leaves, and the server sleeps." }).click();
  await expect(page.getByText("Nobody home. It sleeps. Every block saved.")).toBeVisible();
  const field = page.getByRole("textbox", { name: /Pick a name for your server/ });
  await field.fill("pumpkin-patch");
  await expect(page.getByText("pumpkin-patch.kurbis.host is free. Press Enter to step through.")).toBeVisible();
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page).toHaveURL(/\/login$/);
  expect(fetched).toEqual([]);
});
