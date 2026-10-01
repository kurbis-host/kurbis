# Kurbis Implementation Progress

Last reviewed: 2026-10-01

## Current status

The local control plane, PostgreSQL lifecycle model, workers, Java/Bedrock gateway and waiting flow, snapshot storage, native development runtime, Next.js product panel, and account identity/linking flows are implemented. No deployment has been performed.

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

- `TEST_DATABASE_URL=postgres://localhost/kurbis_auth_plan_20261001_4c91 cargo test --all-targets -- --test-threads=1` passed **93 tests**, including populated-data migration, verified-email linking, concurrent account merges, suspension preservation, session revocation, and multiple-world/default selection.
- `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `git diff --check`, and Python fixture compilation passed.
- `npm run typecheck` and `npm test` passed; the frontend suite has **14 tests across 9 files**.
- The complete Playwright suite passed **10/10** in development mode and **10/10** against a production Next.js build. Coverage includes signed-out Minecraft login, signed-in merge, one-use token replay, OAuth identity/session behavior, Axe, keyboard use, reduced motion, and responsive layouts.
- The pinned, patched Pumpkin host-control harness passed with real Java and Bedrock protocol clients. Offline clients exposed no verified identity; reconnects received new connection IDs; authenticated RPC, save, graceful shutdown, and socket cleanup passed.
- The native gateway restart integration passed with real Java and Bedrock clients before and after control-plane/gateway restarts. Both editions joined waiting before backend transfer; valid unused hostnames reached waiting and were rejected; queued wake state and active placement survived restart. Evidence: `output/runtime/auth-gateway-restart-final-3/result.json`.
- The Impeccable detector reported no findings for the changed account, login, and app-shell surfaces.
- The native worker runtime's recovery, stale-PID fencing, and log-cursor unit tests passed **3/3**. The full native Java/Bedrock worker lifecycle passed **19 checks** against disposable local Postgres, including waiting admission, online/queued transfers, snapshot outage/retry, worker recovery/fencing, cross-worker restore, clean shutdown, Java/Bedrock wake, and automatic sleep after a verified **307-second idle interval following rejoin**. Full evidence: `output/runtime/native-plan-final-20261001/result.json`.

## Remaining release checks

- Live Google/Discord sign-in and Microsoft/Xbox-backed Java/Bedrock identity validation require real provider accounts. Local OAuth, Minecraft redemption, and runtime identity tests use controlled fixtures.
- Hard memory and disk quotas under Linux container load were not tested because Docker is unavailable in this environment.
- Production deployment was not requested or performed.
