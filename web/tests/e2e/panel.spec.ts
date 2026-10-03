import { randomBytes, randomUUID } from 'node:crypto';
import { blake3 } from '@noble/hashes/blake3';
import { test, expect, Page, APIRequestContext } from '@playwright/test';
import { Pool } from 'pg';
import AxeBuilder from '@axe-core/playwright';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';

const pool = new Pool({ connectionString: process.env.TEST_DATABASE_URL });
const api = process.env.BROWSER_API_URL ?? 'http://127.0.0.1:18080';
const fixture = process.env.BROWSER_FIXTURE_URL ?? 'http://127.0.0.1:18081';
const panel = process.env.BROWSER_PANEL_URL ?? 'http://127.0.0.1:18082';
const artifactRoot = process.env.BROWSER_OUTPUT_DIR ?? path.resolve('../output/playwright');
const fixtureDataRoot = process.env.BROWSER_FIXTURE_DATA_DIR;
const objectStoreRoot = process.env.BROWSER_OBJECT_STORE_ROOT;
async function capture(page: Page, name: string) {
  const screenshot = path.join(artifactRoot, 'screenshots', name);
  await mkdir(path.dirname(screenshot), { recursive: true });
  await page.screenshot({ path: screenshot, fullPage: true });
}
async function writeFixtureWorld(serverId: string, bytes: string) {
  if (!fixtureDataRoot) throw new Error('BROWSER_FIXTURE_DATA_DIR is required');
  const world = path.join(fixtureDataRoot, 'worlds', serverId, 'world');
  await mkdir(world, { recursive: true });
  await writeFile(path.join(world, 'level.dat'), bytes);
}
async function snapshotWorldFile(serverId: string, generation: number) {
  if (!objectStoreRoot) throw new Error('BROWSER_OBJECT_STORE_ROOT is required');
  const result = await pool.query(
    'SELECT manifest_key FROM snapshots WHERE server_id = $1 AND generation = $2 AND complete',
    [serverId, generation],
  );
  if (!result.rowCount) return null;
  const serverStore = path.join(objectStoreRoot, "servers", serverId);
  const manifest = JSON.parse(await readFile(path.join(serverStore, result.rows[0].manifest_key), 'utf8'));
  const file = manifest.files.find((entry: { path: string }) => entry.path === 'world/level.dat');
  return file
    ? readFile(path.join(serverStore, 'chunks', file.blake3), 'utf8')
    : null;
}
async function captureResponsive(page: Page, route: string, name = route === '/' ? 'landing' : route.replaceAll('/', '-'), compareVisual = false, snapshotName = name) {
  for (const width of [1440, 820, 390]) {
    await page.setViewportSize({ width, height: 1000 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), `${route} at ${width}px`).toBeTruthy();
    const navigation = page.getByRole('navigation', { name: 'Server panel' });
    if (await navigation.count()) {
      const metrics = await navigation.evaluate(element => ({
        scrollWidth: element.scrollWidth,
        clientWidth: element.clientWidth,
        links: [...element.querySelectorAll('a')].map(link => {
          const rect = link.getBoundingClientRect();
          return rect.left >= 0 && rect.right <= window.innerWidth && rect.top >= 0 && rect.bottom <= window.innerHeight;
        }),
      }));
      expect(metrics.scrollWidth, `${route} navigation at ${width}px`).toBeLessThanOrEqual(metrics.clientWidth);
      expect(metrics.links.every(Boolean), `${route} links are visible at ${width}px`).toBeTruthy();
    }
    await capture(page, `${name}-${width}.png`);
    if (compareVisual && (width === 1440 || width === 390)) {
      await page.addStyleTag({ content: "time { visibility: hidden !important; }" });
      await expect(page).toHaveScreenshot(`${snapshotName.replaceAll('/', '-')}-${width}.png`, {
        fullPage: true,
        animations: 'disabled',
        caret: 'hide',
        maxDiffPixelRatio: 0.01,
      });
    }
  }
}
function visualRouteName(route: string) {
  if (route.endsWith('/console')) return 'console';
  if (route.endsWith('/backups')) return 'backups';
  if (route.endsWith('/players')) return 'players';
  if (route.endsWith('/settings')) return 'settings';
  if (route === '/app/account') return 'account';
  return 'overview';
}
async function command(request: APIRequestContext, name: string, data?: unknown) {
  const response = await request.post(`${fixture}/fixture/${name}`, { data });
  expect(response.ok(), await response.text()).toBeTruthy();
  return response;
}
type MinecraftIdentity = { kind: 'java'; uuid: string } | { kind: 'bedrock'; xuid: string };
function minecraftPlayerId(identity: MinecraftIdentity) {
  if (identity.kind === 'java') return identity.uuid;
  return BigInt(identity.xuid).toString(16).padStart(32, '0').replace(
    /^(........)(....)(....)(....)(............)$/,
    '$1-$2-$3-$4-$5',
  );
}
async function issueMinecraftLink(request: APIRequestContext, identity: MinecraftIdentity) {
  const response = await request.post(`${api}/internal/minecraft-login-links`, {
    headers: { authorization: 'Bearer browser-internal-token' },
    data: { identity, minecraft_player_id: minecraftPlayerId(identity), display_name: 'Minecraft Player' },
  });
  expect(response.status(), await response.text()).toBe(200);
  return (await response.json()).url as string;
}
async function login(page: Page, name = 'Browser Player', provider = 'Google') {
  await page.goto('/login');
  await page.getByRole('link', { name: `Continue with ${provider}` }).click();
  await expect.poll(async () => {
    const session = await browserApi(page, '/api/v1/session');
    return session.status === 200 ? JSON.parse(session.text).user.display_name : null;
  }).toBe(name);
  await expect(page).toHaveURL(/\/app(?:\/setup|\/servers)$/);
  if (new URL(page.url()).pathname === '/app/servers') {
    await page.getByRole('link', { name: 'Create a world' }).first().click();
    await expect(page).toHaveURL(/\/app\/setup$/);
  }
  await expect(page.getByPlaceholder('friends-world')).toBeVisible();
}
async function state(page: Page, value: string) {
  await expect(page.getByRole('heading', { name: value, exact: true })).toBeVisible();
}
async function tickUntil(page: Page, request: APIRequestContext, value: string, attempts = 4) {
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    await command(request, 'tick');
    await page.reload();
    if (await page.getByRole('heading', { name: value, exact: true }).isVisible()) return;
  }
  await state(page, value);
}
function workerLeaseId(token: string) {
  const hex = Buffer.from(blake3(Buffer.from(token)).slice(0, 16)).toString('hex');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
async function startWorldOnline(page: Page, request: APIRequestContext, worldName: string) {
  await login(page);
  await create(page, worldName);
  const id = await serverId(page);
  const capacity = await command(request, 'capacity', { available: true });
  expect(capacity.status()).toBe(204);
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await state(page, 'Restoring');
  await tickUntil(page, request, 'Online');
  await command(request, 'capacity', { available: true });
  const placement = await pool.query('SELECT worker_id FROM placements WHERE server_id = $1 AND active', [id]);
  expect(placement.rowCount).toBe(1);
  const workerToken = randomBytes(32).toString('base64url');
  await pool.query(
    `INSERT INTO worker_credentials(worker_id, api_token_hash, lease_id, lease_expires_at, revoked_at)
     VALUES($1, $2, $3, now() + interval '1 hour', NULL)
     ON CONFLICT(worker_id) DO UPDATE SET api_token_hash = EXCLUDED.api_token_hash,
       lease_id = EXCLUDED.lease_id, lease_expires_at = EXCLUDED.lease_expires_at, revoked_at = NULL`,
    [placement.rows[0].worker_id, Buffer.from(blake3(Buffer.from(workerToken))), workerLeaseId(workerToken)],
  );
  return id;
}
async function create(page: Page, name: string) {
  await page.getByPlaceholder('friends-world').fill(name);
  await page.getByRole('button', { name: /Create sleeping world/ }).click();
  await state(page, 'Sleeping');
  await expect(page).toHaveURL(/\/manage\/[0-9a-f-]+$/);
}
async function browserApi(page: Page, path: string, method = 'GET', data?: unknown) {
  const cookie = (await page.context().cookies()).map(({ name, value }) => `${name}=${value}`).join('; ');
  const response = await page.request.fetch(`${panel}${path}`, {
    method,
    headers: { ...(data ? { 'content-type': 'application/json' } : {}), ...(cookie ? { Cookie: cookie } : {}) },
    data,
  });
  return { status: response.status(), text: await response.text() };
}
async function current(page: Page) {
  const response = await browserApi(page, '/api/v1/servers');
  expect(response.status).toBe(200);
  const servers = JSON.parse(response.text) as Array<{
    id: string;
    is_default: boolean;
    subdomain: string;
    settings: { motd: string; whitelist: boolean; difficulty: string; gamemode: string };
  }>;
  return servers.find((server) => server.is_default) ?? servers[0] ?? null;
}
async function serverId(page: Page) {
  const server = await current(page);
  expect(server).not.toBeNull();
  return server!.id;
}
async function serverApiPath(page: Page, suffix = '') {
  return `/api/v1/servers/${await serverId(page)}${suffix}`;
}
async function serverRoute(page: Page, suffix = '') {
  return `/manage/${await serverId(page)}${suffix}`;
}
async function resetFromSettings(page: Page, request: APIRequestContext, id: string, mode: 'world' | 'full') {
  await page.goto(`/manage/${id}/settings`);
  const trigger = mode === 'world' ? 'World reset' : 'Full server reset';
  const confirmation = mode === 'world' ? 'Reset world' : 'Reset full server';
  await page.getByRole('button', { name: trigger, exact: true }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  const accepted = page.waitForResponse((response) =>
    response.url().endsWith(`/api/v1/servers/${id}/commands`) && response.request().method() === 'POST',
  );
  await dialog.getByRole('button', { name: confirmation, exact: true }).click();
  const acceptedResponse = await accepted;
  expect(acceptedResponse.status()).toBe(202);
  expect(acceptedResponse.request().postDataJSON()).toEqual({ command: 'reset', mode });
  await expect.poll(async () => {
    const result = await pool.query('SELECT observed_state FROM servers WHERE id = $1', [id]);
    return result.rows[0]?.observed_state;
  }).toBe('resetting');
  const result = await command(request, 'tick');
  const resultBody = await result.text();
  if (!result.headers()['content-type']?.includes('application/json')) {
    const diagnostics = await pool.query(
      `SELECT s.generation, s.current_snapshot_generation, s.storage_bytes,
              op.created_at AS operation_created_at,
              snap.generation AS snapshot_generation, snap.size_bytes,
              snap.reason, snap.complete, snap.created_at AS snapshot_created_at
       FROM servers s
       LEFT JOIN operations op ON op.server_id = s.id AND op.kind = 'reset'
       LEFT JOIN snapshots snap ON snap.server_id = s.id AND snap.reason = 'reset'
       WHERE s.id = $1
       ORDER BY op.created_at DESC NULLS LAST, snap.generation DESC NULLS LAST`,
      [id],
    );
    throw new Error(`fixture tick failed: ${resultBody}; diagnostics=${JSON.stringify(diagnostics.rows)}`);
  }
  expect((JSON.parse(resultBody) as { operation: string }).operation).toBe('reset');
  await expect.poll(async () => {
    const state = await pool.query('SELECT observed_state FROM servers WHERE id = $1', [id]);
    const operation = await pool.query(
      "SELECT status FROM operations WHERE server_id = $1 AND kind = 'reset' ORDER BY created_at DESC LIMIT 1",
      [id],
    );
    return [state.rows[0]?.observed_state, operation.rows[0]?.status];
  }).toEqual(['sleeping', 'succeeded']);
  await page.goto(`/manage/${id}`);
  await state(page, 'Sleeping');
}

test.beforeEach(async ({ request }) => { await command(request, 'reset'); });
test.afterAll(async () => { await pool.end(); });

test('account server list and selector open the shared server route without an owner alias', async ({ page }) => {
  await login(page);
  await create(page, 'account-world-one');
  const firstId = await serverId(page);
  const owner = await pool.query('SELECT owner_id FROM servers WHERE id = $1', [firstId]);
  const secondId = randomUUID();
  await pool.query(
    'INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)',
    [secondId, owner.rows[0].owner_id, 'account-world-two'],
  );

  await page.goto('/app/servers');
  await expect(page.getByRole('link', { name: /account-world-one/ })).toBeVisible();
  await expect(page.getByRole('link', { name: /account-world-two/ })).toBeVisible();
  const selector = page.getByRole('combobox', { name: 'Select server' });
  await expect(selector.locator('option')).toHaveCount(2);
  await selector.selectOption(secondId);
  await expect(page).toHaveURL(`${panel}/manage/${secondId}`);
  await expect(page.getByRole('main').getByText('account-world-two.mc.example.test').first()).toBeVisible();

  const legacyOwnerRoute = await page.request.get(`${panel}/app/servers/${secondId}`);
  expect(legacyOwnerRoute.status()).toBe(404);
  const legacyEventsRoute = await page.request.get(`${panel}/api/events?server_id=${secondId}`);
  expect(legacyEventsRoute.status()).toBe(404);
});

test('panel routes support AA accessibility, responsive widths and keyboard dialog control', async ({ page, request, browser }, testInfo) => {
  test.setTimeout(120000);
  const axeResults: Record<string, unknown> = {};
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.goto('/');
  await expect(page.getByRole('link', { name: 'Sign in' })).toBeVisible();
  const publicAudit = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa']).analyze();
  if (publicAudit.violations.length) axeResults['/'] = publicAudit.violations;
  await captureResponsive(page, '/', 'landing', true);
  await page.goto('/login');
  await expect(page.getByRole('link', { name: 'Continue with Google' })).toBeVisible();
  await expect(page.getByRole('link', { name: 'Continue with Discord' })).toBeVisible();
  const loginAudit = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa']).analyze();
  if (loginAudit.violations.length) axeResults['/login'] = loginAudit.violations;
  await captureResponsive(page, '/login');
  await login(page);
  await expect(page).toHaveURL(/\/app\/setup$/);
  const setupAudit = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa']).analyze();
  if (setupAudit.violations.length) axeResults['/app/setup'] = setupAudit.violations;
  await captureResponsive(page, '/app/setup', 'setup', true);
  await create(page, 'accessible-world');
  const routes = [
    await serverRoute(page),
    await serverRoute(page, '/console'),
    await serverRoute(page, '/backups'),
    await serverRoute(page, '/players'),
    await serverRoute(page, '/settings'),
    '/app/account',
  ];
  for (const route of routes) {
    await page.goto(route);
    await expect(page.getByRole('main')).toBeVisible();
    const audit = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa']).analyze();
    if (audit.violations.length) axeResults[route] = audit.violations;
    await captureResponsive(page, route, `routes/${route.replaceAll('/', '-')}`, route !== '/app/account', visualRouteName(route));
  }
  await page.setViewportSize({ width: 1440, height: 1000 });
  await expect(page.getByText('Live updates connected')).toBeVisible();
  await page.goto(await serverRoute(page, '/settings'));
  const trigger = page.getByRole('button', { name: 'Delete server', exact: true });
  await trigger.focus();
  await page.keyboard.press('Enter');
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole('button', { name: 'Cancel', exact: true })).toBeFocused();
  await page.keyboard.press('Tab');
  await expect(dialog.getByRole('button', { name: 'Delete server', exact: true })).toBeFocused();
  await page.keyboard.press('Tab');
  await expect(dialog.getByRole('button', { name: 'Cancel', exact: true })).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(dialog).not.toBeVisible();
  await expect(trigger).toBeFocused();
  expect(await page.evaluate(() => getComputedStyle(document.documentElement).scrollBehavior)).toBe('auto');

  await page.goto(await serverRoute(page));
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await state(page, 'Queued');
  const queuedAudit = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa']).analyze();
  if (queuedAudit.violations.length) axeResults[`${await serverRoute(page)} (queued)`] = queuedAudit.violations;
  await captureResponsive(page, await serverRoute(page), 'states/queued', true);
  await page.getByRole('button', { name: 'Stop', exact: true }).click();
  await state(page, 'Sleeping');
  await command(request, 'capacity', { available: true });
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await state(page, 'Restoring');
  await tickUntil(page, request, 'Online');
  const onlineAudit = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa']).analyze();
  if (onlineAudit.violations.length) axeResults[`${await serverRoute(page)} (online)`] = onlineAudit.violations;
  await captureResponsive(page, await serverRoute(page), 'states/online', true);
  await pool.query(
    "UPDATE servers SET observed_state = 'failed', last_error = $1 WHERE subdomain = $2",
    ['Fixture visual state: readiness timed out', 'accessible-world'],
  );
  await page.reload();
  await state(page, 'Needs attention');
  const failedAudit = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa']).analyze();
  if (failedAudit.violations.length) axeResults[`${await serverRoute(page)} (failed)`] = failedAudit.violations;
  await captureResponsive(page, await serverRoute(page), 'states/failed', true);
  await testInfo.attach('axe-route-results.json', {
    body: Buffer.from(JSON.stringify(axeResults, null, 2)),
    contentType: 'application/json',
  });
  expect(axeResults).toEqual({});

  const accountCookie = (await page.context().cookies()).find(cookie => cookie.name === 'kurbis_session');
  expect(accountCookie).toBeTruthy();
  const eventContext = await browser.newContext();
  await eventContext.addCookies([{
    name: accountCookie!.name,
    value: accountCookie!.value,
    url: panel,
    httpOnly: accountCookie!.httpOnly,
    secure: accountCookie!.secure,
    sameSite: accountCookie!.sameSite,
    expires: accountCookie!.expires,
  }]);
  const eventPage = await eventContext.newPage();
  await eventPage.goto(`${panel}/login`);
  expect((await eventContext.cookies(panel)).some(cookie => cookie.name === 'kurbis_session')).toBe(true);
  const eventServerId = await serverId(page);
  const eventsPath = `/api/v1/servers/${encodeURIComponent(eventServerId)}/events`;
  const liveMotd = 'Live server event reached the browser';
  const eventPromise = eventPage.evaluate((path) => new Promise<string>((resolve, reject) => {
    const source = new EventSource(path, { withCredentials: true });
    const timeout = window.setTimeout(() => {
      source.close();
      reject(new Error('SSE did not deliver the committed server update'));
    }, 5000);
    source.addEventListener('server', event => {
      const data = (event as MessageEvent<string>).data;
      if (JSON.parse(data).data?.settings?.motd === 'Live server event reached the browser') {
        window.clearTimeout(timeout);
        source.close();
        resolve(data);
      }
    });
    source.onopen = () => {
      sessionStorage.setItem('server-event-source-open', 'true');
    };
  }), eventsPath);
  await eventPage.waitForFunction(() => sessionStorage.getItem('server-event-source-open') === 'true');
  await pool.query(
    "UPDATE servers SET settings = jsonb_set(settings, '{motd}', to_jsonb($2::text), true) WHERE id = $1",
    [eventServerId, liveMotd],
  );
  const rawEvent = await eventPromise;
  await eventContext.close();
  expect(JSON.parse(rawEvent)).toMatchObject({ type: 'server', data: { id: expect.any(String) } });
});

test('world and full reset preserve their distinct settings, whitelist, and recovery guarantees', async ({ page, request }) => {
  await login(page);
  await create(page, 'reset-world');
  const id = await serverId(page);
  await command(request, 'capacity', { available: true });
  const settingsPath = await serverApiPath(page);
  const updated = await browserApi(page, settingsPath, 'PATCH', {
    settings: { motd: 'Keep this through a world reset', whitelist: true },
  });
  expect(updated.status).toBe(200);
  const playerId = randomUUID();
  await pool.query(
    `INSERT INTO server_known_players(server_id, player_id, name, edition)
     VALUES($1, $2, 'PersistentPlayer', 'java')`,
    [id, playerId],
  );
  const added = await browserApi(page, `${settingsPath}/whitelist`, 'POST', {
    name: 'PersistentPlayer',
  });
  expect(added.status).toBe(201);
  await writeFixtureWorld(id, 'world data before world reset');

  await resetFromSettings(page, request, id, 'world');
  let server = await current(page);
  expect(server?.subdomain).toBe('reset-world');
  expect(server?.settings).toMatchObject({
    motd: 'Keep this through a world reset',
    whitelist: true,
  });
  let whitelist = await pool.query('SELECT player_id FROM whitelist_entries WHERE server_id = $1', [id]);
  expect(whitelist.rows).toEqual([{ player_id: playerId }]);
  let snapshots = await pool.query(
    "SELECT generation FROM snapshots WHERE server_id = $1 AND complete AND reason = 'reset' ORDER BY generation",
    [id],
  );
  expect(snapshots.rows).toHaveLength(2);
  const worldBackup = Number(snapshots.rows[0].generation);
  const worldReplacement = Number(snapshots.rows[1].generation);
  expect(await snapshotWorldFile(id, worldBackup)).toBe('world data before world reset');
  expect(await snapshotWorldFile(id, worldReplacement)).toBeNull();

  const changedAgain = await browserApi(page, settingsPath, 'PATCH', {
    settings: { motd: 'Full reset returns defaults' },
  });
  expect(changedAgain.status).toBe(200);
  await writeFixtureWorld(id, 'world data before full reset');
  await resetFromSettings(page, request, id, 'full');
  server = await current(page);
  expect(server?.subdomain).toBe('reset-world');
  expect(server?.settings).toMatchObject({
    motd: 'Hosted by Kurbis',
    whitelist: false,
    difficulty: 'Normal',
    gamemode: 'Survival',
  });
  whitelist = await pool.query('SELECT player_id FROM whitelist_entries WHERE server_id = $1', [id]);
  expect(whitelist.rows).toEqual([]);
  snapshots = await pool.query(
    "SELECT generation FROM snapshots WHERE server_id = $1 AND complete AND reason = 'reset' ORDER BY generation",
    [id],
  );
  expect(snapshots.rows).toHaveLength(4);
  const fullBackup = Number(snapshots.rows[2].generation);
  const fullReplacement = Number(snapshots.rows[3].generation);
  expect(await snapshotWorldFile(id, fullBackup)).toBe('world data before full reset');
  expect(await snapshotWorldFile(id, fullReplacement)).toBeNull();
});

test('OAuth, settings, capacity queue, restart, logs, final snapshot, deletion and logout', async ({ page, request }) => {
  await login(page);
  await create(page, 'browser-world');
  await expect(page.getByText('browser-world.mc.example.test', { exact: true })).toHaveCount(3);
  await expect(page.getByText('2.00 GiB', { exact: true })).toBeVisible();
  await expect(page.getByText('5.00 GiB storage', { exact: true })).toBeVisible();
  await page.goto(await serverRoute(page, '/settings'));
  const session = (await page.context().cookies()).find(cookie => cookie.name === 'kurbis_session');
  expect(session).toMatchObject({ httpOnly: true, secure: true, sameSite: 'Lax' });
  const address = page.getByRole('textbox', { name: 'Address', exact: true });
  const motd = page.getByRole('textbox', { name: 'MOTD', exact: true });
  await expect(address).toBeEnabled();
  await address.fill('renamed-world');
  await motd.fill('Draft survives repeated polling');
  // Two state polls must not erase an in-progress draft.
  await page.waitForTimeout(5500);
  await expect(address).toHaveValue('renamed-world');
  await expect(motd).toHaveValue('Draft survives repeated polling');
  await page.getByLabel('Difficulty', { exact: true }).selectOption('Hard');
  await page.getByRole('button', { name: 'Save configuration' }).click();
  await expect.poll(async () => (await current(page)).subdomain).toBe('renamed-world');
  await page.reload();
  await expect(motd).toHaveValue('Draft survives repeated polling');
  await expect(page.getByLabel('Difficulty', { exact: true })).toHaveValue('Hard');
  await page.goto(await serverRoute(page));
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await state(page, 'Queued');
  await expect(page.getByRole('link', { name: 'Settings', exact: true })).toBeVisible();
  await page.reload();
  await state(page, 'Queued');
  await page.getByRole('button', { name: 'Stop', exact: true }).click();
  await state(page, 'Sleeping');
  await command(request, 'capacity', { available: true });
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await state(page, 'Restoring');
  const blocked = await browserApi(page, await serverApiPath(page), 'PATCH', { subdomain: 'forbidden-edit' });
  expect(blocked.status).toBe(409);
  await command(request, 'tick');
  await command(request, 'tick');
  await page.reload();
  await state(page, 'Online');
  await page.goto(await serverRoute(page, '/console'));
  await expect(page.getByRole('log')).toContainText('Fixture runtime ready');
  await page.goto(await serverRoute(page));
  await page.getByRole('button', { name: 'Restart', exact: true }).click();
  await state(page, 'Stopping');
  for (let attempt = 0; attempt < 3; attempt += 1) {
    const tick = await command(request, 'tick');
    const body = await tick.text();
    if (!body.trim().startsWith('{')) {
      throw new Error(`fixture restart tick ${attempt + 1} failed: ${body}`);
    }
  }
  await page.reload();
  await state(page, 'Online');
  await page.getByRole('button', { name: 'Stop', exact: true }).click();
  await state(page, 'Stopping');
  await tickUntil(page, request, 'Sleeping');
  const result = await pool.query(`SELECT s.current_snapshot_generation, w.reserved_memory_bytes,
    (SELECT count(*) FROM snapshots sn WHERE sn.server_id = s.id AND sn.complete AND reason = 'shutdown') AS final_count,
    (SELECT count(*) FROM operations o WHERE o.server_id = s.id AND o.kind = 'start') AS starts
    FROM servers s JOIN placements p ON p.server_id = s.id JOIN workers w ON w.id = p.worker_id
    WHERE s.subdomain = 'renamed-world'`);
  expect(result.rows[0]).toMatchObject({ current_snapshot_generation: '2', reserved_memory_bytes: '0', final_count: '2', starts: '2' });
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBeTruthy();
  await capture(page, 'panel-mobile.png');
  await page.setViewportSize({ width: 1440, height: 1000 });
  await capture(page, 'panel-desktop.png');
  await page.goto(await serverRoute(page, '/settings'));
  await page.getByRole('button', { name: 'Delete server', exact: true }).click();
  const id = await serverId(page);
  const deleteResponse = page.waitForResponse(response =>
    response.url().endsWith(`/api/v1/servers/${id}/commands`) && response.request().method() === 'POST'
  );
  await page.getByRole('dialog').getByRole('button', { name: 'Delete server', exact: true }).click();
  const acceptedDelete = await deleteResponse;
  expect(acceptedDelete.status()).toBe(202);
  expect(acceptedDelete.request().postDataJSON()).toEqual({ command: 'delete' });
  await expect(page).toHaveURL(`${panel}/manage/server-removed`);
  const deletion = await command(request, 'tick');
  expect((await deletion.json()).operation).toBe('delete');
  await page.reload();
  await expect(page).toHaveURL(`${panel}/manage/server-removed`);
  await expect(page.getByRole('heading', { name: 'Server removal requested' })).toBeVisible();
  await expect.poll(async () => (await pool.query("SELECT count(*) FROM servers WHERE subdomain = 'renamed-world'")).rows[0].count).toBe('0');
  await page.goto('/app/account');
  await page.getByRole('button', { name: 'Sign out' }).click();
  await expect(page.getByRole('link', { name: 'Choose a sign-in method' })).toBeVisible();
  expect((await browserApi(page, '/api/v1/session')).status).toBe(401);
});

test('OAuth browser binding, state replay, ownership and invalid configuration are rejected', async ({ page, browser }) => {
  const attacker = await browser.newContext();
  const start = await attacker.request.get(`${api}/auth/google/login`, { maxRedirects: 0 });
  const oauth = new URL(start.headers().location);
  const forged = `${api}/auth/google/callback?state=${oauth.searchParams.get('state')}&code=stolen`;
  const wrongProvider = forged.replace('/auth/google/callback', '/auth/discord/callback');
  const rejection = await page.request.get(forged);
  expect(rejection.status()).toBe(400);
  expect(await rejection.text()).toContain('does not match this browser');
  await attacker.addCookies([{
    name: 'kurbis_oauth_state_discord',
    value: oauth.searchParams.get('state')!,
    domain: '127.0.0.1',
    path: '/auth/discord',
    secure: true,
    httpOnly: true,
    sameSite: 'Lax',
  }]);
  const providerRejection = await attacker.request.get(wrongProvider, {
    headers: { Cookie: `kurbis_oauth_state_discord=${oauth.searchParams.get('state')}` },
  });
  expect(providerRejection.status()).toBe(400);
  expect(await providerRejection.text()).toContain('invalid or expired OAuth state');
  await attacker.close();
  await login(page);
  await create(page, 'validation-world');
  const duplicate = await browserApi(page, '/api/v1/servers', 'POST', { subdomain: 'another-world' });
  expect(duplicate.status).toBe(409);
  for (const input of [{ subdomain: '../bad' }, { settings: { difficulty: 'Impossible' } }, { settings: { view_distance: 33 } }, { settings: { plugins: ['unrestricted'] } }]) {
    const response = await browserApi(page, await serverApiPath(page), 'PATCH', input);
    expect(response.status, `invalid settings ${JSON.stringify(input)}: ${response.text}`).toBe(422);
  }
  expect((await current(page)).subdomain).toBe('validation-world');
  const anonymous = await browser.newContext();
  expect((await anonymous.request.get(`${api}/api/v1/servers`)).status()).toBe(401);
  await anonymous.close();
  const replay = await page.request.get(forged);
  expect(replay.status()).toBe(400);
  expect((await browserApi(page, '/api/v1/session')).status).toBe(200);
});


test('Google and Discord keep provider identities separate and both establish sessions', async ({ browser }) => {
  const google = await browser.newPage();
  const discord = await browser.newPage();
  await login(google, 'Browser Player', 'Google');
  await login(discord, 'Browser Player', 'Discord');
  const identities = await pool.query(
    "SELECT provider, subject, email FROM identities WHERE subject = 'browser-user' ORDER BY provider",
  );
  expect(identities.rows).toEqual([
    { provider: 'discord', subject: 'browser-user', email: 'browser@example.test' },
    { provider: 'google', subject: 'browser-user', email: 'browser@example.test' },
  ]);
  for (const page of [google, discord]) {
    expect((await browserApi(page, '/api/v1/session')).status).toBe(200);
    const sessionCookie = (await page.context().cookies()).find(cookie => cookie.name === 'kurbis_session');
    expect(sessionCookie).toMatchObject({ httpOnly: true, secure: true, sameSite: 'Lax' });
    expect((await page.context().cookies()).some(cookie => cookie.name.startsWith('kurbis_oauth_state_'))).toBe(false);
  }

  await google.goto('/app/account');
  await expect(google.getByRole('heading', { name: 'Linked identities' })).toBeVisible();
  await expect(google.getByText('Google', { exact: true })).toBeVisible();
  await expect(google.getByText('Discord', { exact: true })).toBeVisible();
  await expect(google.getByRole('heading', { name: 'Browser sessions' })).toBeVisible();
  await expect(google.getByText('Current', { exact: true })).toHaveCount(1);
  const otherSession = google.getByRole('listitem').filter({ hasText: 'Browser session' });
  await expect(otherSession).toHaveCount(1);
  await otherSession.getByRole('button', { name: 'Revoke', exact: true }).click();
  await expect.poll(async () => (await browserApi(discord, '/api/v1/session')).status).toBe(401);
  expect((await browserApi(google, '/api/v1/session')).status).toBe(200);
  await google.close();
  await discord.close();
});

test('Minecraft fragment links redeem once through the same-origin browser page', async ({ page, browser, request }) => {
  const identity = { kind: 'java' as const, uuid: randomUUID() };
  const link = await issueMinecraftLink(request, identity);
  const redemption = page.waitForRequest(request =>
    new URL(request.url()).pathname === '/auth/minecraft/redeem' && request.method() === 'POST'
  );
  const appLoaded = page.waitForURL(/\/app\/servers$/, { waitUntil: 'load' });
  await page.goto(link);
  await redemption;
  await appLoaded;
  expect(new URL(page.url()).hash).toBe('');
  expect((await browserApi(page, '/api/v1/session')).status).toBe(200);
  await page.goto('/app/account');
  await expect(page.getByText('Minecraft Java Edition', { exact: true })).toBeVisible();

  const identityRow = await pool.query(
    "SELECT user_id, provider FROM identities WHERE provider='minecraft_java' AND subject=$1",
    [identity.uuid],
  );
  expect(identityRow.rows).toEqual([{ user_id: expect.any(String), provider: 'minecraft_java' }]);
  const sessions = await pool.query(
    'SELECT count(*) FROM sessions WHERE user_id=$1',
    [identityRow.rows[0].user_id],
  );
  expect(sessions.rows[0].count).toBe('1');

  const replay = await browser.newPage();
  await replay.goto(link);
  await expect(replay.getByText(/invalid, expired, or already used/i)).toBeVisible();
  expect((await browserApi(replay, '/api/v1/session')).status).toBe(401);
  await replay.close();
});

test('signed-in Minecraft redemption attaches to the browser account without replacing its world', async ({ page, request }) => {
  await login(page);
  await create(page, 'minecraft-owner-world');
  const identity = { kind: 'bedrock' as const, xuid: '2533274790395904' };
  const link = await issueMinecraftLink(request, identity);
  await page.goto(link);
  await expect(page).toHaveURL(/\/app\/servers$/);
  expect(new URL(page.url()).hash).toBe('');
  expect((await current(page)).subdomain).toBe('minecraft-owner-world');

  const owner = await pool.query(
    "SELECT i.user_id, s.id AS server_id, s.is_default FROM identities i JOIN servers s ON s.owner_id=i.user_id WHERE i.provider='minecraft_bedrock' AND i.subject=$1",
    [identity.xuid],
  );
  expect(owner.rows).toHaveLength(1);
  expect(owner.rows[0]).toMatchObject({ server_id: expect.any(String), is_default: true });
  await page.goto('/app/account');
  await expect(page.getByText('Minecraft Bedrock Edition', { exact: true })).toBeVisible();
});

test('unverified provider email addresses are not saved on the account', async ({ page, request }) => {
  await command(request, 'email-verification', { verified: false });
  await login(page, 'Browser Player', 'Discord');
  const identity = await pool.query(
    "SELECT email FROM identities WHERE provider = 'discord' AND subject = 'browser-user'",
  );
  expect(identity.rows).toEqual([{ email: null }]);
});


test('separate OAuth accounts cannot read or mutate each other and address conflicts are explicit', async ({ page, browser, request }) => {
  await login(page);
  await create(page, 'owned-world');
  const ownedServerId = await serverId(page);
  await command(request, 'identity', { subject: 'browser-other-user' });
  const other = await browser.newContext();
  const otherPage = await other.newPage();
  await login(otherPage, 'Other Player');
  expect(await current(otherPage)).toBeNull();
  expect((await browserApi(otherPage, `/api/v1/servers/${ownedServerId}`, 'PATCH', { subdomain: 'owned-world' })).status).toBe(404);
  expect((await browserApi(otherPage, `/api/v1/servers/${ownedServerId}/actions`, 'POST', { action: 'start' })).status).toBe(404);
  expect((await browserApi(otherPage, '/api/v1/servers', 'POST', { subdomain: 'owned-world' })).status).toBe(409);
  await create(otherPage, 'other-world');
  expect((await browserApi(otherPage, `/api/v1/servers/${ownedServerId}`, 'PATCH', { subdomain: 'owned-world' })).status).toBe(404);
  expect((await current(page)).subdomain).toBe('owned-world');
  expect((await current(otherPage)).subdomain).toBe('other-world');
  await other.close();
});

test('expired OAuth state and a replayed successful callback cannot create sessions', async ({ page, request }) => {
  await command(request, 'pause', { paused: true });
  await page.goto('/login');
  await page.getByRole('link', { name: 'Continue with Google' }).click();
  await page.waitForURL(`${fixture}/authorize?**`);
  const expiredState = new URL(page.url()).searchParams.get('state');
  await pool.query("UPDATE oauth_states SET expires_at = now() - interval '1 second'");
  const expired = await page.goto(`${api}/auth/google/callback?state=${expiredState}&code=unused`);
  expect(expired?.status()).toBe(400);
  expect(await expired?.text()).toContain('invalid or expired OAuth state');
  await command(request, 'pause', { paused: false });
  let successfulCallback = '';
  page.on('request', request => {
    if (new URL(request.url()).pathname === '/auth/google/callback') successfulCallback = request.url();
  });
  await login(page);
  const validCallback = successfulCallback;
  expect(validCallback).not.toBe('');
  const originalState = new URL(validCallback).searchParams.get('state')!;
  const before = (await pool.query('SELECT count(*) FROM sessions')).rows[0].count;
  // Replay with the original browser cookie restored so database one-time
  // consumption, rather than cookie mismatch, is the tested boundary.
  await page.context().addCookies([{ name: 'kurbis_oauth_state_google', value: originalState,
    domain: '127.0.0.1', path: '/auth/google', secure: true, httpOnly: true, sameSite: 'Lax' }]);
  const replay = await page.goto(validCallback);
  expect(replay?.status()).toBe(400);
  expect(await replay?.text()).toContain('invalid or expired OAuth state');
  expect((await pool.query('SELECT count(*) FROM sessions')).rows[0].count).toBe(before);
});


test('online console commands are deduplicated, fenced to a runtime generation, and stream output or failures', async ({ page, request }) => {
  test.setTimeout(120000);
  const id = await startWorldOnline(page, request, 'console-command-world');
  await page.goto(`/manage/${id}/console`);
  await expect(page.getByText('Runtime online')).toBeVisible();
  await expect(page.getByText('Live updates connected')).toBeVisible();

  const commandText = 'say Browser command works';
  const commandInput = page.getByRole('textbox', { name: 'Minecraft command' });
  await commandInput.fill(`/${commandText}`);
  const acceptedResponse = page.waitForResponse((response) =>
    response.url().endsWith(`/api/v1/servers/${id}/console/commands`) && response.request().method() === 'POST',
  );
  await page.getByRole('button', { name: 'Send command' }).click();
  const accepted = await acceptedResponse;
  expect(accepted.status()).toBe(202);
  const queued = await accepted.json() as {
    id: string;
    server_id: string;
    request_id: string;
    generation: number;
    attempt: number;
    command: string;
    status: string;
  };
  expect(queued).toMatchObject({
    server_id: id,
    generation: 1,
    attempt: 0,
    command: commandText,
    status: 'queued',
  });
  expect(queued.request_id).toMatch(/^[0-9a-f-]{36}$/i);
  await expect(page.getByRole('status')).toContainText('Command queued');

  const replay = await page.evaluate(async ({ requestId, command: submitted }) => {
    const response = await fetch(`/api/v1/servers/${window.location.pathname.split('/')[2]}/console/commands`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ request_id: requestId, command: submitted }),
    });
    return { status: response.status, body: await response.json() };
  }, { requestId: queued.request_id, command: commandText });
  expect(replay.status).toBe(202);
  expect(replay.body.id).toBe(queued.id);
  const conflictingReplay = await page.evaluate(async ({ requestId, command: submitted }) => {
    const response = await fetch(`/api/v1/servers/${window.location.pathname.split('/')[2]}/console/commands`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ request_id: requestId, command: submitted }),
    });
    return { status: response.status, body: await response.json() };
  }, { requestId: queued.request_id, command: 'say this is a different command' });
  expect(conflictingReplay.status).toBe(409);
  expect(conflictingReplay.body.code).toBe('console_request_id_conflict');

  const execution = await request.post(`${fixture}/fixture/console/execute`, {
    data: {
      server_id: id,
      command_id: queued.id,
      output: ['\u001b[32mBrowser command returned OK\u001b[0m'],
    },
  });
  expect(execution.status(), await execution.text()).toBe(200);
  expect(await execution.json()).toMatchObject({ id: queued.id, status: 'succeeded', attempt: 1 });
  await expect(page.getByRole('log').getByText('Browser command returned OK')).toBeVisible();
  await expect(page.getByRole('log').getByText('Browser command returned OK')).toHaveCSS('color', 'rgb(0, 170, 0)');
  const succeeded = await page.evaluate(async (commandId) => {
    const serverId = window.location.pathname.split('/')[2];
    const response = await fetch(`/api/v1/servers/${serverId}/console/commands/${commandId}`);
    return { status: response.status, body: await response.json() };
  }, queued.id);
  expect(succeeded).toMatchObject({ status: 200, body: { status: 'succeeded', attempt: 1 } });

  await commandInput.fill('say failing command');
  const failedResponsePromise = page.waitForResponse((response) =>
    response.url().endsWith(`/api/v1/servers/${id}/console/commands`) && response.request().method() === 'POST',
  );
  await page.getByRole('button', { name: 'Send command' }).click();
  const failedResponse = await failedResponsePromise;
  expect(failedResponse.status()).toBe(202);
  const failedQueued = await failedResponse.json() as { id: string; status: string };
  expect(failedQueued.status).toBe('queued');
  const failure = await request.post(`${fixture}/fixture/console/execute`, {
    data: { server_id: id, command_id: failedQueued.id, succeeded: false, error: 'fixture runtime rejected the command' },
  });
  expect(failure.status(), await failure.text()).toBe(200);
  await expect(page.getByRole('log').getByText('! /say failing command failed: fixture runtime rejected the command')).toBeVisible();
  const failedStatus = await page.evaluate(async (commandId) => {
    const serverId = window.location.pathname.split('/')[2];
    const response = await fetch(`/api/v1/servers/${serverId}/console/commands/${commandId}`);
    return { status: response.status, body: await response.json() };
  }, failedQueued.id);
  expect(failedStatus).toMatchObject({ status: 200, body: { status: 'failed', attempt: 1, error: 'fixture runtime rejected the command' } });

  for (const width of [1440, 820, 390]) {
    await page.setViewportSize({ width, height: width === 390 ? 844 : 1000 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    const audit = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa']).analyze();
    expect(audit.violations, `console command route at ${width}px`).toEqual([]);
    if (width === 1440 || width === 390) {
      await capture(page, `console-command-${width}.png`);
    }
  }
});

