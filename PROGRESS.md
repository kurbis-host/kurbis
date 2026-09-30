# Kurbis Implementation Progress

Last reviewed: 2026-10-01

## Current status

The local control plane, PostgreSQL lifecycle model, workers, Java/Bedrock gateway and waiting flow, snapshot storage, native development runtime, and Next.js product panel are implemented. I am completing current-source verification while the account identity/linking surface is being integrated. No deployment has been performed.

Docker Desktop is stopped as requested. Development and the runtime evidence in this file use native macOS processes. The real-container memory-cgroup and 5 GiB filesystem-quota tests remain unrun because those require Docker/Linux; native directory checks are not a substitute for those hard limits.

## Implemented

- Rust/Axum control plane with durable users and provider identities, sessions, servers, operations, placements, workers, capacity reservations, FIFO wake queue, player presence, whitelist revisions, activity, logs, snapshots, and event cursors.
- Versioned owner-scoped API with lifecycle validation, typed DTOs, durable PostgreSQL-backed SSE, replay/catch-up, snapshot restore with monotonic generations, and account/session endpoints.
- Native Rust gateway for Java TCP and Bedrock RakNet, hostname validation, waiting Pumpkin admission, wake/queue messages, and Minecraft-native backend transfer. Durable route resolution and invalidation remain in Postgres.
- Rust worker with lease/fencing and recovery, Docker runtime support, and native host-process runtime for local development. Native mode does not impose cgroups or an ext4 quota.
- Content-addressed incremental snapshots with checksum/manifest validation, complete-generation publication, restore pins, bounded retention, recovery retries, and garbage collection.
- Next.js App Router public site and authenticated panel with setup, overview, console, backups, players, settings, account, same-origin API/OAuth proxying, typed schemas, TanStack Query, and SSE updates.
- Original Kurbis voxel visual system, Tailwind CSS 4 utilities with a small global design-token stylesheet, responsive states, reduced-motion support, keyboard-accessible dialogs, and Axe coverage.

## Verification evidence

- `TEST_DATABASE_URL=postgres://user@127.0.0.1:55433/melon_hoster_final_test cargo test --all-targets` passed **79 tests** across library, API, identity, gateway routing, lifecycle, snapshots, and worker leases.
- Current integrated source passes `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `git diff --check`.
- `npm run test:frontend` passed **14 tests across 9 files**; `npm audit --audit-level=high` found **0 vulnerabilities**.
- The production Next.js browser suite passed **7/7 journeys**. It covered landing/onboarding, OAuth/session security and replay rejection, provider identity linking, account isolation, full lifecycle/queue/snapshot/deletion, Axe, keyboard dialogs, reduced motion, and desktop/tablet/390px overflow.
- On fresh `melon_hoster_identity_parallel_test`, `cargo test --test identity_preservation` passed **2/2**. It verifies concurrent case/whitespace-normalized verified-email links converge on one account, a suspended source account suspends the merged account, the browser account's default world stays selected, and snapshot, operation, log, event, player, and whitelist history counts remain intact for both worlds.
- The same fresh database passed `cargo test --test identity_migration`. It applies migrations 1–9 to a temporary isolated schema with populated users, worlds, sessions, and OAuth state, then verifies migration 10 preserves provider identities and normalized legacy email metadata without marking it verified, deletes expired sessions, and adds default-world support while removing legacy provider columns.
- On fresh `melon_hoster_auth_api_test`, all **10 control-plane API unit tests** passed, including signed-in/out Minecraft redemption, origin/replay/expiry/forged-identity rejection, complete world merge, session listing/revocation, suspension enforcement, and cookie renewal.
- On fresh `melon_hoster_minecraft_auth_e2e`, the new Playwright Minecraft journey passed **1/1** against the local dev panel. It verifies fragment removal before same-origin POST, secure persistent signed-out login, world setup, and linking a Bedrock identity into the same signed-in Java account without replacing its session.
- A local native gateway-restart scenario passed with real Java and Bedrock clients. It restarted the control plane and gateway, then verified unchanged active placement/lease data and exactly one queued wake with the same request timestamp. Evidence: `output/runtime/gateway-restart-final-current/result.json`.
- The previous both-edition native lifecycle run passed queue waiting, Bedrock 1.26.40 waiting-world login, Java/Bedrock wake and transfer, cross-worker world/inventory recovery, snapshot-store outage recovery, runtime restart/fencing, and five-minute idle reset. Current identity-schema fixture updates are being exercised in a fresh run before this evidence is treated as final.
- Production browser screenshots and traces are stored under `output/playwright/run-18680/`. A bounded visual review found no layout or overflow defects in the inspected landing, setup, queued, and backups captures.

## Work still in progress

- Finish the account identity/linking UI integration already underway, then rerun typecheck, production build, component tests, production browser journeys, Axe, responsive checks, and the final screenshot review against the settled files.
- Complete the remaining auth verification against fresh databases after the account migration settles, including all browser redemption security cases and the full native Java/Bedrock lifecycle.
- Add owner-scoped switching among merged worlds so every retained world remains reachable from the panel; then verify ownership isolation and account UI behavior.
- Rerun the full both-edition native lifecycle harness using current identity-schema fixtures and current native Pumpkin/control-plane sources.
- Run the Impeccable detector once over the final changed web UI targets and resolve any actionable findings.
- Perform a final requirement-by-requirement review and commit only this task’s completed work.

Runtime documentation now distinguishes the native local-development/integration path from Docker-only Linux hard-quota verification and records the actual scope of disposable protocol identities.

## Explicitly unverified

- Hard memory and disk quotas under real Linux container load, including memory OOM and `ENOSPC`, because Docker is stopped at the user's direction.
- Production deployment. All implementation and verification work remains local on this Mac.
