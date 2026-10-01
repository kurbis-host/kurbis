# Kurbis Implementation Progress

Last reviewed: 2026-10-01

## Status

Plan 1 authentication and account linking and Plan 2 server management, durable resets, and world export are implemented in the Rust/Postgres control plane, gateway, worker, and Next.js panel. Per-server APIs and panel pages require an active owner session. In-game management commands remain deferred.

Minecraft admission now uses the authenticated waiting runtime for both editions. The runtime exposes authenticated Java UUIDs and validated Bedrock XUIDs; only verified identities receive one-use browser login links. Signed-out redemption creates a persistent session; signed-in redemption merges identities and account data. Account creation and Minecraft admission serialize on the account row, preserving the one-server capacity rule and a single default server.

World-only and full resets persist recoverable stages and generation fences. Export jobs pin immutable snapshots, stream ZIP64 world archives, validate source chunks and uploaded archive checksums, and expire after 24 hours.

## Verification

- `cargo test --all-targets -j 1 -- --test-threads=1` passed **106 tests** against PostgreSQL. Coverage includes identity migration and concurrent merges, suspension and world preservation, authenticated Java/Bedrock identity fixtures, account-serialized admission races, server-owner isolation, SSE, reset recovery, snapshots, worker leases, and ZIP64 archive contents/checksums.
- `cargo fmt --all -- --check` passed.
- Frontend unit tests passed **18/18**; TypeScript typecheck passed. The clean production build emitted only `/app/servers/{serverId}` panel routes and `/auth/minecraft`, with no deferred guest routes.
- The fresh-database production Playwright run passed **12/12** journeys, including signed-in and signed-out Minecraft redemption, OAuth/session boundaries, both reset modes, owner isolation, responsive/accessibility checks, and security headers.
- Native Pumpkin worker lifecycle verification is being repeated after removing an obsolete plugin-only worker environment dependency. Docker verification is unavailable because Docker Desktop is stopped and the local Docker socket is absent.

## Release limits

Real Microsoft/Xbox Bedrock login and handoff remain unverified: the pinned Pumpkin runtime has a documented NetherNet signaling TLS incompatibility. Controlled fixtures cover verified Bedrock XUID admission. Live Google/Discord accounts and Linux container memory/disk quota behavior also require external accounts or Docker/Linux. No production deployment was performed.