test('Players page manages independent whitelist and protected operator rosters without exposing IDs', async ({ page, request }) => {
  test.setTimeout(120000);
  const id = await startWorldOnline(page, request, 'managed-player-world');
  const owner = await pool.query('SELECT owner_id FROM servers WHERE id = $1', [id]);
  const ownerProfileId = randomUUID();
  const playerId = '8667ba71-b85a-4004-af54-457a9734eed7';
  await pool.query(
    `INSERT INTO identities(id, user_id, provider, subject, display_name, minecraft_player_id)
     VALUES($1, $2, 'minecraft_java', $3, 'BrowserOwner', $4)`,
    [randomUUID(), owner.rows[0].owner_id, ownerProfileId, ownerProfileId],
  );
  await pool.query(
    `INSERT INTO server_players(server_id, player_id, name, edition)
     VALUES($1, $2, 'BrowserHelper', 'java')
     ON CONFLICT(server_id, player_id) DO UPDATE SET name = EXCLUDED.name, edition = EXCLUDED.edition,
       connected_at = now(), last_seen_at = now()`,
    [id, playerId],
  );
  await page.goto(`/manage/${id}/players`);
  await expect(page.getByText('BrowserHelper', { exact: true })).toBeVisible();
  await expect(page.getByText('BrowserOwner', { exact: true })).toBeVisible();
  await expect(page.getByText('Locked', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Remove BrowserOwner as operator' })).toHaveCount(0);
  await expect(page.getByPlaceholder('00000000-0000-0000-0000-000000000000')).toHaveCount(0);

  await page.getByRole('button', { name: 'Allow', exact: true }).click();
  await expect(page.getByText('Allowed', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Grant OP', exact: true }).click();
  await expect(page.getByText('Operator', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Remove BrowserHelper as operator' })).toBeVisible();

  const whitelistStored = await pool.query(
    'SELECT player_id, name FROM whitelist_entries WHERE server_id = $1', [id],
  );
  const operatorStored = await pool.query(
    'SELECT player_id, name FROM operator_entries WHERE server_id = $1', [id],
  );
  expect(whitelistStored.rows).toEqual([{ player_id: playerId, name: 'BrowserHelper' }]);
  expect(operatorStored.rows).toEqual([{ player_id: playerId, name: 'BrowserHelper' }]);

  await page.reload();
  await expect(page.getByText('BrowserOwner', { exact: true })).toBeVisible();
  await expect(page.getByText('BrowserHelper', { exact: true })).toHaveCount(3);
  await page.getByRole('button', { name: 'Remove BrowserHelper from whitelist' }).click();
  await expect(page.getByText('Whitelist is empty')).toBeVisible();
  await page.getByRole('button', { name: 'Remove BrowserHelper as operator' }).click();
  await expect(page.getByText('No extra operators')).toBeVisible();
  await expect.poll(async () => {
    const [whitelist, operators] = await Promise.all([
      pool.query('SELECT count(*) FROM whitelist_entries WHERE server_id = $1', [id]),
      pool.query('SELECT count(*) FROM operator_entries WHERE server_id = $1', [id]),
    ]);
    return [whitelist.rows[0].count, operators.rows[0].count];
  }).toEqual(['0', '0']);

  for (const width of [1440, 820, 390]) {
    await page.setViewportSize({ width, height: width === 390 ? 844 : 1000 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    const audit = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa']).analyze();
    expect(audit.violations, `Players route at ${width}px`).toEqual([]);
    if (width === 1440 || width === 390) {
      await capture(page, `managed-players-${width}.png`);
    }
  }
});
