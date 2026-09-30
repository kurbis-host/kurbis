# Minecraft Hoster v1

## Summary

Build a free single-region Minecraft hosting platform around Pumpkin with:

- Rust control plane, native Rust Minecraft gateway, and Rust worker agent.
- Next.js public panel.
- Docker-isolated Pumpkin instance per hosted server.
- Java and Bedrock support matching Pumpkin.
- One free server per account.
- 2 GiB RAM, 5 GiB persistent storage, no explicit player-count limit.
- Automatic sleep after 5 minutes with zero players.
- Wake-on-join with a waiting experience, then Minecraft-native transfer to the real backend.
- Postgres as the only control-plane database/coordination system.
- Worker-local SSD for active data plus periodic incremental object-store snapshots.
- Servers may move workers only while stopped/idle.
- Hard resource quotas and a wake/start queue when capacity is exhausted.

## Key Changes

### Edge and routing

Create a native Rust gateway responsible for all public Minecraft traffic.

- Java listens on :25565; wildcard DNS such as \*.example.com points at the gateway.
- Parse the Java handshake hostname to resolve subdomain -> server.
- For an already-running server, proxy the connection to its assigned worker.
- For a sleeping server, terminate the temporary Minecraft session in the gateway, request a wake, present a starting/queued state, and transfer the client once the backend reports ready.
- Bedrock uses its normal UDP/RakNet entrypoint. Complete enough of the Bedrock login flow to read the client's ServerAddress, resolve the hostname, then use Bedrock's native transfer packet once the target is ready.
- Treat malformed, unknown, deleted, suspended, and unavailable hostnames as explicit Minecraft protocol responses rather than raw socket failures.

The gateway keeps no authoritative routing state locally. It reads/caches routing data derived from Postgres and invalidates that cache from control-plane changes.

### Control plane and data model

Use one Rust/Axum service with Postgres as the durable source of truth.

Core entities:

- users: OAuth identity and account status.
- servers: owner, subdomain, desired state, observed state, configuration, storage usage.
- workers: capacity, health heartbeat, resource reservations, draining state.
- placements: current server-to-worker assignment and generation/lease metadata.
- snapshots: object-store key, generation, size, checksum, creation reason.
- start_requests: queued wake/manual-start requests and ordering.
- operations: durable lifecycle operations for idempotent start/stop/restore/snapshot handling.

Use transactions, row locks, advisory locks where useful, and explicit generation numbers so multiple coordinator requests cannot start the same server twice.

Public API/panel supports:

- OAuth login.
- Create/delete one server.
- Select/change an available subdomain while stopped.
- Start/stop/restart.
- Current state: sleeping, queued, restoring, starting, online, stopping, snapshotting, failed.
- Basic console/log viewing.
- Curated Pumpkin settings only.
- Storage usage and 5 GiB quota.
- Server address for Java and Bedrock.

No payments, arbitrary binaries, plugin uploads, or unrestricted file manager in v1.

### Workers, scheduling, and persistence

Each machine runs a Rust worker agent with access to Docker, local server storage, and object storage.

Placement uses resource-aware bin packing:

- Require sufficient unreserved RAM, disk, and CPU headroom.
- Reserve 2 GiB RAM per running hosted server.
- Prefer a worker that already has the latest snapshot cached locally.
- Never migrate an active server merely for balancing.
- Reassign sleeping servers freely because object storage is authoritative between runs.

Start flow:

1. Acquire the server lifecycle lock in Postgres.
2. If capacity is unavailable, insert/retain one queue entry.
3. Select a healthy worker.
4. Worker restores the newest complete snapshot if local state is stale/missing.
5. Validate storage remains within 5 GiB.
6. Start the constrained Pumpkin container.
7. Wait for an explicit readiness check.
8. Publish the active placement.
9. Gateway transfers waiting clients to the backend.

Stop flow:

1. After zero players for 5 continuous minutes, mark stop requested.
2. Prevent new direct routing while shutdown begins.
3. Ask Pumpkin to save cleanly.
4. Stop Pumpkin gracefully.
5. Produce and upload the final incremental snapshot.
6. Atomically mark that snapshot current.
7. Release worker resource reservations and mark the server sleeping.

Periodic persistence:

- Trigger Pumpkin save plus an incremental/deduplicated snapshot on a fixed short interval while running; default 5 minutes for v1.
- Never expose a partial upload as the latest valid snapshot.
- Snapshot generations are immutable; publish the new generation only after checksum/manifest validation.
- Keep several recent recovery generations rather than immediately deleting the predecessor.
- Clean shutdown always creates a final snapshot even if a periodic snapshot just ran.

The first implementation should use an S3-compatible object-store abstraction so production can use any compatible provider without storage-provider logic leaking into lifecycle code.

### Pumpkin integration

Keep hosting-specific orchestration outside the Pumpkin fork where possible.

Use Pumpkin's existing:

- player-count information,
- save-all path,
- graceful shutdown,
- Java/Bedrock transfer support,
- join/leave lifecycle.

Add only a small stable host-control interface where polling or external control is insufficient, preferably authenticated loopback/Unix-socket RPC exposed to the worker agent. It should provide readiness, player count, save completion, and graceful shutdown.

The worker owns Docker/container state. Pumpkin must not communicate directly with Postgres or schedule itself.

## Test Plan

Test the complete lifecycle rather than isolated smoke tests:

- Java and Bedrock hostname routing for multiple subdomains sharing one public gateway.
- Sleeping join -> wake -> restore -> ready -> transfer.
- Multiple simultaneous joins to one sleeping server cause exactly one start.
- Capacity exhaustion queues a wake, preserves the waiting experience, then starts it when capacity is released.
- Five-minute idle timer resets when a player rejoins.
- Graceful stop persists player/world data and resumes identically on another worker.
- Worker reassignment of sleeping servers with and without cached snapshots.
- Periodic snapshot followed by abrupt worker loss restores to the latest completed generation without accepting partial snapshots.
- Concurrent start/stop/delete requests remain idempotent through Postgres locking/generation checks.
- Gateway/control-plane restart does not lose placements or queued wakes.
- Worker heartbeat loss makes its routes unavailable and does not start a second copy until the previous placement is proven inactive.
- 2 GiB memory and 5 GiB storage quotas are enforced under real container load.
- Unknown/invalid subdomains, failed restores, corrupt snapshots, object-store outage, full workers, and Pumpkin startup failure produce deterministic states and recoverable retries.
- Browser E2E tests cover OAuth, server creation, address selection, lifecycle controls, queue state, logs, and deletion.

## Assumptions

- V1 is one region with redundant gateway/control-plane processes but a single logical Postgres cluster.
- One account owns at most one hosted server.
- OAuth-only authentication; provider selection can be configured without changing the account model.
- Pumpkin is the only supported server runtime in v1.
- No explicit player limit is imposed beyond what 2 GiB RAM and Pumpkin itself can sustain.
- Bedrock hostname routing is implemented at the Minecraft login layer using ServerAddress; validating this against the exact Pumpkin-supported Bedrock protocol version is an early gateway milestone before building the rest of the Bedrock waiting flow.
- Migrations occur only when a server is empty/stopped; active-player live migration is outside v1.
- Object storage is the cross-worker persistence authority; local worker disks are disposable performance caches.
