# Kurbis Complete Web Frontend

## Summary

Rebuild Kurbis as a complete v1 product: a focused public site plus a production-grade authenticated hosting panel. Keep the Kurbis name and existing hosting truth, but replace the current visual system and single-page client implementation entirely.

The new visual direction is an **original voxel-world interface**: unmistakably Minecraft-adjacent through block geometry, materials, pixel-aware details, custom voxel illustrations, and game-like state feedback, while using original Kurbis assets and modern accessible UX rather than copying Minecraft UI or textures.

The target audience is mixed: effortless for casual friend groups, with progressively disclosed controls for experienced server owners.

The v1 product includes lifecycle management, connection details, live state, a runtime-online console command surface, safely formatted logs, configuration, player presence, independent whitelist and managed-operator rosters, backup history/manual backups/restore, operational activity, resource usage, account UX, onboarding, and failure recovery. Keep payments, plugin/mod uploads, unrestricted file management, arbitrary binaries, and administrative tooling beyond these scoped player controls out of scope.

## Product, UX, and Design

- Build a focused / product site around the actual Kurbis mechanism: a voxel world visibly sleeping, waking when a player connects, restoring from snapshots, and releasing infrastructure when empty. Include free-tier limits, Java + Bedrock support, the five-minute idle flow, snapshot safety, FAQ, and a prominent sign-in/create CTA.
- Replace the current login screen with the public product entry point. Authenticated visitors go directly into /app/servers; unauthenticated CTAs enter OAuth.
- /app/setup claims nothing: it lets a signed-in user try a name, says whether it is free, and tells them to join that address in Minecraft. The first join creates the server; the account link from the lobby chat links it to the signed-in account, and the page then opens the server panel.
- Keep `/app/servers` as the account-level server list and selector. Use the `/app` account shell for setup and account navigation; all per-server surfaces live below `/manage/{serverId}`: overview, console, backups, players, and settings. Remove every `/app/servers/{serverId}` page without a redirect or alias.
- Keep `/manage/{serverId}` as the only server-panel entry point. It accepts an account session authorized for that server or active scoped server-session proof. Retain multiple active scoped server-login grants; the selector shows the union of account-accessible servers and all servers covered by active scoped grants. Account cookies do not mask valid scoped access to another owner's server. Scoped access never authenticates account routes.
- Support the in-game server-only entry point separately from account login. `/auth/server` reads the one-use token from the URL fragment, removes it from the address bar, and redeems it with a same-origin POST. The browser holds one opaque grant-collection cookie; each covered server grant remains active until that grant is explicitly revoked or its server is removed or suspended. The one-use login link still expires after 15 minutes.
- Make every lifecycle state a first-class designed state: sleeping, queued, restoring, starting, online, stopping, snapshotting, failed, deleted. Show useful stage-specific language and actions rather than a generic spinner.
- Failure states expose a stable user-facing error explanation, retry/recovery action where applicable, and expandable technical detail.
- Preserve drafts across live server updates by keeping server state and form state separate.
- Visual system:
    - Original voxel/block geometry and custom Kurbis iconography.
    - Stone/earth/grass/pumpkin/sky-inspired material palette with high-contrast functional states.
    - Pixel-influenced display typography for identity and small labels; highly legible non-pixel body/UI typography.
    - Custom voxel/isometric illustrations rather than stock imagery.
    - Landing hero uses one authored sleep ↔ wake voxel-world transition; the panel uses restrained state transitions and tactile press feedback.
    - Respect prefers-reduced-motion; motion is never the only state indicator.
    - WCAG AA contrast, complete keyboard operation, visible focus, correct ARIA/live-region behavior, minimum touch targets, and layouts verified from 390px mobile through large desktop.
- Use one authored visual theme for v1; do not spend scope on a theme switcher.

## Frontend and Application Architecture

- Replace the monolithic web/app/page.tsx, global selector-heavy CSS, manual polling, and manually maintained API typings. Do not retain compatibility wrappers for the old frontend.
- Keep Next.js 16 App Router and structure it around (marketing) and (app) route groups with shared layouts, route-level loading/error states, small Server Components, and narrow Client Component boundaries.
- Make browser traffic same-origin:
    - Route /api/\* and the control-plane /auth/\* endpoints through the panel transport proxy to a server-only CONTROL_PLANE_INTERNAL_URL. The proxy must stream responses directly so SSE is never buffered by the application router.
    - Remove NEXT_PUBLIC_CONTROL_PLANE_URL and the browser’s direct cross-origin dependency on the control plane.
    - Point OAuth callbacks at the public Kurbis origin so session cookies belong to the frontend origin.
    - Remove public-panel CORS coupling from the control plane once all browser traffic uses the same-origin path.
