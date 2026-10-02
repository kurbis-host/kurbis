import { randomBytes, randomUUID } from "node:crypto";
import { blake3 } from "@noble/hashes/blake3";
import AxeBuilder from "@axe-core/playwright";
import { expect, test, type APIRequestContext, type Page } from "@playwright/test";
import { Pool } from "pg";

const api = process.env.BROWSER_API_URL ?? "http://127.0.0.1:18080";
const fixture = process.env.BROWSER_FIXTURE_URL ?? "http://127.0.0.1:18081";
const panel = process.env.BROWSER_PANEL_URL ?? "http://127.0.0.1:18082";
const pool = new Pool({ connectionString: process.env.TEST_DATABASE_URL });
const accessDeniedStatuses = [401, 403, 404];

function workerLeaseId(token: string) {
  const hex = Buffer.from(blake3(Buffer.from(token)).slice(0, 16)).toString("hex");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

async function loginAndCreateServer(page: Page) {
  await page.goto("/login");
  await page.getByRole("link", { name: "Continue with Google" }).click();
  await expect.poll(async () => page.evaluate(async () => {
    const response = await fetch("/api/v1/session");
    return response.ok ? (await response.json()).user.display_name : null;
  })).toBe("Browser Player");
  await expect(page).toHaveURL(/\/app(?:\/setup|\/servers)$/);
  if (!page.url().endsWith("/app/setup")) {
    await page.getByRole("link", { name: "Create a world" }).first().click();
    await expect(page).toHaveURL(/\/app\/setup$/);
  }
  await page.getByPlaceholder("friends-world").fill("server-access-world");
  await page.getByRole("button", { name: /Create sleeping world/ }).click();
  await expect(page).toHaveURL(/\/manage\/[0-9a-f-]+$/);
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
  const leaseId = workerLeaseId(workerToken);
  await pool.query(
    `INSERT INTO worker_credentials(worker_id, api_token_hash, lease_id, lease_expires_at, revoked_at)
     VALUES($1, $2, $3, now() + interval '1 hour', NULL)
     ON CONFLICT(worker_id) DO UPDATE SET
       api_token_hash = EXCLUDED.api_token_hash,
       lease_id = EXCLUDED.lease_id,
       lease_expires_at = EXCLUDED.lease_expires_at,
       revoked_at = NULL`,
    [workerId, Buffer.from(blake3(Buffer.from(workerToken))), leaseId],
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
    headers: { authorization: "Bearer browser-internal-token", "x-worker-lease": leaseId },
    data: { server_id: serverId, generation: 1 },
  });
  expect(sharedTokenAttempt.status()).toBe(401);
  const forgedWorkerAttempt = await request.post(`${api}/internal/worker/server-capabilities`, {
    headers: { authorization: `Bearer ${workerToken}`, "x-worker-lease": leaseId },
    data: { server_id: serverId, worker_id: randomUUID(), generation: 1 },
  });
  expect(forgedWorkerAttempt.status()).toBeGreaterThanOrEqual(400);
  expect(forgedWorkerAttempt.status()).toBeLessThan(500);

  const issued = await request.post(`${api}/internal/worker/server-capabilities`, {
    headers: { authorization: `Bearer ${workerToken}`, "x-worker-lease": leaseId },
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

async function createScopedWorldLink(request: APIRequestContext, subdomain: string, portOffset: number) {
  const ownerId = randomUUID();
  const serverId = randomUUID();
  const uniqueSubdomain = `${subdomain}-${serverId.slice(0, 8)}`;
  const worker = await pool.query("SELECT id, port_min FROM workers WHERE name = 'browser-worker'");
  expect(worker.rowCount).toBe(1);
  const workerId = worker.rows[0].id as string;
  const javaPort = (worker.rows[0].port_min as number) + portOffset;
  await pool.query("INSERT INTO users(id, display_name, account_status) VALUES($1, $2, 'active')", [
    ownerId,
    `${uniqueSubdomain} owner`,
  ]);
  await pool.query("INSERT INTO servers(id, owner_id, subdomain, generation) VALUES($1, $2, $3, 1)", [
    serverId,
    ownerId,
    uniqueSubdomain,
  ]);
  await pool.query(
    `INSERT INTO placements(server_id, worker_id, generation, lease_token,
                            java_port, bedrock_port, signal_port, active)
     VALUES($1, $2, 1, $3, $4, $5, $6, true)`,
    [serverId, workerId, randomUUID(), javaPort, javaPort + 1, javaPort + 2],
  );
  const capability = randomBytes(32).toString("base64url");
  await pool.query(
    `INSERT INTO server_instance_capabilities(server_id, worker_id, placement_generation, token_hash)
     VALUES($1, $2, 1, $3)`,
    [serverId, workerId, Buffer.from(blake3(Buffer.from(capability)))],
  );
  const minted = await request.post(`${api}/api/v1/server/login-links`, {
    headers: { authorization: `Bearer ${capability}` },
  });
  expect(minted.status(), await minted.text()).toBe(200);
  const link = new URL((await minted.json()).url as string);
  expect(link.origin).toBe(panel);
  return { serverId, link, subdomain: uniqueSubdomain };
}

async function tickFixture(request: APIRequestContext) {
  const response = await request.post(`${fixture}/fixture/tick`);
  expect(response.ok(), await response.text()).toBeTruthy();
  return response.json() as Promise<{ operation: string | null }>;
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

test("account and multiple scoped grants share the selector with independent revocation", async ({ page, request }) => {
  const accountServerId = await loginAndCreateServer(page);
  const accountGrant = await mintCurrentPlacementLink(request, accountServerId);
  const first = await createScopedWorldLink(request, "scoped-shared-one", 10);
  const second = await createScopedWorldLink(request, "scoped-shared-two", 20);
  const third = await createScopedWorldLink(request, "scoped-shared-three", 30);

  for (const grant of [{ serverId: accountServerId, link: accountGrant }, first, second, third]) {
    await page.goto(`${grant.link.pathname}${grant.link.hash}`);
    await expect(page).toHaveURL(`${panel}/manage/${grant.serverId}`);
    await expect(page.getByRole("link", { name: "Account" })).toBeVisible();
  }

  const ownerCookie = (await page.context().cookies()).find((cookie) => cookie.name === "kurbis_session");
  const grantCookie = (await page.context().cookies()).find((cookie) => cookie.name === "kurbis_server_session");
  expect(ownerCookie).toBeTruthy();
  expect(grantCookie).toBeTruthy();
  const grantHash = Buffer.from(blake3(Buffer.from(grantCookie!.value)));
  await pool.query(
    "UPDATE server_session_grants SET created_at = now() - interval '365 days' WHERE token_hash = $1",
    [grantHash],
  );
  await pool.query(
    `UPDATE server_sessions SET created_at = now() - interval '365 days'
     WHERE grant_id = (SELECT id FROM server_session_grants WHERE token_hash = $1)`,
    [grantHash],
  );
  const authorizationExpiryColumns = await pool.query(
    `SELECT table_name, column_name
     FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name IN ('server_session_grants', 'server_sessions')
       AND column_name IN ('expires_at', 'lease_expires_at')`,
  );
  expect(authorizationExpiryColumns.rows).toEqual([]);

  const accessibleIds = await page.evaluate(async () => {
    const response = await fetch("/api/v1/servers");
    if (!response.ok) return { status: response.status, ids: [] };
    const rows = await response.json();
    return { status: response.status, ids: rows.map((row: { id: string }) => row.id).sort() };
  });
  expect(accessibleIds).toEqual({
    status: 200,
    ids: [accountServerId, first.serverId, second.serverId, third.serverId].sort(),
  });

  const foreignOwnerId = randomUUID();
  const foreignServerId = randomUUID();
  await pool.query("INSERT INTO users(id, display_name, account_status) VALUES($1, $2, 'active')", [foreignOwnerId, "Foreign owner"]);
  await pool.query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)", [
    foreignServerId,
    foreignOwnerId,
    `scoped-foreign-${foreignServerId.slice(0, 8)}`,
  ]);

  await page.goto(`${panel}/manage/${first.serverId}`);
  const initialSelector = page.getByRole("combobox", { name: "Select server" });
  await expect(initialSelector.locator("option")).toHaveCount(4);
  // Login links are minted from an active placement, but the durable browser grant
  // remains valid after that placement ends. Exercise lifecycle authorization from
  // the normal sleeping state instead of asking start to race an existing placement.
  await pool.query("UPDATE placements SET active = false WHERE server_id = $1", [first.serverId]);
  const acceptedCommand = page.waitForResponse((response) =>
    response.url().endsWith(`/api/v1/servers/${first.serverId}/commands`) && response.request().method() === "POST",
  );
  await page.getByRole("button", { name: "Start", exact: true }).click();
  const response = await acceptedCommand;
  expect(response.status()).toBe(202);
  expect(response.request().postDataJSON()).toEqual({ command: "start" });
  expect(await tickFixture(request)).toMatchObject({ operation: "start" });
  await expect(page.getByRole("link", { name: "Account" })).toBeVisible();

  await page.goto("/app/servers");
  await expect(page.getByRole("listitem").filter({ hasText: "server-access-world" })).toHaveCount(1);
  const firstServerRow = page.getByRole("listitem").filter({ hasText: first.subdomain });
  await expect(firstServerRow).toBeVisible();
  await firstServerRow.getByRole("button", { name: "Revoke access" }).click();
  await expect(firstServerRow).toHaveCount(0);
  const revokedServer = await page.request.get(`${panel}/manage/${first.serverId}`);
  expect(revokedServer.status()).toBe(200);
  expect(new URL(revokedServer.url()).pathname).toBe("/manage/access-unavailable");
  expect(await revokedServer.text()).toContain("Server access unavailable");

  await page.goto(`${panel}/manage/${second.serverId}`);
  const selector = page.getByRole("combobox", { name: "Select server" });
  await expect(selector.locator("option")).toHaveCount(3);
  await expect(page.getByRole("link", { name: "Account" })).toBeVisible();
  expect((await page.context().cookies()).some((cookie) => cookie.name === "kurbis_session")).toBe(true);

  const ownerHash = Buffer.from(blake3(Buffer.from(ownerCookie!.value)));
  await pool.query("DELETE FROM sessions WHERE token_hash = $1", [ownerHash]);
  await page.context().clearCookies({ name: "kurbis_session" });
  await page.context().addCookies([{ ...ownerCookie!, value: "invalid-account-session" }]);
  await page.clock.setFixedTime(new Date(Date.now() + 365 * 24 * 60 * 60 * 1000));
  await page.goto(`${panel}/manage/${second.serverId}`);
  await expect(page.getByText("Server only • no Kurbis account")).toBeVisible();
  await expect(page.getByRole("combobox", { name: "Select server" }).locator("option")).toHaveCount(3);
  expect(await page.evaluate(async () => (await fetch("/api/v1/account")).status)).toBe(401);
  await pool.query("UPDATE placements SET active = false WHERE server_id = $1", [second.serverId]);
  const scopedCommand = page.waitForResponse((res) =>
    res.url().endsWith(`/api/v1/servers/${second.serverId}/commands`) && res.request().method() === "POST",
  );
  await page.getByRole("button", { name: "Start", exact: true }).click();
  const scopedResponse = await scopedCommand;
  expect(scopedResponse.status()).toBe(202);
  expect(scopedResponse.request().postDataJSON()).toEqual({ command: "start" });
  expect(await tickFixture(request)).toMatchObject({ operation: "start" });

  const foreignCommandStatus = await page.evaluate(async (serverId) => (await fetch(`/api/v1/servers/${serverId}/commands`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ command: "start" }),
  })).status, foreignServerId);
  expect(accessDeniedStatuses).toContain(foreignCommandStatus);
  const foreignReadStatuses = await page.evaluate(async (serverId) => Promise.all([
    fetch(`/api/v1/servers/${serverId}`).then((response) => response.status),
    fetch(`/api/v1/servers/${serverId}/events`).then((response) => response.status),
  ]), foreignServerId);
  for (const status of foreignReadStatuses) expect(accessDeniedStatuses).toContain(status);

  await pool.query("UPDATE placements SET active = false WHERE server_id = $1", [third.serverId]);
  await page.goto(`${panel}/manage/${third.serverId}/settings`);
  await page.getByRole("button", { name: "World reset", exact: true }).click();
  const resetResponse = page.waitForResponse((res) =>
    res.url().endsWith(`/api/v1/servers/${third.serverId}/commands`) && res.request().method() === "POST",
  );
  await page.getByRole("dialog").getByRole("button", { name: "Reset world", exact: true }).click();
  const resetAccepted = await resetResponse;
  expect(resetAccepted.status()).toBe(202);
  expect(resetAccepted.request().postDataJSON()).toEqual({ command: "reset", mode: "world" });
  await expect.poll(async () => (await pool.query("SELECT observed_state FROM servers WHERE id=$1", [third.serverId])).rows[0].observed_state).toBe("resetting");
  expect(await tickFixture(request)).toMatchObject({ operation: "reset" });
  await expect.poll(async () => (await pool.query(
    "SELECT s.observed_state, o.status FROM servers s JOIN operations o ON o.server_id=s.id WHERE s.id=$1 AND o.kind='reset' ORDER BY o.created_at DESC LIMIT 1",
    [third.serverId],
  )).rows[0]).toMatchObject({ observed_state: "sleeping", status: "succeeded" });

  await page.goto(`${panel}/manage/${third.serverId}/settings`);
  await page.getByRole("button", { name: "Delete server", exact: true }).click();
  const deleteResponse = page.waitForResponse((res) =>
    res.url().endsWith(`/api/v1/servers/${third.serverId}/commands`) && res.request().method() === "POST",
  );
  await page.getByRole("dialog").getByRole("button", { name: "Delete server", exact: true }).click();
  const deleteAccepted = await deleteResponse;
  expect(deleteAccepted.status(), await deleteAccepted.text()).toBe(202);
  expect(deleteAccepted.request().postDataJSON()).toEqual({ command: "delete" });
  expect(await tickFixture(request)).toMatchObject({ operation: "delete" });
  await expect.poll(async () => (await pool.query(
    "SELECT count(*) FROM servers WHERE id=$1",
    [third.serverId],
  )).rows[0].count).toBe("0");

  const remainingIds = await page.evaluate(async () => {
    const response = await fetch("/api/v1/servers");
    const rows = await response.json();
    return rows.map((row: { id: string }) => row.id).sort();
  });
  expect(remainingIds).toEqual([accountServerId, second.serverId].sort());
  expect(await page.evaluate(async () => (await fetch("/api/v1/account")).status)).toBe(401);
});

