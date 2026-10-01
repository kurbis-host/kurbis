# Minecraft Hoster v1

## Summary

Build a free single-region Minecraft hosting platform around Pumpkin with:

- Rust control plane, native Rust Minecraft gateway, and Rust worker agent.
- Next.js public panel.
- Docker-isolated Pumpkin instance per hosted server.
- Java and Bedrock support matching Pumpkin.
- One free server for a newly created account; identity merges retain every existing world and select one default.
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

- users: account status and display name.
- identities: provider and verified subject for Google, Discord, Minecraft Java, or Minecraft Bedrock.
- sessions: opaque, revocable browser sessions.
- servers: owner, subdomain, desired state, observed state, configuration, storage usage.
- workers: capacity, health heartbeat, resource reservations, draining state.
- placements: current server-to-worker assignment and generation/lease metadata.
- snapshots: object-store key, generation, size, checksum, creation reason.
- start_requests: queued wake/manual-start requests and ordering.
- operations: durable lifecycle operations for idempotent start/stop/restore/snapshot handling.

Use transactions, row locks, advisory locks where useful, and explicit generation numbers so multiple coordinator requests cannot start the same server twice.

Public API/panel supports:

- Google/Discord OAuth login and explicit provider linking.
- Verified Minecraft Java/Bedrock sign-in and linking through one-use browser redemption links.
- Account identity listing, browser-session listing/revocation, preserved-world/default-world selection, and logout.
- Create/delete one server for a new account; preserve all worlds when identities merge.
- Select/change an available subdomain while stopped.
- Start/stop/restart.
- Built-in OP-only Pumpkin management menu and commands for server status, players, logs, activity, settings, whitelist, snapshots, and lifecycle controls.
- One-use server-only login link from the in-game plugin into the existing panel. It creates a server-scoped browser session and does not sign in to a Kurbis account.
- Current state: sleeping, queued, restoring, starting, online, stopping, snapshotting, failed.
- Basic console/log viewing.
- Curated Pumpkin settings only.
- Storage usage and 5 GiB quota.
- Server address for Java and Bedrock.

No payments, arbitrary binaries, plugin uploads, or unrestricted file manager in v1.

### Workers, scheduling, and persistence

Each machine runs a Rust worker agent with access to a runtime, local server storage, and object storage. Production workers use Docker isolation; local development on macOS or Linux can run the same Pumpkin host-control build as a native process without Docker.

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

### Kurbis server plugins

Kurbis should ship first-party Pumpkin plugins that bring hosting controls into Minecraft while reusing the existing HTTP API. V1 starts with the built-in management plugin and its one-use server login link; this is the foundation for future Kurbis-owned server plugins, not a third-party plugin marketplace or arbitrary upload feature.

The worker keeps each server capability outside its container and stages only that server's credential for the managed plugin. The API derives server scope from the capability; a plugin cannot select another server ID. Placement changes rotate the capability. Hosted runtime configuration grants the managed plugin only its declared permissions and keeps unsigned third-party plugins disabled.

Workers are isolated from one another at the HTTP API, PostgreSQL, and object-store boundaries. The deployment CLI provisions a distinct worker ID, API credential, and restricted database credential for each worker; the control plane stores only a hash of each API credential. The control plane derives worker identity from the authenticated credential, never from a request field. A worker may obtain a server capability and access server-scoped data only while that server's current placement belongs to it. Shared internal credentials cannot issue server capabilities. Worker database roles cannot read or modify capability or worker-credential records, change placements or lifecycle generations, or access server-scoped rows outside their current assignments. Workers do not receive fleet-wide object-store credentials; snapshot and export object transfers go through control-plane endpoints that authorize the worker, server, assignment kind, and generation for every request and reauthorize streamed reads. Placement ownership, lease generations, server ownership, and desired state remain control-plane managed. Missing, duplicated, stale, or revoked worker credentials fail closed.

The worker owns Docker/container state. Pumpkin must not communicate directly with Postgres or schedule itself.