- Fetch authenticated shell/initial page data in Server Components with the incoming session cookie, eliminating the current auth/loading flash. Hydrate only live/interactable areas.
- Add TanStack Query for normalized server-side state/mutation caching and a single event integration layer. Forms use React Hook Form + Zod schemas; accessibility-sensitive primitives such as dialogs, menus, tooltips, and switches use unstyled accessible primitives rather than hand-rolled focus management.
- Use Tailwind CSS 4 utilities with a small global design-token layer instead of component-spanning global selectors. Keep shared visual patterns in reusable primitives and split the app shell, domain components, feature components, hooks, API code, and schemas into focused modules.
- Replace browser polling with authenticated **Server-Sent Events**. REST remains authoritative for mutations; SSE updates the client cache for lifecycle, player presence, logs, operations, and snapshots.
- SSE must support reconnect/catch-up with event IDs. Back it with PostgreSQL notification/event sources rather than process-local broadcasts so multiple control-plane replicas remain consistent.
- Keep logs safe and readable in the console: render ANSI/Minecraft formatting, filter visible text, and export plain text. Allow authorized browser users to queue a Minecraft command only while the runtime is online; submission feedback confirms queue acceptance while command output continues through the normal live log stream.

## Backend/API Changes

Replace the current ad-hoc panel endpoints with a versioned typed API and remove old endpoint paths without compatibility aliases.

Public contracts:

- GET /api/v1/session
- GET /api/v1/servers; GET | PATCH /api/v1/servers/{server_id}
- POST /api/v1/servers/{server_id}/commands for typed start | stop | restart | snapshot | reset | restore | delete intents
- GET /api/v1/servers/{server_id}/events for SSE
- POST /api/v1/servers/{server_id}/console/commands with a validated `{ request_id, command }` body to idempotently queue an authorized console command only while the runtime is online; return `202` and do not wait for execution
- GET /api/v1/servers/{server_id}/logs?after_id=&limit=
- GET /api/v1/servers/{server_id}/activity
- GET /api/v1/servers/{server_id}/snapshots
- GET /api/v1/servers/{server_id}/players
- GET | POST | DELETE /api/v1/servers/{server_id}/whitelist
- GET | POST | DELETE /api/v1/servers/{server_id}/operators
- GET | POST /api/v1/servers/{server_id}/exports; GET /api/v1/servers/{server_id}/exports/{export_id}/download; DELETE /api/v1/servers/{server_id}/exports/{export_id}
- GET /api/v1/account
- PUT /api/v1/account/worlds/{server_id}/default
- DELETE /api/v1/account/sessions/{session_id}
- POST /auth/minecraft/redeem (same-origin only; GET requests never mutate account state)
- GET /api/v1/server/sessions; DELETE /api/v1/server/sessions/{session_id} (list and independently revoke this browser's scoped grants)
- POST /auth/server/redeem (same-origin only; one-use 15-minute token exchange that adds a persistent grant scoped to one server)

Create explicit serializable DTOs for Session, Server, ServerSettings, LifecycleStatus, Operation, Snapshot, PlayerPresence, WhitelistEntry, LogEntry, and discriminated ServerEvent variants. Stop returning raw settings JSON and unstructured lifecycle errors.

Keep Axum/Tower and use typed actor authentication middleware/extractors; handlers dispatch into canonical domain operations rather than owning lifecycle rules. The single commands path serves account and assigned-worker lifecycle intents, with actor-specific authorization and placement fences. Remove `/actions`, separate reset/delete/restore lifecycle paths, and duplicate worker stop/snapshot paths without aliases.

Use Rust route declarations and DTOs as the source of truth for the public JSON API. Produce OpenAPI with Aide and Schemars, generate TypeScript types with `openapi-typescript` and the fetch client with `openapi-fetch`, and use `garde`-derived request validation. Generated drift checks cover every public JSON endpoint. SSE and object transfer/download routes remain specialized streaming endpoints with explicit authorization.

Every per-server route and event stream checks that the caller may access the path's server ID. An account session must be active and own that server; persistent scoped server grants authorize only their covered servers and never authenticate account routes. Multiple grants remain available to the selector and can be revoked independently. The account-accessible server list is the deduplicated union of owned servers and servers covered by this browser's scoped grants. Worker capabilities are service credentials, not browser sessions, and are valid only for the worker's current placement. Workers access the control plane only through its API and receive no database credentials, SQL pool, or bucket credentials.

Extend the server response with timestamps, current operation/stage, latest snapshot metadata, player count, storage limits/usage, and structured error information.

For backup restore, never move current_snapshot_generation backwards. Restoring an older complete snapshot must create a **new authoritative generation derived from that snapshot**, preserving monotonic generations and existing recovery invariants.

Expose retained completed snapshots only. Manual backup creation uses the existing snapshot operation machinery and is available while the runtime is online; restore requires the server to be sleeping.

Make whitelist entries and managed operator entries first-class, independent persisted server data. Resolve names server-side, keep player IDs internal, lock owner operators, fix managed operators at permission level 4, and preserve unrelated in-game operators. Apply both rosters when Pumpkin starts and synchronize mutations into the live runtime when online. World reset retains managed operators; full reset clears them. Extend host control as necessary so online player presence and whitelist/operator changes have an acknowledged runtime boundary instead of frontend-only state.

Persist enough player presence/activity data for the control plane to serve the panel without the browser reaching worker-local sockets.

Emit or trigger PostgreSQL notifications for server state, logs, operations, snapshots, and player-presence changes so SSE works correctly across redundant control-plane processes.

## Test Plan and Acceptance Criteria

- Add Rust/Postgres integration coverage for every new API, ownership isolation, suspended accounts, lifecycle command validation, durable console command request deduplication, generation fencing, no automatic replay, manual snapshots, snapshot-history visibility, monotonic rollback restore, whitelist/operator persistence and runtime synchronization, player identity resolution, player presence, pagination, and SSE authorization/reconnect/catch-up behavior.
- Verify per-server ownership isolation for every resource, command, export, and event stream. Confirm `/app/servers/{serverId}` is absent with no redirect, and `/manage/{serverId}` covers account-authorized servers and the union of active scoped sessions.
- Verify exactly one `/api/v1/servers/{server_id}/commands` route handles shared lifecycle intents, including owner/worker parity, and no old action/reset/restore/delete or worker stop/snapshot route remains as an alias.
- Verify OpenAPI generation and TypeScript drift checks cover every public JSON route; test `garde`-derived validation against request DTO constraints.
- Bootstrap the canonical schema from an empty database and verify workers have no DB credentials, SQL pool, or bucket credentials. Exercise worker assignment checks through the API and streamed object authorization.
- Add frontend component tests for lifecycle rendering, optimistic mutations, settings draft isolation, validation, reconnect handling, console command online-state gating/dispatch feedback, ANSI and Minecraft formatting, readable filtering/exports, destructive confirmations, backup restrictions, whitelist interactions, and error states.
- Expand Playwright into complete user journeys: public landing → OAuth → onboarding → start/queue/restore/online → run and complete a console command with live output → manage whitelist and operators → settings → backup → restore → restart/stop → deletion → logout.
- Preserve browser security assertions for OAuth state binding, explicit provider linking, secure renewable cookies, session revocation, replay rejection, user isolation, and address conflicts.
- Cover Java and Bedrock Minecraft login redemption for signed-in and signed-out users, token expiry/reuse, cross-origin POST rejection, and rejection of unauthenticated or forged game identities.
- Cover server-only login redemption, fragment removal before the same-origin POST, one-use and 15-minute login-link expiry, persistent grant access after advancing the browser clock, independent per-server revocation and selector removal, account-cookie plus scoped-grant commands, account-route denial, cross-server denial, and deleted-server recovery states.
- Verify worker identity comes from its unique API credential, not request fields; test cross-worker capability denial, placement reassignment, credential provisioning/rotation, and denial of placement or generation changes through worker API requests.
- Verify identity merges retain every server and its backup/history data, carry suspension forward, and preserve the browser account's default world or choose the oldest surviving world.
- Verify account owners can switch the default among their preserved worlds and cannot select a world owned by another account.
- Verify that a BrowserGrant can reset or delete only its bound server, while a placement-scoped plugin capability and worker actor cannot issue reset/delete commands.
- Add Playwright checks at desktop, tablet, and 390px mobile widths; verify navigation, dialogs, console, long server/log content, empty/error states, and absence of horizontal overflow.
- Run automated accessibility checks with Axe plus keyboard-only Playwright flows and reduced-motion coverage.
- Maintain visual-regression captures for the landing page, overview, console, backups, players, settings, setup, and representative queued/online/failed states at desktop and mobile.
- Final verification requires TypeScript checking, Next production build, frontend tests, Playwright E2E, Rust test suite, Clippy with warnings denied, API integration tests, and one bounded visual QA pass on desktop/mobile followed by a single correction/confirmation pass.

## Assumptions

- New accounts receive one server; merged identities retain all existing worlds with one selected default. Kurbis remains one region, supports Google/Discord OAuth and verified Java/Bedrock identity login, 2 GiB RAM, 5 GiB persistent storage, five-minute idle flow, and no explicit player-count limit.
- English-only is sufficient for v1, but UI strings should be centralized enough that localization is not structurally blocked.
- Existing implementation work is the behavioral baseline; preserve v1 product behavior while replacing obsolete frontend/API contracts and schema compatibility paths.
- No backwards-compatibility layer is required. Legacy frontend components, polling logic, old API routes, cross-origin browser configuration, and obsolete CSS are removed once their replacements are covered by the new tests.