test("server login link opens a scoped panel and keeps account routes private", async ({ page, request, browser }) => {
  test.setTimeout(60000);
  const serverId = await loginAndCreateServer(page);
  const link = await mintCurrentPlacementLink(request, serverId);

  // The account owner who generated the link is signed out before redeeming it.
  await pool.query("DELETE FROM sessions");
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
  await expect(navigation.getByRole("link", { name: "Console" })).toBeFocused();
  for (const route of [
    { label: "Console", suffix: "/console", heading: "Console" },
    { label: "Backups", suffix: "/backups", heading: "Backups" },
    { label: "Players", suffix: "/players", heading: "Players" },
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
  expect(serverCookie!.expires).toBeGreaterThan(Math.floor(Date.now() / 1000) + 365 * 24 * 60 * 60);
  expect(cookies.some((cookie) => cookie.name === "kurbis_session")).toBe(false);
  const accountApiStatuses = await page.evaluate(async (expectedServerId) => {
    const [account, servers] = await Promise.all([
      fetch("/api/v1/account"),
      fetch("/api/v1/servers"),
    ]);
    const rows = servers.ok ? await servers.json() : [];
    return [account.status, servers.status, rows.length === 1 && rows[0].id === expectedServerId];
  }, serverId);
  expect(accountApiStatuses).toEqual([401, 200, true]);

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
  await expect(page).toHaveURL(`${panel}/manage/access-unavailable`);
  await expect(page.getByRole("heading", { name: "Server access unavailable" })).toBeVisible();
  await expect(page.getByText(/no longer has permission to manage this world/)).toBeVisible();

  await page.goto("/app/account");
  await expect(page).toHaveURL(`${panel}/`);
  await expect(page.getByRole("link", { name: "Sign in" })).toBeVisible();

  await page.goto(`/manage/${serverId}`);
  await expect(page.getByRole("navigation", { name: "Server panel" })).toBeVisible();
  const sessionId = await page.evaluate(async (id) => {
    const response = await fetch("/api/v1/server/sessions");
    const grants = await response.json();
    return grants.find((grant: { server_id: string }) => grant.server_id === id)?.id as string | undefined;
  }, serverId);
  expect(sessionId).toBeTruthy();
  expect(await page.evaluate(async (id) => (await fetch(`/api/v1/server/sessions/${id}`, { method: "DELETE" })).status, sessionId)).toBe(204);
  await page.reload();
  await expect(page).toHaveURL(`${panel}/manage/access-unavailable`);
  await expect(page.getByRole("heading", { name: "Server access unavailable" })).toBeVisible();
  await expect(page.getByText(/no longer has permission to manage this world/)).toBeVisible();
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
  await expect(page).toHaveURL(/\/manage\/access-unavailable$/);
  expect(redemptions).toBe(2);
});

test("server sessions are fenced when the owning account is suspended", async ({ page, request }) => {
  const serverId = await loginAndCreateServer(page);
  const link = await mintCurrentPlacementLink(request, serverId);
  await pool.query("DELETE FROM sessions");
  await page.context().clearCookies();
  await page.goto(`${link.pathname}${link.hash}`);
  await expect(page).toHaveURL(new RegExp(`/manage/${serverId}$`));
  const owner = await pool.query("SELECT owner_id FROM servers WHERE id = $1", [serverId]);
  expect(owner.rowCount).toBe(1);
  await pool.query("UPDATE users SET account_status = 'suspended' WHERE id = $1", [owner.rows[0].owner_id]);

  await page.reload();
  await expect(page).toHaveURL(`${panel}/manage/access-unavailable`);
  await expect(page.getByRole("heading", { name: "Server access unavailable" })).toBeVisible();
  const response = await page.evaluate(async (id) => (await fetch(`/api/v1/servers/${id}`)).status, serverId);
  expect(response).toBe(401);
});

test("server-only deletion reports completion and fences the old session", async ({ page, request }) => {
  const serverId = await loginAndCreateServer(page);
  const link = await mintCurrentPlacementLink(request, serverId);
  await pool.query("DELETE FROM sessions");
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
  await expect(page).toHaveURL(`${panel}/manage/access-unavailable`);
  await expect(page.getByRole("heading", { name: "Server access unavailable" })).toBeVisible();
});
