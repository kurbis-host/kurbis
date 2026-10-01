# Kurbis Complete Web Frontend

## Summary

Rebuild Kurbis as a complete v1 product: a focused public site plus a production-grade authenticated hosting panel. Keep the Kurbis name and existing hosting truth, but replace the current visual system and single-page client implementation entirely.

The new visual direction is an **original voxel-world interface**: unmistakably Minecraft-adjacent through block geometry, materials, pixel-aware details, custom voxel illustrations, and game-like state feedback, while using original Kurbis assets and modern accessible UX rather than copying Minecraft UI or textures.

The target audience is mixed: effortless for casual friend groups, with progressively disclosed controls for experienced server owners.

The v1 product includes lifecycle management, connection details, live state, logs, configuration, player/whitelist management, backup history/manual backups/restore, operational activity, resource usage, account UX, onboarding, and failure recovery. Keep payments, plugins/mod uploads, unrestricted file management, arbitrary binaries, and admin tooling out of scope.

## Product, UX, and Design

- Build a focused / product site around the actual Kurbis mechanism: a voxel world visibly sleeping, waking when a player connects, restoring from snapshots, and releasing infrastructure when empty. Include free-tier limits, Java + Bedrock support, the five-minute idle flow, snapshot safety, FAQ, and a prominent sign-in/create CTA.
- Replace the current login screen with the public product entry point. Authenticated visitors go directly into /app; unauthenticated CTAs enter OAuth.
- Create /app/setup for first-run server creation: choose the address, select a sensible Survival/Creative preset and core settings, explain 2 GiB RAM / 5 GiB storage / wake-on-connect, create the server asleep, then guide the user to copy the address or start it.
- Use a persistent /app shell with responsive navigation and these surfaces:
    - /app — world overview: lifecycle state, start/stop/restart, Java/Bedrock addresses, connected players, storage, latest backup, current issue, and recent activity.
    - /app/console — cursor-based live read-only logs with pause/resume, autoscroll, search/filter, copy, and download.
    - /app/backups — retained snapshot timeline, reasons/sizes/timestamps, manual backup creation, and safe restore.
    - /app/players — current online players plus persistent whitelist management.
    - /app/settings — address and curated Pumpkin settings with explicit dirty state, validation, sleeping-state restrictions, and clear “stop to edit” affordances.
    - /app/account — linked Google, Discord, Java, and Bedrock identities; explicit provider linking; browser-session listing/revocation; preserved-world/default-world selection; account state, service limits, and logout.
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
    - Add Next rewrites for /api/\* and /auth/\* to a server-only CONTROL_PLANE_INTERNAL_URL.
    - Remove NEXT_PUBLIC_CONTROL_PLANE_URL and the browser’s direct cross-origin dependency on the control plane.
    - Point OAuth callbacks at the public Kurbis origin so session cookies belong to the frontend origin.
    - Remove public-panel CORS coupling from the control plane once all browser traffic uses the same-origin path.
- Fetch authenticated shell/initial page data in Server Components with the incoming session cookie, eliminating the current auth/loading flash. Hydrate only live/interactable areas.
- Add TanStack Query for normalized server-side state/mutation caching and a single event integration layer. Forms use React Hook Form + Zod schemas; accessibility-sensitive primitives such as dialogs, menus, tooltips, and switches use unstyled accessible primitives rather than hand-rolled focus management.
- Use Tailwind CSS 4 utilities with a small global design-token layer instead of component-spanning global selectors. Keep shared visual patterns in reusable primitives and split the app shell, domain components, feature components, hooks, API code, and schemas into focused modules.
- Replace browser polling with authenticated **Server-Sent Events**. REST remains authoritative for mutations; SSE updates the client cache for lifecycle, player presence, logs, operations, and snapshots.
- SSE must support reconnect/catch-up with event IDs. Back it with PostgreSQL notification/event sources rather than process-local broadcasts so multiple control-plane replicas remain consistent.
- Keep the console read-only in v1. Do not add arbitrary Minecraft command execution as part of this frontend project.

## Backend/API Changes

Replace the current ad-hoc panel endpoints with a versioned typed API and delete the old endpoint set when the frontend migrates.

Public contracts:

