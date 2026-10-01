# Kurbis Web Frontend Progress

Last verification reported: 2026-10-01

The account panel uses account sessions and server-specific URLs under `/app/servers/{serverId}`. Account pages list linked identities, sessions, and owned servers. The in-game `/auth/server` flow redeems a fragment token through a same-origin POST and opens `/manage/{serverId}` with a distinct one-hour server-only session. That session is limited to its server and cannot access account routes or another server. Guest server-login pages are implemented.

## Verification

- TypeScript typecheck passed.
- Frontend unit tests passed **18/18** across 9 files.
- Clean Next.js production build passed and emitted the owner panel, `/auth/minecraft`, `/auth/server`, and `/manage/{serverId}` routes.
- Fresh-database production Playwright passed **13/13** journeys, covering server-scoped link redemption, account and cross-server denial, OAuth/session boundaries, resets, owner isolation, responsive behavior, accessibility, and security headers.

These results are the previously reported run; this documentation update did not rerun them. Browser fixtures use controlled OAuth and Minecraft identities, so live Microsoft/Xbox or Google/Discord accounts are not verified. Visual-regression baselines and comparison assertions are present for the landing page, setup, panel routes, and queued/online/failed states, but no new comparison result is claimed here. Clippy with warnings denied is not recorded as complete. Real Bedrock handoff and Linux/Docker hard-quota checks remain unverified; live Bedrock phone testing was canceled. The root plan also requires worker API, PostgreSQL, and object-store isolation; those checks must pass before that work is reported complete. See the root [`PROGRESS.md`](../PROGRESS.md) for current runtime limits.
