import { randomUUID } from 'node:crypto';
import { expect, type APIRequestContext, type Page } from '@playwright/test';

const api = process.env.BROWSER_API_URL ?? 'http://127.0.0.1:18080';
const panel = process.env.BROWSER_PANEL_URL ?? 'http://127.0.0.1:18082';

export type MinecraftIdentity = { kind: 'java'; uuid: string } | { kind: 'bedrock'; xuid: string };

export type Admission = {
  created: boolean;
  owner: boolean;
  account_link: string | null;
  private_link: string | null;
  target: { server_id: string; subdomain: string };
};

export function minecraftPlayerId(identity: MinecraftIdentity) {
  if (identity.kind === 'java') return identity.uuid;
  return BigInt(identity.xuid).toString(16).padStart(32, '0').replace(
    /^(........)(....)(....)(....)(............)$/,
    '$1-$2-$3-$4-$5',
  );
}

/**
 * What the lobby asks the control plane when a verified player joins `name`. Joining
 * a free address is the only way a server name is claimed.
 */
export async function joinServer(
  request: APIRequestContext,
  name: string,
  identity: MinecraftIdentity = { kind: 'java', uuid: randomUUID() },
  displayName = 'Minecraft Player',
): Promise<Admission> {
  const response = await request.post(`${api}/internal/admission`, {
    headers: { authorization: 'Bearer browser-internal-token' },
    data: {
      hostname: `${name}.mc.example.test`,
      identity,
      minecraft_player_id: minecraftPlayerId(identity),
      display_name: displayName,
    },
  });
  expect(response.status(), await response.text()).toBe(200);
  return response.json();
}

/** The one-use account link the lobby shows an owner: a same-origin fragment link. */
export function accountLink(admission: Admission) {
  expect(admission.owner).toBe(true);
  expect(admission.account_link).toEqual(expect.any(String));
  const url = new URL(admission.account_link!);
  expect(url.origin).toBe(panel);
  expect(url.pathname).toBe('/auth/minecraft');
  expect(url.search).toBe('');
  return url;
}

/**
 * Open an account link in a signed-in browser, which asks before linking the player.
 * `accountName`, when given, is the account the page must name; otherwise any is fine.
 */
export async function linkToSignedInAccount(page: Page, link: URL, accountName?: string) {
  await page.goto(`${link.pathname}${link.hash}`);
  const name = accountName ? escapeRegExp(accountName) : '.+';
  await expect(page.getByRole('status')).toContainText(new RegExp(`signed in to \\(${name}\\)`));
  await page.getByRole('button', { name: new RegExp(`^Link to ${name}$`) }).click();
}

function escapeRegExp(text: string) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * Claim `name` for the account signed in to `page`: a new player joins the address,
 * which creates the server, and the browser opens the link the lobby handed them,
 * which links both. `playerName` is the joining player's in-game name. Returns the new
 * server's id.
 */
export async function claimByJoining(page: Page, name: string, playerName = 'Minecraft Player') {
  const admission = await joinServer(page.request, name, { kind: 'java', uuid: randomUUID() }, playerName);
  expect(admission.created).toBe(true);
  await linkToSignedInAccount(page, accountLink(admission));
  await expect(page).toHaveURL(new RegExp(`/manage/${admission.target.server_id}$`));
  return admission.target.server_id;
}
