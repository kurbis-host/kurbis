# Kurbis Web Frontend Progress

Last reviewed: 2026-10-02

## Status: Route and API Contract Overhaul Complete and Verified

The frontend/API contract in [PLAN.md](../PLAN.md) is implemented. The web app has one server-panel route family under `/manage/{serverId}`. `/app/servers` remains the account-level list and selector; there are no `/app/servers/{serverId}` pages, redirects, or compatibility aliases.

The selector merges account-accessible servers with all persistent scoped grants in the browser. Each scoped grant is independently revocable and remains valid until revoked or its server is no longer available. The 15-minute expiry belongs only to the one-use login link. The no-access destination is `/manage/access-unavailable`.

The public API contract is generated from the Rust Aide/Schemars router into `contracts/openapi.json`, then exposed through generated TypeScript operation/type modules. Browser and server data clients use the generated operations. The canonical lifecycle endpoint is `POST /api/v1/servers/{server_id}/commands`; the removed `/actions` route has no compatibility alias.

Scoped server access is represented by persistent opaque browser grants backed by independent per-server rows. `/api/v1/servers` returns the deduplicated union of account and grant access. Server-session APIs enumerate and independently revoke grants. A valid account session can coexist with scoped access to a different owner's server.

Browser grants may reset or delete only their bound server. Raw plugin capabilities and worker actors do not gain those destructive permissions. The Kurbis command tree and inventory menu share the same action registry.

## Final Verification

- `pnpm --dir web run api:check` passed; generated OpenAPI/TypeScript drift is clean.
- TypeScript checking passed.
- Frontend unit tests passed **29/29** across 13 files.
- Production Next.js build passed and emitted `/app/servers`, `/manage/[serverId]`, and `/manage/access-unavailable`, with no `/app/servers/[serverId]` route family.
- Production Playwright passed **19/19** on a fresh local database.
- Browser journeys cover account and scoped server discovery, multiple persistent grants, independent revocation, account-cookie fallback, the shared `/manage/{serverId}` route family, scoped reset/delete authorization, restart behavior, and post-delete navigation to `/manage/server-removed` before access invalidation can race the router.
- Plugin crate passed **7/7** tests, Clippy, and `wasm32-wasip2` checking; plugin command/menu behavior uses the shared action registry.
- Repository Docker/Linux runtime acceptance completed with `passed: true`, including real Java protocol flows, automated Bedrock/TLS signaling, scoped-grant lifecycle operations, cross-worker restore, snapshot outage recovery, hard 5 GiB storage enforcement, a real five-minute continuous-empty idle window, and hard 2 GiB no-swap memory enforcement with an observed cgroup OOM kill.

Physical Bedrock client login/UI interaction was intentionally not performed per user instruction. Automated Bedrock/TLS coverage is complete.
