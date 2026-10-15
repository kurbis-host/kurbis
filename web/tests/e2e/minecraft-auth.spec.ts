import { randomUUID } from 'node:crypto';
import { expect, test, type Page } from '@playwright/test';
import { Pool } from 'pg';
import { accountLink, joinServer, linkToSignedInAccount, minecraftPlayerId } from './minecraft';

const fixture = process.env.BROWSER_FIXTURE_URL ?? 'http://127.0.0.1:18081';
const panel = process.env.BROWSER_PANEL_URL ?? 'http://127.0.0.1:18082';
const pool = new Pool({ connectionString: process.env.TEST_DATABASE_URL });

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
test.afterAll(async () => { await pool.end(); });

test('the first join claims its name and its link signs in, then links later joins to the same account', async ({ page, request }) => {
  test.setTimeout(60000);
  // A Java player claims a name by joining it; the lobby hands them their account link.
  const java = await joinServer(
    request,
    'minecraft-auth-world',
    { kind: 'java', uuid: randomUUID() },
    'Java Browser Player',
  );
  expect(java.created).toBe(true);
  const javaLink = accountLink(java);
  await recordRedemptionRequest(page);
  await page.goto(`${javaLink.pathname}${javaLink.hash}`);
  await expect(page).toHaveURL(new RegExp(`/manage/${java.target.server_id}$`));
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

  const originalSession = await page.evaluate(async () => {
    const response = await fetch('/api/v1/session');
    return response.json();
  });
  const bedrockXuid = '2533274790395904';
  const bedrockProfile = minecraftPlayerId({ kind: 'bedrock', xuid: bedrockXuid });
  const sourceUser = randomUUID();
  const sourceWorld = 'merged-bedrock-world';
  await pool.query(
    'INSERT INTO users(id, display_name, account_status) VALUES($1, $2, $3)',
    [sourceUser, 'Bedrock Source Account', 'active'],
  );
  await pool.query(
    "INSERT INTO identities(id, user_id, provider, subject, minecraft_player_id, display_name)\n     VALUES($1, $2, 'minecraft_bedrock', $3, $4, $5)",
    [randomUUID(), sourceUser, bedrockXuid, bedrockProfile, 'Bedrock Browser Player'],
  );
  await pool.query(
    'INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)',
    [randomUUID(), sourceUser, sourceWorld],
  );
  // The Bedrock player already owns a world on their own account. Joining it hands
  // them their account link; opening it here joins that account to this one.
  const bedrock = await joinServer(
    request,
    sourceWorld,
    { kind: 'bedrock', xuid: bedrockXuid },
    'Bedrock Browser Player',
  );
  expect(bedrock.created).toBe(false);
  await linkToSignedInAccount(page, accountLink(bedrock), 'Java Browser Player');
  await expect(page).toHaveURL(/\/manage\/[0-9a-f-]+$/);
  await page.goto('/app/account');
  await expect(page.getByRole('heading', { name: 'Linked identities' })).toBeVisible();
  await expect(page.getByText('Minecraft Java Edition', { exact: true })).toBeVisible();
  await expect(page.getByText('Minecraft Bedrock Edition', { exact: true })).toBeVisible();
  await expect(page.getByRole('main').getByText(sourceWorld, { exact: true })).toBeVisible();
  const makeDefault = page.getByRole('button', { name: 'Make default', exact: true });
  await expect(makeDefault).toHaveCount(1);
  await makeDefault.click();
  await expect(page.getByRole('status')).toContainText(sourceWorld + ' is now the default world.');
  const selectedWorld = await page.evaluate(async () => {
    const response = await fetch('/api/v1/servers');
    const servers = await response.json();
    return servers.find((server: { is_default: boolean }) => server.is_default);
  });
  expect(selectedWorld.subdomain).toBe(sourceWorld);
  const linkedSession = await page.evaluate(async () => {
    const response = await fetch('/api/v1/session');
    return response.json();
  });
  expect(linkedSession.user.id).toBe(originalSession.user.id);
  expect(linkedSession.user.display_name).toBe('Java Browser Player');
  expect((await page.context().cookies()).find((item) => item.name === 'kurbis_session')?.value)
    .toBe(cookie?.value);
  const sourceStillExists = await pool.query('SELECT 1 FROM users WHERE id = $1', [sourceUser]);
  expect(sourceStillExists.rowCount).toBe(0);
});

test('a web sign-in claims nothing: joining the name claims it and the link joins both accounts', async ({ page, context }) => {
  await page.goto('/login');
  await page.getByRole('link', { name: 'Continue with Google' }).click();
  await expect(page).toHaveURL(/\/app\/servers$/);
  await page.getByRole('link', { name: 'Claim a server' }).first().click();
  await expect(page).toHaveURL(/\/app\/setup$/);
  await page.getByRole('textbox', { name: 'Server name' }).fill('joined-claim-world');
  await expect(page.getByText('joined-claim-world.mc.example.test is free. Join it in Minecraft to claim it.')).toBeVisible();
  const claimedBySignIn = await pool.query("SELECT count(*) FROM servers WHERE subdomain = 'joined-claim-world'");
  expect(claimedBySignIn.rows[0].count).toBe('0');

  // The join creates the server; the link from chat, opened in this browser, links it.
  const join = await joinServer(page.request, 'joined-claim-world');
  expect(join.created).toBe(true);
  const chatLink = await context.newPage();
  await linkToSignedInAccount(chatLink, accountLink(join), 'Browser Player');
  await expect(chatLink).toHaveURL(new RegExp(`/manage/${join.target.server_id}$`));
  // The claim page that was waiting opens the panel by itself.
  await expect(page).toHaveURL(new RegExp(`/manage/${join.target.server_id}$`), { timeout: 15000 });
  const owner = await pool.query(
    "SELECT i.provider FROM servers s JOIN identities i ON i.user_id = s.owner_id WHERE s.id = $1 ORDER BY i.provider",
    [join.target.server_id],
  );
  expect(owner.rows.map((row) => row.provider)).toEqual(['google', 'minecraft_java']);
});
