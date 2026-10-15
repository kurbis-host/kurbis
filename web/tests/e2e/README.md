# Browser lifecycle verification

Use a separate disposable PostgreSQL database whose name ends with `_e2e`. The fixture refuses other database names and binds only loopback. Playwright owns startup and shutdown of the real Axum API, Next.js panel, and fixture services.

```sh
createdb melon_hoster_e2e
cd web
pnpm install --frozen-lockfile
pnpm exec playwright install chromium
TEST_DATABASE_URL=postgres://localhost/melon_hoster_e2e pnpm run test:e2e
```

An existing Chrome binary may be used via `CHROMIUM_PATH=/absolute/path/to/chrome`.

The browser tests exercise Google and Discord authorization-code flows, provider-specific profile mapping, verified-email linking, isolated provider identities, browser-bound one-time state, Secure renewable sessions and revocation, and Minecraft fragment-token redemption for signed-out and signed-in browsers. The Minecraft journey checks that a web sign-in claims no name, that joining a free address claims it and that the lobby's account link joins both accounts while the claim page opens the new panel by itself, same-origin POST redemption, URL-fragment clearing, Java/Bedrock identity linking, account merging, preservation and selection of merged worlds, and session retention. The panel journey covers claiming by joining, curated settings, draft preservation during polling, address conflicts, owner isolation, durable capacity queue/cancellation, lifecycle controls, logs, final snapshot publication, reservation release, deletion, logout, and mobile width. It also verifies that worker and database state changes reach the open panel over SSE without reloading. Database assertions verify identities, merge ownership, snapshots, start operations, and released reservations.

The external OAuth provider and worker runtime are controlled by `examples/browser_fixture.rs`. Servers are claimed the way players claim them: the tests call the gateway's authenticated admission endpoint as a verified Java UUID/Bedrock XUID joining an address, then open the account link it returns; this verifies browser redemption and merge behavior, not live Microsoft/Xbox credential validation. The worker fixture uses production lifecycle claim/completion and snapshot code but substitutes readiness and clean-shutdown boundaries for Docker/Pumpkin. Delete completion is also fixture-controlled. These tests verify the panel/API/control-plane boundary, not real gameplay, host-control transfers, hard quotas, runtime crash recovery, or live OAuth/Minecraft account availability.

Screenshots, failure traces, and the HTML report are written under `output/playwright/` at the repository root.

The landing page is a WebGL film where it can be, and its own HTML where it cannot. `landing.spec.ts` plays the film (scroll to every stop, the name flow, the keyboard, reduced motion, an axe audit) on machines with a GPU (in software rendering the film is too slow to test, and its tests skip), and checks on every machine that without WebGL the page is its HTML and fetches nothing of the film. The panel journey audits and compares the landing page in that HTML form, which is the same everywhere.

Playwright also compares committed visual baselines for the landing page (as its HTML), setup, owner panel routes, queued/online/failed overview states, and server-only overview at 1440px desktop and 390px mobile widths. Baselines live beside their specs in `*.spec.ts-snapshots/`; refresh them intentionally with `TEST_DATABASE_URL=postgres://localhost/melon_hoster_e2e pnpm run test:e2e:production --update-snapshots` and review the resulting image changes.
