import { randomUUID } from 'node:crypto';
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';

const api = process.env.BROWSER_API_URL ?? 'http://127.0.0.1:18080';
const fixture = process.env.BROWSER_FIXTURE_URL ?? 'http://127.0.0.1:18081';
const panel = process.env.BROWSER_PANEL_URL ?? 'http://127.0.0.1:18082';

async function mintLoginLink(
  request: APIRequestContext,
  identity: { kind: 'java'; uuid: string } | { kind: 'bedrock'; xuid: string },
  displayName: string,
) {
  const response = await request.post(`${api}/internal/minecraft-login-links`, {
    headers: { authorization: 'Bearer browser-internal-token' },
    data: { identity, display_name: displayName },
  });
  expect(response.status(), await response.text()).toBe(200);
  const { url } = await response.json() as { url: string };
  const loginUrl = new URL(url);
  expect(loginUrl.origin).toBe(panel);
  expect(loginUrl.pathname).toBe('/auth/minecraft');
  expect(loginUrl.search).toBe('');
  return loginUrl;
}

async function recordRedemptionRequest(page: Page) {
  await page.addInitScript(() => {
    const originalFetch = window.fetch;
    window.fetch = (input, init) => {
      const requestUrl = input instanceof Request ? input.url : String(input);
      const url = new URL(requestUrl, window.location.href);
      if (url.pathname === '/auth/minecraft/redeem') {
        sessionStorage.setItem('minecraft-redemption-request', JSON.stringify({
          fragmentAtRequest: window.location.hash,
          origin: url.origin,
          method: init?.method ?? (input instanceof Request ? input.method : 'GET'),
        }));
      }
      return originalFetch(input, init);
    };
  });
}

test.beforeEach(async ({ request }) => {
  const reset = await request.post(`${fixture}/fixture/reset`);
  expect(reset.ok(), await reset.text()).toBeTruthy();
});

test('Minecraft browser links clear the fragment, redeem same-origin, and attach to signed-in accounts', async ({ page, request }) => {
  test.setTimeout(60000);
  const javaLink = await mintLoginLink(
    request,
    { kind: 'java', uuid: randomUUID() },
    'Java Browser Player',
  );
  await recordRedemptionRequest(page);
  await page.goto(`${javaLink.pathname}${javaLink.hash}`);
  await expect(page).toHaveURL(/\/app\/setup$/);
  await expect.poll(async () => page.evaluate(async () => {
    const response = await fetch('/api/v1/session');
    return response.ok ? (await response.json()).user.display_name : null;
  })).toBe('Java Browser Player');

  const cookie = (await page.context().cookies()).find((item) => item.name === 'kurbis_session');
  expect(cookie).toMatchObject({ httpOnly: true, secure: true, sameSite: 'Lax' });
  const initialRequest = await page.evaluate(() => JSON.parse(
    sessionStorage.getItem('minecraft-redemption-request') ?? 'null',
  ));
  expect(initialRequest).toEqual({ fragmentAtRequest: '', origin: panel, method: 'POST' });

  await page.getByPlaceholder('friends-world').fill('minecraft-auth-world');
  await page.getByRole('button', { name: /Create sleeping world/ }).click();
  await expect(page).toHaveURL(/\/app$/);

  const originalSession = await page.evaluate(async () => {
    const response = await fetch('/api/v1/session');
    return response.json();
  });
  const bedrockLink = await mintLoginLink(
    request,
    { kind: 'bedrock', xuid: '2533274790395904' },
    'Bedrock Browser Player',
  );
  await page.goto(`${bedrockLink.pathname}${bedrockLink.hash}`);
  await expect(page).toHaveURL(/\/app$/);
  await page.goto('/app/account');
  await expect(page.getByRole('heading', { name: 'Linked identities' })).toBeVisible();
  await expect(page.getByText('Minecraft Java Edition', { exact: true })).toBeVisible();
  await expect(page.getByText('Minecraft Bedrock Edition', { exact: true })).toBeVisible();
  const linkedSession = await page.evaluate(async () => {
    const response = await fetch('/api/v1/session');
    return response.json();
  });
  expect(linkedSession.user.id).toBe(originalSession.user.id);
  expect(linkedSession.user.display_name).toBe('Java Browser Player');
  expect((await page.context().cookies()).find((item) => item.name === 'kurbis_session')?.value)
    .toBe(cookie?.value);
});
