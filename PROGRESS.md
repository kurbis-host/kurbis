# Kurbis Implementation Progress

Last reviewed: 2026-10-02

## Status: Architecture Overhaul Complete and Locally Verified

The approved v1 architecture and product scope in [PLAN.md](PLAN.md) are implemented and verified locally. The public lifecycle surface is the canonical `POST /api/v1/servers/{server_id}/commands` endpoint; legacy `/actions` aliases and duplicate worker lifecycle routes are removed. Public authentication resolves through typed actor context, the OpenAPI contract is generated from the Rust router, and the TypeScript client is checked for drift.

The backend uses one fresh canonical schema in `migrations/0001_fresh_schema.sql`. Gateway routing reads through the shared repository boundary instead of control-plane API internals. Workers operate through the control-plane API and assignment-scoped object endpoints and do not receive database, bucket, or operator credentials. Worker API leases are fenced, and runtime cleanup removes the stopped runtime before the control plane releases capacity so published ports can be reused safely.

The accepted account behavior is implemented: `/app/servers` is the account-level list/selector and `/manage/{serverId}` is the only server-panel route family. Multiple scoped browser grants persist independently until revoked. The selector exposes the union of account-accessible and grant-accessible servers. A scoped browser grant can reset or delete only its bound server; raw worker/plugin capabilities cannot perform those destructive actions. Plugin commands and menus resolve through one shared action registry.

## Verification Evidence

All verification below ran against the completed implementation on 2026-10-02.

- Fresh Rust/Postgres suite passed in full: 77 library tests; 10 API v1; 1 fresh-schema; 6 gateway-routing; 5 identity; 2 identity-preservation; 27 lifecycle; 5 server-access API; 13 snapshot-persistence; 2 worker-bundle; 13 worker-control API; 1 worker-maintenance; 2 worker-object isolation; 1 world-export; plus doc tests.
- `cargo clippy --locked --all-targets -- -D warnings` passed.
- `cargo fmt --all -- --check` passed.
- Worker-focused Rust library tests passed **35/35** after the runtime teardown changes.
- Plugin crate passed **7/7** tests, Clippy, and `wasm32-wasip2` checking.
- Python runtime-verifier unit suite passed **20/20**.
- Web API drift check (`pnpm --dir web run api:check`) passed.
- Web TypeScript check passed.
- Web unit suite passed **29/29** across 13 files.
- Production Next.js build passed with `/app/servers` and `/manage/[serverId]` routes and no `/app/servers/[serverId]` route family.
- Production Playwright suite passed **19/19**, including multi-grant persistence, independent revocation, account-cookie fallback, scoped reset/delete, and deterministic post-delete navigation.
- `git diff --check` passed before the final progress-document update.

## Docker/Linux Runtime Acceptance

The fresh local runtime harness completed with `passed: true` in `output/runtime/melon-local-verification-183e66584a1c48cf98983d5a2740d2ef/result.json`.

It verified the generated API contract, worker credential isolation, live Pumpkin startup, the waiting runtime, the real gateway listener, trusted Bedrock TLS signaling, owner restart with generation fencing, operator plugin GUI behavior, world export, queued capacity and Java waiting login, clean capacity release, scoped stop/start/reset/delete, periodic snapshots, snapshot-upload outage recovery, worker heartbeat during object-store failure, expired-lease fencing, runtime-loss reconciliation, cross-worker snapshot restoration, clean shutdown/final snapshot, native Java wake/transfer, and player/world persistence across workers.

The hard runtime limits also passed:

- Per-server storage quota returned `ENOSPC` after **5,170,528,256 bytes** were written, proving the hard 5 GiB filesystem boundary.
- Idle shutdown required a fresh continuous-empty interval after rejoin and completed after **308.45 seconds** continuously empty; the original empty interval reached **499.61 seconds** and was correctly reset by the rejoin.
- Docker cgroup memory was limited to **2,147,483,648 bytes** with **0 bytes swap**. A 3 GiB allocation-pressure probe exited **137**, and `memory.events` changed from `oom=0, oom_kill=0` to `oom=1, oom_kill=1`.
- Scoped stop followed immediately by scoped start succeeded after another grant was revoked, confirming the stopped Docker runtime is removed before the published port lease is reused.

## Scope Note

All implementation and acceptance work was local. Physical Bedrock client login/UI interaction was intentionally not performed per user instruction; automated Bedrock/TLS signaling coverage passed in the runtime harness.
