import { randomBytes, randomUUID } from "node:crypto";
import { blake3 } from "@noble/hashes/blake3";
import AxeBuilder from "@axe-core/playwright";
import { expect, test, type APIRequestContext, type Page } from "@playwright/test";
import { Pool } from "pg";

const api = process.env.BROWSER_API_URL ?? "http://127.0.0.1:18080";
const fixture = process.env.BROWSER_FIXTURE_URL ?? "http://127.0.0.1:18081";
const panel = process.env.BROWSER_PANEL_URL ?? "http://127.0.0.1:18082";
const pool = new Pool({ connectionString: process.env.TEST_DATABASE_URL });

async function loginAndCreateServer(page: Page) {
  await page.goto("/login");
  await page.getByRole("link", { name: "Continue with Google" }).click();
  await expect.poll(async () => page.evaluate(async () => {
    const response = await fetch("/api/v1/session");
    return response.ok ? (await response.json()).user.display_name : null;
  })).toBe("Browser Player");
  await expect(page).toHaveURL(/\/app\/setup$/);
  await page.getByPlaceholder("friends-world").fill("server-access-world");
  await page.getByRole("button", { name: /Create sleeping world/ }).click();
  await expect(page).toHaveURL(/\/app\/servers\/[0-9a-f-]+$/);
  return page.evaluate(async () => {
    const response = await fetch("/api/v1/servers");
    const servers = await response.json();
    return servers[0].id as string;
  });
}

async function mintCurrentPlacementLink(request: APIRequestContext, serverId: string) {
  const capacity = await request.post(`${fixture}/fixture/capacity`, { data: { available: true } });
  expect(capacity.ok(), await capacity.text()).toBeTruthy();
  const worker = await pool.query("SELECT id, port_min FROM workers WHERE name = 'browser-worker'");
  expect(worker.rowCount).toBe(1);
  const workerId = worker.rows[0].id as string;
  const javaPort = worker.rows[0].port_min as number;
  const workerToken = randomBytes(32).toString("base64url");
  await pool.query(
    `INSERT INTO worker_auth.credentials(worker_id, api_token_hash, database_role, revoked_at)
     VALUES($1, $2, 'worker_runtime', NULL)
     ON CONFLICT(worker_id) DO UPDATE SET
       api_token_hash = EXCLUDED.api_token_hash,
       database_role = EXCLUDED.database_role,
       revoked_at = NULL`,
    [workerId, Buffer.from(blake3(Buffer.from(workerToken)))],
  );
  const signalPort = javaPort + 2;
  await pool.query("UPDATE servers SET generation = 1 WHERE id = $1", [serverId]);
  await pool.query(
    `INSERT INTO placements(server_id, worker_id, generation, lease_token,
                            java_port, bedrock_port, signal_port, active)
     VALUES($1, $2, 1, $3, $4, $5, $6, true)`,
    [serverId, workerId, randomUUID(), javaPort, javaPort + 1, signalPort],
  );

  const sharedTokenAttempt = await request.post(`${api}/internal/worker/server-capabilities`, {
    headers: { authorization: "Bearer browser-internal-token" },
    data: { server_id: serverId, generation: 1 },
  });
  expect(sharedTokenAttempt.status()).toBe(401);
  const forgedWorkerAttempt = await request.post(`${api}/internal/worker/server-capabilities`, {
    headers: { authorization: `Bearer ${workerToken}` },
    data: { server_id: serverId, worker_id: randomUUID(), generation: 1 },
  });
  expect(forgedWorkerAttempt.status()).toBeGreaterThanOrEqual(400);
  expect(forgedWorkerAttempt.status()).toBeLessThan(500);

  const issued = await request.post(`${api}/internal/worker/server-capabilities`, {
    headers: { authorization: `Bearer ${workerToken}` },
    data: { server_id: serverId, generation: 1 },
  });
  expect(issued.status(), await issued.text()).toBe(200);
  const { capability } = await issued.json() as { capability: string };
  const minted = await request.post(`${api}/api/v1/server/login-links`, {
    headers: { authorization: `Bearer ${capability}` },
  });
  expect(minted.status(), await minted.text()).toBe(200);
  const { url, expires_in_seconds: expiresInSeconds } = await minted.json() as {
    url: string;
    expires_in_seconds: number;
  };
  expect(expiresInSeconds).toBe(15 * 60);
  const link = new URL(url);
  expect(link.origin).toBe(panel);
  expect(link.pathname).toBe("/auth/server");
  return link;
}

