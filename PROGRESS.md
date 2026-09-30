# Kurbis Progress

Last reviewed: 2026-09-30

## Current state

The control plane, worker/runtime orchestration, gateways, and web panel are implemented. The project is not complete: remaining work centers on current-source runtime verification, full Bedrock gateway transfer, final frontend QA, and production-shaped Docker quota/lifecycle evidence. `web/PLAN.md` remains the frontend acceptance checklist.

## Implemented

- Rust/Axum control plane with Postgres-backed accounts, servers, placements, workers, queues, lifecycle operations, player presence, whitelist revisions, activity, and snapshots.
- Versioned server API, OAuth sessions, authenticated event streaming, and the Next.js App Router panel with setup, server management, console, backups, players, settings, and account views.
- Worker leases and lifecycle fencing, capacity-aware queueing, operation recovery, native host-process runtime for local development, and Docker runtime support.
- Content-addressed snapshots with integrity checks, restore validation, retention pins, and garbage collection.
- Java gateway routing and waiting-server transfer. Bedrock UDP/RakNet forwarding and pinned Pumpkin protocol integration are present; complete gateway transfer is not verified yet.

## Latest verification evidence

- The current Rust test run passed **77 tests** with `TEST_DATABASE_URL`; see `output/local-rust-final-v2.log`. Clippy with warnings denied passed; see `output/local-clippy-final-v3.log`.
- The current native Java lifecycle run passed queue admission and transfer, snapshot outage recovery, worker restart and runtime-loss fencing, cross-worker player/world restoration, shutdown snapshotting, sleep/wake, and a five-minute idle interval reset by rejoin. Evidence: `output/runtime/native-current-java/result.json`.
- A direct Bedrock runtime session passed join, StartGame, chunks, inventory/entity traffic, disconnect, save, and graceful shutdown. This does not verify Bedrock gateway transfer: `output/runtime/native-bedrock-direct-v2/result.json`.
- Earlier frontend checks passed TypeScript, production build, unit tests, and four Playwright scenarios. The latest UUID fixture correction has not yet been rerun; final interaction, accessibility, responsive, and visual QA remain open.

## Remaining work

- Rebuild the patched Pumpkin runtime after the Bedrock `max_players = 0` fix and run the both-edition gateway lifecycle, including Bedrock login, hostname routing, wake, and native transfer.
- Complete frontend verification after the latest changes: rerun unit tests and the full Playwright journey, then finish keyboard/accessibility, responsive, and visual review in `web/PLAN.md`.
- Verify Docker runtime recovery and hard resource limits on the current source, including filesystem quota/ENOSPC and memory cgroup behavior. Native directory mode does not establish hard quotas.
- Complete requirement-by-requirement review. No current-source production deployment is verified.

All build and runtime evidence above is from local runs. Historical remote sessions and superseded test totals have been removed. `web/PLAN.md` contains the frontend acceptance checklist.
