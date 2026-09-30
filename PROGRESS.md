# Minecraft Hoster v1 progress

Last updated: 2026-09-30

## Completion status

PLAN.md remains incomplete. This document replaces conflicting audit entries from earlier passes. Code inspection and passing unit tests alone do not establish end-to-end Minecraft, Docker, OAuth, or deployment behavior.

## Implementation present

- Rust/Axum control plane and Postgres migrations for users, sessions, servers, workers, placements, snapshots, queues, operations, logs, reservations, and port leases.
- OAuth flow, one-server ownership, lifecycle API, curated settings, subdomain editing, and Next.js panel.
- Worker Docker orchestration, host-control readiness/save/shutdown/player polling, idle sleep, periodic/final snapshots, local storage, runtime-loss handling, and operation recovery.
- S3-compatible/local object storage, immutable snapshot manifests, content-addressed chunks, integrity validation, and recovery-generation retention.
- Java hostname gateway with Postgres route caching/invalidation, running-server TCP proxying, and forwarding sleeping clients into a separate Pumpkin waiting server. Waiting-player host-control messages and native transfers are implemented in orchestration code, but the complete client flow is unverified.
- Bedrock backend ports and edition-specific transfer orchestration exist. The Bedrock gateway module currently only binds UDP and discards datagrams. Its standalone ClientData JSON parser is unused by the network path. This is a placeholder, not a working Bedrock gateway.
- External hosting Pumpkin fork: `/Users/user/Developer/Developer/games/minecraft/Pumpkin-melon-host-control`. The hoster repository does not yet contain a reproducible deployment of this dependency.

## Current implementation pass

- Removed silent success from database-test setup. Both integration suites now use a shared helper and fail on missing TEST_DATABASE_URL, connection failure, or migration failure.
- Corrected fixtures that assumed a particular worker would win ties between equal-capacity workers. The second worker now appears after the first runtime is assigned.
- Fixed resource admission when worker ports are exhausted. A worker needs a free adjacent Java/Bedrock port pair as well as RAM/disk/CPU; exhaustion queues the request.
- Added migration 0003 so `port_leases` exclusively reserve ports. Sleeping placements retain their cache/assignment metadata without stale uniqueness constraints preventing reuse by other servers.
- Serialized start admission with a Postgres transaction advisory lock and honor the oldest durable queue entry, including when a new gateway/manual request arrives before queue draining. Queue ordering uses requested_at plus server_id for deterministic ties.
- Queue drainers now recheck queued intent under the server lock and skip cancelled/deleted entries from an earlier snapshot. A deterministic regression waits for the actual Postgres advisory-lock waiter before cancelling/removing the server.
- Fixed the waiting-session startup deadline: time spent queued no longer causes an immediate disconnect when restore/start finally begins. Admission receives a fresh startup timeout.

## Verification checklist and evidence

Integration tests use the local disposable Postgres database on 127.0.0.1:55432. Earlier invocations without TEST_DATABASE_URL silently returned from database tests and must not be counted as integration verification.

Regression coverage added this pass:

- **Integration:** port exhaustion queues rather than failing; graceful-stop completion releases ports; another server reuses them while the sleeping placement survives; the sleeping server queues on rejoin while those ports are occupied.
- **Integration:** a new join cannot overtake older queued wakes; concurrent coordinators drain the queue; released capacity admits the next request; a queued request can be cancelled.
- **Unit:** a long capacity wait preserves the full restore/start timeout, including returning to the queue and being admitted again.

Completed checks:

```text
TEST_DATABASE_URL=postgres://user@127.0.0.1:55432/melon_hoster_test cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

All passed: 14 unit tests, 14 lifecycle tests executing against Postgres, and 3 snapshot tests executing against Postgres/local object storage. No browser E2E or real Minecraft/Docker lifecycle tests were run in this pass.

Read-only environment discovery: SSH to respond5 works; Docker 27.3.1 and Dokploy are running. No Melon/Pumpkin hoster runtime appeared in the running-container inventory. No Dokploy MCP tool is currently callable in this session; this does not prevent continuing local implementation or SSH-based runtime investigation.

## Remaining requirement-matched work

- Implement the complete public Bedrock session/login/hostname/native-transfer path against the exact supported Pumpkin transport and protocol. The current Pumpkin checkout uses NetherNet/WebRTC; the plan specifies RakNet. Resolve that implementation mismatch with runtime evidence while preserving Java and Bedrock support.
- Validate Java waiting login, wake, restore, readiness, messages, and native transfer with real clients and multiple hostnames. Add deterministic protocol errors for unavailable backends and malformed traffic.
- Verify route-cache recovery across database reconnections. Route/wake requests, queued admission, and completion of already-claimed starts now enforce suspension; normal account/worker/server invalidation is covered.
- Audit lifecycle idempotency, stale completion fencing, start/stop/restart/delete races, cancelled queue-drain races, worker lease loss, operation recovery, and inactive-runtime proof. Current tests cover only part of these invariants.
- Validate sleeping-worker reassignment with/without cached data, periodic snapshot plus abrupt loss, final shutdown snapshot, restore on another worker, chunk cleanup, publication immutability, and object-store failures with recovery.
- Run real Docker/Pumpkin tests for readiness/save/shutdown, player-count idle reset, resource limits under load, unexpected runtime death, clean restart/restore, and quota enforcement. Directory storage mode is development-only and does not prove a hard filesystem quota.
- Add/run browser E2E for OAuth, creation, addresses/settings, lifecycle/queue/logs, deletion, and responsive states.
- Add reproducible build/deployment configuration and operations documentation; inspect respond5/Dokploy and verify restart behavior without losing placements or queued requests.
- Complete final review and requirement-by-requirement verification, then commit only task work with a detailed conventional commit message. Existing project files are untracked; do not claim the task is finished on that basis.

## Account routing and cache pass

- Added account suspension to authoritative route results. Suspended routes expose no backend address or ports. Java login/status and waiting-session handling report suspension explicitly; lifecycle admission rejects wakes, and queue draining removes suspended entries so they cannot block active owners.
- Added migration 0004 notifications for account status, worker heartbeat/address, and server insertion/deletion/desired-state changes. Existing placement/state notifications remain in place.
- Gateway subscribes to invalidations before serving traffic and supervises the listener alongside protocol tasks. An invalidation failure terminates the process instead of leaving an unsupervised task dead.
- Cache generations prevent an in-flight database fetch from republishing a stale route after invalidation. Healthy cache entries expire no later than the worker heartbeat deadline; this preserves placement fencing while making stale workers unavailable even without a notification.
- Java wake/waiting/backend connection failures now send login disconnect packets; backend connection attempts have a five-second timeout.

Coverage checklist:

- **Postgres integration:** suspended owner cannot wake; queued suspension is removed without consuming a generation; account suspension/reactivation, worker address/heartbeat updates, and server deletion intent notify route watchers.
- **Postgres integration:** cached online route expires at heartbeat deadline even without a listener; unreachable runtime remains fenced.
- **Protocol unit:** real TCP framing returns explicit disconnects for suspension, failed wake requests, unreachable waiting server, and unreachable running backend.

Final verification passed:

```text
TEST_DATABASE_URL=postgres://user@127.0.0.1:55432/melon_hoster_test cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