async function expireLatestLoginLink(serverId: string) {
  await pool.query(
    `UPDATE server_login_tokens SET expires_at = now() - interval '1 second'
     WHERE token_hash = (
       SELECT token_hash FROM server_login_tokens
       WHERE server_id = $1 ORDER BY created_at DESC LIMIT 1
     )`,
    [serverId],
  );
}

test.beforeEach(async ({ request }) => {
  const reset = await request.post(`${fixture}/fixture/reset`);
  expect(reset.ok(), await reset.text()).toBeTruthy();
});

test.afterAll(async () => { await pool.end(); });

test("server login link opens a scoped panel, keeps account routes private, and expires cleanly", async ({ page, request, browser }) => {
  test.setTimeout(60000);
  const serverId = await loginAndCreateServer(page);
  const link = await mintCurrentPlacementLink(request, serverId);

  // The account owner who generated the link is signed out before redeeming it.
  await page.context().clearCookies();
  await page.addInitScript(() => {
    const initialFragment = window.location.hash;
    if (initialFragment) sessionStorage.setItem("server-access-initial-fragment", initialFragment);
    const originalFetch = window.fetch.bind(window);
    window.fetch = (input, init) => {
      const rawUrl = input instanceof Request ? input.url : String(input);
      const url = new URL(rawUrl, window.location.href);
      if (url.pathname === "/auth/server/redeem") {
        sessionStorage.setItem("server-access-redemption-request", JSON.stringify({
          fragmentAtRequest: window.location.hash,
          origin: url.origin,
          method: init?.method ?? (input instanceof Request ? input.method : "GET"),
        }));
      }
      return originalFetch(input, init);
    };
  });
  await page.goto(`${link.pathname}${link.hash}`);
  await expect(page).toHaveURL(new RegExp(`/manage/${serverId}$`));
  const navigation = page.getByRole("navigation", { name: "Server panel" });
  for (const width of [1440, 820, 390]) {
    await page.setViewportSize({ width, height: width === 390 ? 844 : 1000 });
    await expect(navigation).toBeVisible();
    await expect(navigation.getByRole("link")).toHaveCount(5);
    await expect(page.getByText("Server only • no Kurbis account")).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    const navMetrics = await navigation.evaluate((element) => ({
      scrollWidth: element.scrollWidth,
      clientWidth: element.clientWidth,
      links: [...element.querySelectorAll("a")].map((link) => {
        const rect = link.getBoundingClientRect();
        return rect.left >= 0 && rect.right <= window.innerWidth && rect.top >= 0 && rect.bottom <= window.innerHeight;
      }),
    }));
    expect(navMetrics.scrollWidth).toBeLessThanOrEqual(navMetrics.clientWidth);
    expect(navMetrics.links.every(Boolean)).toBeTruthy();
    if (width !== 1440) {
      const axe = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21aa", "wcag22aa"]).analyze();
      expect(axe.violations).toEqual([]);
    }
    if (width === 1440 || width === 390) {
      await page.addStyleTag({ content: "time { visibility: hidden !important; }" });
      await expect(page).toHaveScreenshot(`server-only-overview-${width}.png`, {
        fullPage: true,
        animations: "disabled",
        caret: "hide",
        maxDiffPixelRatio: 0.01,
      });
    }
  }
  expect(await navigation.getByRole("link", { name: "Account" }).count()).toBe(0);
  await navigation.getByRole("link", { name: "Overview" }).focus();
  await page.keyboard.press("Tab");
  await expect(navigation.getByRole("link", { name: "Logs" })).toBeFocused();
  for (const route of [
    { label: "Logs", suffix: "/console", heading: "Console" },
    { label: "Snapshots", suffix: "/backups", heading: "Backups" },
    { label: "Players & whitelist", suffix: "/players", heading: "Players" },
    { label: "Settings", suffix: "/settings", heading: "World settings" },
  ]) {
    await navigation.getByRole("link", { name: route.label }).click();
    await expect(page).toHaveURL(`${panel}/manage/${serverId}${route.suffix}`);
    await expect(page.getByRole("heading", { name: route.heading, exact: true })).toBeVisible();
    await expect(page.getByText("Server only • no Kurbis account")).toBeVisible();
  }
  expect(await page.evaluate(() => window.location.hash)).toBe("");
  const redemptionRequest = await page.evaluate(() => JSON.parse(
    sessionStorage.getItem("server-access-redemption-request") ?? "null",
  ));
  expect(redemptionRequest).toEqual({ fragmentAtRequest: "", origin: panel, method: "POST" });
  expect(await page.evaluate(() => sessionStorage.getItem("server-access-initial-fragment")))
    .toMatch(/^#token=/);

  const cookies = await page.context().cookies();
  const serverCookie = cookies.find((cookie) => cookie.name === "kurbis_server_session");
  expect(serverCookie, `browser cookies: ${JSON.stringify(cookies)}`).toMatchObject({
    httpOnly: true,
    secure: true,
    sameSite: "Lax",
    path: "/",
  });
  const now = Math.floor(Date.now() / 1000);
  expect(serverCookie!.expires).toBeGreaterThan(now + 3500);
  expect(serverCookie!.expires).toBeLessThan(now + 3605);
  expect(cookies.some((cookie) => cookie.name === "kurbis_session")).toBe(false);
  const accountApiStatuses = await page.evaluate(async () => {
    const [account, servers] = await Promise.all([
      fetch("/api/v1/account"),
      fetch("/api/v1/servers"),
    ]);
    return [account.status, servers.status];
  });
  expect(accountApiStatuses).toEqual([401, 401]);

  const replayContext = await browser.newContext();
  const replayPage = await replayContext.newPage();
  await replayPage.goto(`${link.pathname}${link.hash}`);
  await expect(replayPage.getByRole("status")).toContainText("invalid, expired, or already used");
  await expect(replayPage.getByRole("status")).toContainText("run /kurbis login to request a fresh link");
  await expect(replayPage.getByRole("button", { name: "Try again" })).toHaveCount(0);
  expect((await replayContext.cookies()).some((cookie) => cookie.name === "kurbis_server_session")).toBe(false);
  await replayContext.close();

  const otherOwnerId = randomUUID();
  const otherServerId = randomUUID();
  await pool.query("INSERT INTO users(id, display_name, account_status) VALUES($1, $2, 'active')", [
    otherOwnerId,
    "Other Browser Player",
  ]);
  await pool.query(
    `INSERT INTO identities(id, user_id, provider, subject, display_name, email_verified)
     VALUES($1, $2, 'google', 'browser-other-user', 'Other Browser Player', false)`,
    [randomUUID(), otherOwnerId],
  );
  await pool.query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)", [
    otherServerId,
    otherOwnerId,
    "server-access-other-world",
  ]);
  await page.goto(`/manage/${otherServerId}`);
  await expect(page).toHaveURL(`${panel}/manage/access-expired`);
  await expect(page.getByRole("heading", { name: "Server access unavailable" })).toBeVisible();
  await expect(page.getByText(/can no longer access this world/)).toBeVisible();

  await page.goto("/app/account");
  await expect(page).toHaveURL(`${panel}/`);
  await expect(page.getByRole("link", { name: "Sign in" })).toBeVisible();

  await page.goto(`/manage/${serverId}`);
  await expect(page.getByRole("navigation", { name: "Server panel" })).toBeVisible();
  await pool.query(
    "UPDATE server_sessions SET expires_at = now() - interval '1 second' WHERE server_id = $1",
    [serverId],
  );
  await page.reload();
  await expect(page).toHaveURL(`${panel}/manage/access-expired`);
  await expect(page.getByRole("heading", { name: "Server access unavailable" })).toBeVisible();
  await expect(page.getByText(/can no longer access this world/)).toBeVisible();
});

