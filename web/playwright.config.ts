import { defineConfig } from '@playwright/test';
import path from 'node:path';

const database = process.env.TEST_DATABASE_URL;
if (!database || !new URL(database).pathname.endsWith('_e2e')) {
  throw new Error('TEST_DATABASE_URL must name a disposable database ending in _e2e');
}
const root = path.resolve(__dirname, '..');
const portBase = Number(process.env.E2E_PORT_BASE ?? '18080');
if (!Number.isInteger(portBase) || portBase < 1024 || portBase > 65533) throw new Error('Invalid E2E_PORT_BASE');
const apiUrl = `http://127.0.0.1:${portBase}`;
const fixtureUrl = `http://127.0.0.1:${portBase + 1}`;
const panelUrl = `http://127.0.0.1:${portBase + 2}`;
const artifactRoot = path.join(root, `output/playwright/run-${portBase}`);
const panelCommand = process.env.E2E_NEXT_MODE === 'production'
  ? `npm run start -- --hostname 127.0.0.1 --port ${portBase + 2}`
  : `npm run dev -- --hostname 127.0.0.1 --port ${portBase + 2}`;
process.env.BROWSER_FIXTURE_URL = fixtureUrl;
process.env.BROWSER_PANEL_URL = panelUrl;
process.env.BROWSER_API_URL = apiUrl;
process.env.BROWSER_OUTPUT_DIR = artifactRoot;
const env = {
  ...process.env,
  NEXT_DIST_DIR: `.next-e2e-${portBase}`,
  TEST_DATABASE_URL: database,
  DATABASE_URL: database,
  TEST_ARTIFACT_ROOT: path.join(root, `output/playwright/fixture-${portBase}`),
  BROWSER_OUTPUT_DIR: artifactRoot,
  CONTROL_PLANE_LISTEN: `127.0.0.1:${portBase}`,
  BROWSER_FIXTURE_LISTEN: `127.0.0.1:${portBase + 1}`,
  PUBLIC_BASE_URL: panelUrl,
  PANEL_ORIGIN: panelUrl,
  ROOT_DOMAIN: 'mc.example.test',
  INTERNAL_TOKEN: 'browser-internal-token',
  GOOGLE_OAUTH_CLIENT_ID: 'browser-google-client',
  GOOGLE_OAUTH_CLIENT_SECRET: 'browser-google-secret',
  GOOGLE_OAUTH_AUTHORIZE_URL: `${fixtureUrl}/authorize`,
  GOOGLE_OAUTH_TOKEN_URL: `${fixtureUrl}/token`,
  GOOGLE_OAUTH_USERINFO_URL: `${fixtureUrl}/userinfo`,
  DISCORD_OAUTH_CLIENT_ID: 'browser-discord-client',
  DISCORD_OAUTH_CLIENT_SECRET: 'browser-discord-secret',
  DISCORD_OAUTH_AUTHORIZE_URL: `${fixtureUrl}/authorize`,
  DISCORD_OAUTH_TOKEN_URL: `${fixtureUrl}/token`,
  DISCORD_OAUTH_USERINFO_URL: `${fixtureUrl}/userinfo`,
  CONTROL_PLANE_INTERNAL_URL: apiUrl,
};

export default defineConfig({
  testDir: './tests/e2e',
  timeout: 60000,
  expect: { timeout: 10000 },
  workers: 1,
  fullyParallel: false,
  outputDir: path.join(artifactRoot, 'results'),
  reporter: [['list'], ['html', { outputFolder: path.join(artifactRoot, 'report'), open: 'never' }]],
  use: {
    baseURL: env.PANEL_ORIGIN,
    headless: true,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    launchOptions: process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : {},
  },
  webServer: [
    { command: 'cargo run --example browser_fixture', cwd: root, env, url: `${fixtureUrl}/healthz`, timeout: 120000 },
    { command: 'cargo run --bin control-plane', cwd: root, env, url: `${apiUrl}/healthz`, timeout: 120000 },
    { command: panelCommand, env, url: env.PANEL_ORIGIN, timeout: 120000 },
  ],
});
