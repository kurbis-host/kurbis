# Browser lifecycle verification

Use a separate disposable PostgreSQL database whose name ends with `_e2e`. The fixture refuses other database names and binds only loopback. Playwright owns startup and shutdown of the real Axum API, Next.js panel, and fixture services.

```sh
createdb melon_hoster_e2e
cd web
npm ci
npx playwright install chromium
TEST_DATABASE_URL=postgres://localhost/melon_hoster_e2e npm run test:e2e
```

An existing Chrome binary may be used via `CHROMIUM_PATH=/absolute/path/to/chrome`.

The browser tests exercise both Google and Discord authorization-code flows, provider-specific profile mapping, isolated provider identities, browser-bound one-time state, Secure session cookies, creation, curated settings, draft preservation during polling, address conflicts, owner isolation, durable capacity queue/cancellation, lifecycle controls, logs, final snapshot publication, reservation release, deletion, logout, and mobile width. Database assertions verify provider identities, final snapshot count, start operation count, and released reservations.

The external OAuth provider and worker runtime are controlled by `examples/browser_fixture.rs`. The worker fixture uses production lifecycle claim/completion and snapshot code but substitutes readiness and clean-shutdown boundaries for Docker/Pumpkin. Delete completion is also fixture-controlled. These tests verify the panel/API/control-plane boundary, not real gameplay, host-control transfers, hard quotas, runtime crash recovery, or production OAuth provider availability.

Screenshots, failure traces, and the HTML report are written under `output/playwright/` at the repository root.