35 tests passed: 15 unit/protocol tests, 3 Postgres gateway-routing tests, 14 Postgres lifecycle tests, and 3 Postgres/local-object-store snapshot tests. Browser E2E, Bedrock sessions, real Pumpkin transfers, and Docker lifecycle behavior remain unverified.

## Durable operation completion pass

- Split durable operation claim/retry/failure/completion logic into `src/control_plane/lifecycle/operations.rs`; lifecycle admission and scheduling remain in the parent module.
- Completion and terminal failure lock the server and authoritative operation row, verify server/worker/kind/generation/attempt identity, and reject pending, superseded, or incompatible terminal claims. Repeating the same completed result is a no-op.
- Start completion verifies its placement before publishing readiness. Late replay after shutdown cannot make a sleeping server online. Stop/failure replays cannot subtract another server's reserved resources or overwrite the published snapshot pointer.
- Retry updates require the current claim attempt and worker identity. A late failure from a recovered attempt cannot requeue the replacement claim.
- Exhausted starts retain reservations and a placement fence until container removal is confirmed. Worker reconciliation retries failed removal instead of ignoring Docker errors; only confirmed cleanup calls runtime-loss reconciliation to release capacity. This cleanup path still needs real Docker verification.
- If the owner is suspended while a start is claimed, readiness completion enqueues one clean stop and preserves stopped intent. Duplicate readiness delivery does not enqueue another stop.

Regression checklist (Postgres integration): concurrent/repeated stop completion, delayed start completion after stop, stale recovered claim completion/retry/failure, wrong worker identity, failure replay after success, failed-start fencing and explicit inactivity proof, and suspension during claimed start.

Final checks passed:

```text
TEST_DATABASE_URL=postgres://user@127.0.0.1:55432/melon_hoster_test cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

39 tests passed: 15 unit/protocol, 3 Postgres routing, 18 Postgres lifecycle, and 3 Postgres/local-object-store snapshot tests. Bedrock, browser E2E, live Docker/Pumpkin, and deployment verification remain open.

## Disposable worker-disk recovery pass

- Extracted stopped-server data preparation into `src/worker/persistence.rs`, used directly by worker start handling.
- A Postgres cached-generation hint is now checked against the complete published manifest: local file counts, paths, lengths, and BLAKE3 contents must match. Dirty files, unpublished extras, and missing worker storage trigger a reset and restore from object storage.
- With no completed snapshot, unpublished leftovers are removed instead of being treated as authoritative world data. Cache metadata is cleared before reset and is recorded again only after a successful restore of the expected generation.
- Recovered starts inspect Docker generation before touching local data. A live container for the same lifecycle generation follows readiness recovery without resetting its storage; another live generation is rejected.
- Docker inspection failures now distinguish an absent container from an unavailable/erroring daemon. A daemon failure cannot authorize destructive restore by falsely reporting that no container exists.
- Manifest loading and checksum/identity validation are shared by restore and local-cache validation.

Regression checklist (Postgres + real local filesystem/object-store integration): a completed periodic snapshot with a newer incomplete generation; same-sized dirty world data; extra unpublished files; full local directory loss with a surviving database cache hint; restore on another worker with no cache; preserved player inventory; no-snapshot cleanup; corrupt chunk failure without cache publication; successful retry after chunk repair.

These tests execute the production storage preparation helper in directory mode. They also reject a symlink substituted for a manifest file even when an extra file preserves the total file count. They do not prove real Docker crash recovery, hard ext4 quotas, Pumpkin save consistency, or client reconnection.

Final checks passed:

```text
TEST_DATABASE_URL=postgres://user@127.0.0.1:55432/melon_hoster_test cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

41 tests passed: 15 unit/protocol, 3 Postgres routing, 18 Postgres lifecycle, and 5 Postgres/filesystem snapshot tests. The full PLAN.md is still incomplete; its open requirements above remain in scope.