test("malformed and expired server login links explain how to recover", async ({ page, browser, request }) => {
  await page.goto("/auth/server#token=malformed-token");
  await expect(page.getByRole("status")).toContainText("invalid, expired, or already used");
  await expect(page.getByRole("status")).toContainText("run /kurbis login to request a fresh link");
  await expect(page.getByRole("button", { name: "Try again" })).toHaveCount(0);
  expect((await page.context().cookies()).some((cookie) => cookie.name === "kurbis_server_session")).toBe(false);

  const owner = await browser.newPage();
  const serverId = await loginAndCreateServer(owner);
  const link = await mintCurrentPlacementLink(request, serverId);
  await expireLatestLoginLink(serverId);

  const guest = await browser.newPage();
  await guest.goto(`${link.pathname}${link.hash}`);
  await expect(guest.getByRole("status")).toContainText("invalid, expired, or already used");
  await expect(guest.getByRole("button", { name: "Try again" })).toHaveCount(0);
  expect((await guest.context().cookies()).some((cookie) => cookie.name === "kurbis_server_session")).toBe(false);
  await owner.close();
  await guest.close();
});

test("server login keeps a retry action for temporary redemption failures", async ({ page }) => {
  let redemptions = 0;
  await page.route("**/auth/server/redeem", async (route) => {
    redemptions += 1;
    if (redemptions === 1) {
      await route.abort();
      return;
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ server_id: "00000000-0000-0000-0000-000000000001" }),
    });
  });

  await page.goto("/auth/server#token=temporary-network-failure");
  const retry = page.getByRole("button", { name: "Try again" });
  await expect(retry).toBeVisible();
  await retry.click();
  await expect(page).toHaveURL(/\/manage\/access-expired$/);
  expect(redemptions).toBe(2);
});

