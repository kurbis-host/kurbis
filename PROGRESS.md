# Kurbis Implementation Progress

Last reviewed: 2026-10-01

## Status

Plan 1 authentication and account linking and Plan 2 server management, durable resets, and world export are implemented in the Rust/Postgres control plane, gateway, worker, and Next.js panel.

The built-in Pumpkin management plugin is implemented. OPs can open /kurbis for server status, players, logs, activity, settings, whitelist, snapshots, and lifecycle controls. /kurbis login creates a one-use link into the existing panel. The plugin calls the existing server API; a placement-scoped capability maps every request to one server. The worker keeps the capability source of truth outside the container, rotates it when placement changes, and stages only that server's credential for the plugin. Login tokens are stored hashed, expire after 15 minutes, and redeem once into a one-hour server-only browser session. These sessions cannot authenticate Kurbis accounts or access another server.

## Worker Isolation Hardening (In Progress)

The prior verification below predates the requested worker-isolation changes. The current implementation provisions a unique worker API token and PostgreSQL login per worker, enforces assigned-server access in PostgreSQL, keeps placement and lifecycle ownership in the control plane, and mediates object access through request-time worker authorization. Workers do not retain a shared internal token, unrestricted database URL, or fleet-wide object-store credentials. Focused role, lifecycle, object, maintenance, API, and browser verification is now complete; local Linux runtime/quota evidence remains separate.

Live Bedrock phone testing was canceled at the user's direction and is excluded from this task. Automated TLS/handoff and fresh browser verification passed; local Docker/Linux runtime and quota verification remain separate.

The standalone worker launcher environment matrix passes **7/7** subprocess checks for native and Docker modes, per-worker bundle values, secret scrubbing, explicit local HTTP opt-in, and missing/malformed bundle failures. Fresh local PostgreSQL checks now pass the historical identity migration target (**1/1**), real-role/RLS worker isolation target (**1/1**), worker-control API target (**9/9**, including actor-aware owner/worker stop parity, stale-actor rejection, and bounded retry diagnostics), object-store isolation target (**1/1**), CP maintenance/expiry target (**1/1**), and lifecycle/placement target (**27/27**). `cargo check --locked --all-targets`, formatting, and clippy with `-D warnings` pass. Fresh rebuilt browser E2E passed **17/17**; production Linux/Docker evidence remains separate.

Lifecycle HTTP adapters now share actor-aware command methods. Public owner/operator actions and assigned-worker stop/snapshot requests dispatch through one transactionally fenced `LifecycleActor` path; authentication derives actor identity and generation fences, while the canonical lifecycle service owns placement checks, locks, and state transitions. Retry, defer, completion, runtime-loss, and export routes already dispatch through shared lifecycle operations and resource-fenced helpers.

Minecraft admission now uses the authenticated waiting runtime for both editions. The runtime exposes authenticated Java UUIDs and validated Bedrock XUIDs; only verified identities receive one-use browser login links. Signed-out redemption creates a persistent session; signed-in redemption merges identities and account data. Account creation and Minecraft admission serialize on the account row, preserving the one-server capacity rule and a single default server.

World-only and full resets persist recoverable stages and generation fences. Export jobs pin immutable snapshots, stream ZIP64 world archives, validate source chunks and uploaded archive checksums, and expire after 24 hours.

## Verification

- With `TEST_DATABASE_URL` set to a disposable local PostgreSQL database, `cargo test --locked --all-targets -- --test-threads=1` passed **135 tests**. This includes server-capability rotation and scoping, same-generation worker relocation, login-token/session boundaries, identity migration and concurrent merges, suspension and world preservation, reset recovery, snapshots, worker leases, real worker roles/RLS, worker-control lifecycle fences, server-scoped streamed objects, CP maintenance/expiry, and ZIP64 archive contents/checksums.
- `cargo test --locked --manifest-path plugins/kurbis-control/Cargo.toml` passed **6/6** plugin tests.
- `cargo fmt --all -- --check` passed.
- Frontend unit tests passed **18/18**; TypeScript typecheck and production build passed. The build includes /auth/server and /manage/{serverId}.
- The fresh-database Playwright run passed **17/17** journeys, including server-scoped link redemption, account/cross-server denial, OAuth/session boundaries, resets, owner isolation, and security headers.
- Native Pumpkin runtime verification passed with the built-in plugin staged. It covered OP-only command visibility, link issuance and one-use redemption, cross-origin/replay/expiry rejection, cross-server and account API denial, plugin/capability file placement, worker restart, snapshot upload recovery, worker fencing, cross-worker restore, and the real five-minute idle timer after a rejoin.
- Local Docker readiness-only verification passed with the real nested Pumpkin runtime, short host-control socket path, plugin installation, and worker-to-control-plane transport. Full quota/lifecycle/GUI runtime verification remains unverified after subsequent harness/parser iterations; no green `result.json` is claimed.
- Cargo formatting, shell syntax checks, Python compilation, and `git diff --check` passed.

## Release limits

Real Microsoft/Xbox Bedrock login and handoff remain unverified: the pinned Pumpkin runtime has a documented NetherNet signaling TLS incompatibility. Controlled fixtures cover verified Bedrock XUID admission. Live Google/Discord accounts and Linux container memory/disk quota behavior require external accounts or Docker/Linux. Docker image execution and hard 2 GiB/5 GiB quota verification were unavailable because Docker Desktop was stopped and the local Docker socket was absent. No production deployment was performed.