The native development runtime shares worker leases, lifecycle operations, storage manifests, snapshots, recovery, and gateway transfers with Docker workers. It is intended for trusted local development and does not provide hard memory or filesystem isolation. Linux container tests remain required to verify production quota enforcement.

### Identity and account linking

Use a provider-neutral identities table rather than provider columns on users. Link Google and Discord identities automatically only when both providers confirm the same email; normalize verified email by trimming whitespace and lowercasing. Preserve legacy provider identities during migration without treating an email as verified when its original provider-verification status was not stored.

Minecraft login links are issued only for identities proven by successful online Java authentication or validated Bedrock Xbox authentication. Bind links to the authenticated game connection, store only a hash of a random single-use token, and expire it after 15 minutes. Show the link privately in the waiting session. Redeem it through a same-origin POST; a GET never changes account state. Signed-out redemption creates or resumes the Minecraft account. Signed-in redemption merges the Minecraft account into the browser account.

An account merge moves identities, sessions, and server ownership transactionally. Preserve snapshots, operations, logs, events, player history, whitelist data, and all other records attached to the existing server IDs. A suspended source or destination leaves the merged account suspended. Keep the browser account's selected default world; if it has none, choose the oldest surviving world. There remains one default world after each merge.

Browser sessions use random opaque tokens stored as hashes in Postgres and Secure, HttpOnly, SameSite cookies. Renew cookie retention during visits and support listing and revoking individual sessions.

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
- Identity tests cover legacy-data migration, normalized verified-email linking under concurrency, suspended-account merges, preservation of all server-linked history, and default-world selection.
- Browser security tests cover OAuth provider linking, signed-in and signed-out Minecraft redemption, one-use/15-minute tokens, cross-origin rejection, forged or offline identity rejection, persistent sessions, revocation, and logout.
- Per-server API tests verify every server route checks active account ownership, including events, logs, exports, players, whitelist, snapshots, and lifecycle mutations.
- Server-access tests cover current-placement capability rotation, malformed and stale credentials, one-use 15-minute login links, one-hour server-only sessions, OP-only plugin commands, and denial of cross-server and account-wide APIs.
- Worker-isolation tests cover unique credential provisioning, rejection of the shared internal token and forged worker IDs, capability issuance only for the authenticated worker's current placement, stale credentials after reassignment, and rotation/revocation behavior.
- PostgreSQL permission tests prove a worker role cannot read or change capability or credential records, cannot modify placements or lifecycle generations, and can read or write server-scoped operational data only for its current assignments. Cross-worker attempts are tested before and after placement changes.
- Object-store isolation tests prove a worker cannot read, write, head, or delete another server's objects, cannot reuse an old assignment or generation, and loses access during streamed transfers after revocation or reassignment.
- Native end-to-end tests use real Java clients and available automated Bedrock protocol fixtures to cover authenticated waiting, wake, queueing, transfer, snapshots, recovery, and worker fencing. Live Bedrock phone testing is out of scope. Use Linux containers separately for hard memory and disk quota tests; if local Docker is unavailable, record that limitation and do not treat native verification as quota evidence.

## Assumptions

- V1 is one region with redundant gateway/control-plane processes but a single logical Postgres cluster.
- A new account can create one hosted server. Merging identities may retain multiple existing worlds under one account.
- Google and Discord OAuth and verified Minecraft identities are supported. The provider-neutral identity model allows adding providers without changing account ownership.
- Pumpkin is the only supported server runtime in v1.
- No explicit player limit is imposed beyond what 2 GiB RAM and Pumpkin itself can sustain.
- Bedrock hostname routing is implemented at the Minecraft login layer using ServerAddress and is verified against the pinned Pumpkin-supported protocol in native integration tests.
- Migrations occur only when a server is empty/stopped; active-player live migration is outside v1.
- Object storage is the cross-worker persistence authority; local worker disks are disposable performance caches.
