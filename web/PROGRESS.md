# Kurbis Web Frontend Progress

This file tracks implementation evidence against `web/PLAN.md`. An item is only marked complete after the implementation exists in the current worktree and the relevant verification has passed.

## Backend foundations

- [x] Initial durable event/player/whitelist schema drafted in migrations.
- [x] `OperationKind::Restore` added to the Rust model.
- [x] `whitelist.json` excluded from world snapshots as orchestration-owned runtime state.
- [x] Split/validate the restore enum migration and durable event migration.
- [x] Manual online snapshot lifecycle request with duplicate/concurrency protection.
- [x] Monotonic restore: restoring an older retained snapshot publishes a new authoritative generation.
- [x] Persist live player presence in Postgres and clear stale presence on runtime loss/stop.
- [x] Persist desired whitelist entries and synchronize them to Pumpkin with an acknowledged revision boundary.
- [x] Apply whitelist state before runtime startup and reconcile desired revisions while online.

## Versioned API and live events

- [x] Typed `/api/v1/session` and `/api/v1/server` contracts.
- [x] Typed lifecycle action endpoint.
- [x] Cursor-based logs, activity, snapshots, restore, players, and whitelist endpoints.
- [x] Authenticated Postgres-backed SSE with durable event IDs and catch-up after reconnect.
- [x] Remove legacy unversioned panel API routes and public-panel CORS coupling.
- [x] Same-origin OAuth callback/session behavior.

## Frontend

- [x] Replace the monolithic page with App Router route groups and a persistent authenticated shell.
- [x] Server Components for initial authenticated reads with narrow client boundaries.
- [x] Same-origin Next rewrites using server-only `CONTROL_PLANE_INTERNAL_URL`.
- [x] TanStack Query cache plus one SSE integration layer.
- [x] React Hook Form + Zod forms and accessible unstyled primitives.
- [x] Public marketing site with original Kurbis voxel-world art direction.
- [x] Setup/onboarding flow.
- [x] Overview, console, backups, players, settings, and account routes.
- [x] Designed lifecycle/failure/empty states.
- [x] Responsive layouts from 390px mobile through desktop, keyboard focus, reduced motion, AA contrast.
- [x] Remove browser polling, `NEXT_PUBLIC_CONTROL_PLANE_URL`, legacy page/CSS/API client code.

## Verification

- [x] `cargo check --all-targets` passes with restore handling wired through the worker.
- [x] Rust/Postgres integration coverage for new API/lifecycle/persistence semantics.
- [ ] Frontend component coverage for state rendering, mutations, forms, validation, reconnects, confirmations, whitelist, backups, and errors.
- [x] Complete Playwright journey and OAuth/security assertions.
- [ ] Desktop/tablet/390px responsive and overflow checks.
- [ ] Axe, keyboard-only, and reduced-motion coverage.
- [ ] Visual-regression captures for required routes/states.
- [x] TypeScript check.
- [x] Next production build.
- [x] Frontend tests.
- [x] Playwright E2E.
- [x] Full Rust test suite.
- [x] `cargo clippy --all-targets --all-features -- -D warnings`.
- [x] API integration tests.
- [ ] Impeccable detector on final changed UI targets.
- [ ] Bounded desktop/mobile visual QA correction pass followed by confirmation pass.

## Current implementation note

The repository began this implementation with substantial unrelated pre-existing changes. Final commit selection must include only files changed for this web plan and preserve unrelated work.

## Verification evidence (local)

- The implementation is now in App Router `(marketing)` and `(app)` route groups with server-side initial reads, TanStack Query, React Hook Form/Zod forms, Tailwind CSS 4 utilities, same-origin rewrites, and a PostgreSQL-backed SSE client bridge.
- `npm run typecheck`, `npm run build`, and the five Vitest files with all 9 tests pass after the Tailwind migration.
- `TEST_DATABASE_URL=postgres://kurbis:kurbis@127.0.0.1:55432/kurbis_test cargo test --all-targets` passes: 30 library tests, 4 gateway routing tests, 23 lifecycle tests, 13 snapshot/persistence tests, and 2 worker-lease tests.
- `cargo clippy --all-targets --all-features -- -D warnings` and `cargo fmt --all -- --check` pass.
- The primary Playwright journey passes against the disposable `kurbis_e2e` database: OAuth → setup → settings draft persistence → capacity queue → restore/start → console → restart/stop → responsive mobile/desktop capture → destructive deletion → logout.
- Browser QA exposed and fixed an overview hydration mismatch caused by calling `Date.now()` during SSR, plus stale/racy E2E assumptions around stop and delete completion.
- The versioned API now has black-box Postgres coverage for authentication, server creation/read/update, lifecycle validation, log cursor pagination, activity, retained snapshot filtering/restore admission, player presence, revisioned whitelist mutations, ownership isolation, and real SSE catch-up through `Last-Event-ID`.
- Manual snapshots reject duplicate live requests, and restoring retained generation 1 while generation 2 is current is verified to complete only after a new authoritative generation 3 is published with restore provenance.
- Player presence and whitelist synchronization are extracted into a focused worker module. A fake-runtime/Postgres test verifies stale-player cleanup, pre-start `whitelist.json`, acknowledged applied revisions, failed live synchronization without false acknowledgement, and successful retry.
- Server-deletion testing exposed two cascade-trigger failures. Migrations `0007` and `0008` make durable event and whitelist-revision triggers intentionally no-op after the parent server is gone, so deletion can cascade safely.
- Whitelist API/client alignment now carries desired/applied revisions and uses real player UUIDs instead of browser-generated random identifiers; currently-online players can be allowed directly from persisted presence. Frontend interaction, accessibility, visual-regression, and complete browser-journey coverage remain open until their artifacts pass.

### Current native-only verification

Docker Desktop is stopped at the user’s request. Native PostgreSQL and native Rust/Next processes passed all 77 Rust tests, Clippy, 17 frontend unit tests, TypeScript and the production build. All four browser OAuth/lifecycle journeys passed (`output/web-e2e-current.log`). Axe/keyboard/tablet and comprehensive route visual verification remain open; component implementation alone is not proof of those gates.

### Accessibility and route verification (2026-10-01)

- Axe cleared the public landing page and both Google/Discord sign-in links after correcting the CTA section’s foreground/background contrast.
- Overview QA found an unnamed storage meter and a timestamp color just below 4.5:1 contrast; the meter now exposes progressbar values and the timestamp uses a darker accessible text color.
- Axe passed on the landing, sign-in, setup, all six authenticated routes, and queued/online/failed states. The same run passed desktop/tablet/390px overflow, keyboard dialog focus trapping and Escape restoration, and reduced-motion behavior.
- Captured 36 route/state views across desktop, tablet, and mobile under `output/playwright/run-18580/screenshots/`. Visual review found no layout or overflow correction required; the screenshots are from the local Next development server and include its dev badge.
- The complete Playwright OAuth/lifecycle journey suite is now running against the same isolated database and port-scoped artifact directory.
