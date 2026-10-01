# Kurbis Web Frontend Progress

Last verified: 2026-10-01

The panel uses account sessions and server-specific URLs under `/app/servers/{serverId}`. Account pages list linked identities, sessions, and owned servers; server data, mutations, exports, and SSE are scoped by server ID and checked against ownership on every request. Guest server-login pages remain deferred.

## Verification

- TypeScript typecheck passed.
- Frontend unit tests passed **18/18** across 9 files.
- Clean Next.js production build passed and emitted only owner-panel routes plus `/auth/minecraft`; stale generated types were cleared before typecheck.
- Fresh-database production Playwright passed **12/12** journeys, covering Minecraft redemption, OAuth/session handling, server selection and isolation, resets, responsive behavior, accessibility, and security headers.

The browser fixtures use controlled OAuth and Minecraft identities. They do not verify live Microsoft/Xbox or Google/Discord accounts. Docker/Linux quota checks and real Bedrock handoff remain outside local verification; see the root [`PROGRESS.md`](../PROGRESS.md).