test("server sessions are fenced when the owning account is suspended", async ({ page, request }) => {
  const serverId = await loginAndCreateServer(page);
  const link = await mintCurrentPlacementLink(request, serverId);
  await page.context().clearCookies();
  await page.goto(`${link.pathname}${link.hash}`);
  await expect(page).toHaveURL(new RegExp(`/manage/${serverId}$`));
  const owner = await pool.query("SELECT owner_id FROM servers WHERE id = $1", [serverId]);
  expect(owner.rowCount).toBe(1);
  await pool.query("UPDATE users SET account_status = 'suspended' WHERE id = $1", [owner.rows[0].owner_id]);

  await page.reload();
  await expect(page).toHaveURL(`${panel}/manage/access-expired`);
  await expect(page.getByRole("heading", { name: "Server access unavailable" })).toBeVisible();
  const response = await page.evaluate(async (id) => (await fetch(`/api/v1/servers/${id}`)).status, serverId);
  expect(response).toBe(401);
});

test("server-only deletion reports completion and fences the old session", async ({ page, request }) => {
  const serverId = await loginAndCreateServer(page);
  const link = await mintCurrentPlacementLink(request, serverId);
  await page.context().clearCookies();
  await page.goto(`${link.pathname}${link.hash}`);
  await page.getByRole("link", { name: "Settings", exact: true }).click();
  await expect(page).toHaveURL(`${panel}/manage/${serverId}/settings`);
  await expect(page.getByRole("button", { name: "Delete server", exact: true })).toBeEnabled();
  await page.getByRole("button", { name: "Delete server", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Delete server", exact: true }).click();

  await expect(page).toHaveURL(`${panel}/manage/server-removed`);
  await expect(page.getByRole("heading", { name: "Server removal requested" })).toBeVisible();
  expect(await page.evaluate(async (id) => (await fetch(`/api/v1/servers/${id}`)).status, serverId)).toBe(401);
  await page.goto(`/manage/${serverId}`);
  await expect(page).toHaveURL(`${panel}/manage/access-expired`);
  await expect(page.getByRole("heading", { name: "Server access unavailable" })).toBeVisible();
});