- GET /api/v1/session
- GET | POST | PATCH | DELETE /api/v1/server
- POST /api/v1/server/actions with start | stop | restart | snapshot
- GET /api/v1/server/events for SSE
- GET /api/v1/server/logs?after_id=&limit=
- GET /api/v1/server/activity
- GET /api/v1/server/snapshots
- POST /api/v1/server/snapshots/{generation}/restore
- GET /api/v1/server/players
- GET | POST | DELETE /api/v1/server/whitelist
- GET /api/v1/account
- PUT /api/v1/account/worlds/{server_id}/default
- DELETE /api/v1/account/sessions/{session_id}
- POST /auth/minecraft/redeem (same-origin only; GET requests never mutate account state)

Create explicit serializable DTOs for Session, Server, ServerSettings, LifecycleStatus, Operation, Snapshot, PlayerPresence, WhitelistEntry, LogEntry, and discriminated ServerEvent variants. Stop returning raw settings JSON and unstructured lifecycle errors.

Extend the server response with timestamps, current operation/stage, latest snapshot metadata, player count, storage limits/usage, and structured error information.

For backup restore, never move current_snapshot_generation backwards. Restoring an older complete snapshot must create a **new authoritative generation derived from that snapshot**, preserving monotonic generations and existing recovery invariants.

Expose retained completed snapshots only. Manual backup creation uses the existing snapshot operation machinery and is available while the runtime is online; restore requires the server to be sleeping.

Make whitelist entries first-class persisted server data. Apply them when Pumpkin starts and synchronize mutations into the live runtime when online. Extend host control as necessary so online player presence and whitelist changes have an acknowledged runtime boundary instead of frontend-only state.

Persist enough player presence/activity data for the control plane to serve the panel without the browser reaching worker-local sockets.

Emit or trigger PostgreSQL notifications for server state, logs, operations, snapshots, and player-presence changes so SSE works correctly across redundant control-plane processes.

## Test Plan and Acceptance Criteria

- Add Rust/Postgres integration coverage for every new API, ownership isolation, suspended accounts, lifecycle action validation, manual snapshots, snapshot-history visibility, monotonic rollback restore, whitelist persistence/runtime synchronization, player presence, pagination, and SSE authorization/reconnect/catch-up behavior.
- Add frontend component tests for lifecycle rendering, optimistic mutations, settings draft isolation, validation, reconnect handling, destructive confirmations, backup restrictions, whitelist interactions, and error states.
- Expand Playwright into complete user journeys: public landing → OAuth → onboarding → start/queue/restore/online → live logs → players/whitelist → settings → backup → restore → restart/stop → deletion → logout.
- Preserve browser security assertions for OAuth state binding, explicit provider linking, secure renewable cookies, session revocation, replay rejection, user isolation, and address conflicts.
- Cover Java and Bedrock Minecraft login redemption for signed-in and signed-out users, token expiry/reuse, cross-origin POST rejection, and rejection of unauthenticated or forged game identities.
- Verify identity merges retain every server and its backup/history data, carry suspension forward, and preserve the browser account's default world or choose the oldest surviving world.
- Verify account owners can switch the default among their preserved worlds and cannot select a world owned by another account.
- Add Playwright checks at desktop, tablet, and 390px mobile widths; verify navigation, dialogs, console, long server/log content, empty/error states, and absence of horizontal overflow.
- Run automated accessibility checks with Axe plus keyboard-only Playwright flows and reduced-motion coverage.
- Maintain visual-regression captures for the landing page, overview, console, backups, players, settings, setup, and representative queued/online/failed states at desktop and mobile.
- Final verification requires TypeScript checking, Next production build, frontend tests, Playwright E2E, Rust test suite, Clippy with warnings denied, API integration tests, and one bounded visual QA pass on desktop/mobile followed by a single correction/confirmation pass.

## Assumptions

- New accounts receive one server; merged identities retain all existing worlds with one selected default. Kurbis remains one region, supports Google/Discord OAuth and verified Java/Bedrock identity login, 2 GiB RAM, 5 GiB persistent storage, five-minute idle flow, and no explicit player-count limit.
- English-only is sufficient for v1, but UI strings should be centralized enough that localization is not structurally blocked.
- The existing working tree is the implementation baseline; current uncommitted backend work must be preserved while replacing obsolete frontend/API code.
- No backwards-compatibility layer is required. Legacy frontend components, polling logic, old API routes, cross-origin browser configuration, and obsolete CSS are removed once their replacements are covered by the new tests.
