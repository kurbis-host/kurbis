# Kurbis Hoster

Kurbis Hoster is a Minecraft hosting platform built around a Rust control plane, a native Minecraft gateway, and worker agents that run isolated Pumpkin servers.

The project is designed for a free single-region hosting service where servers can sleep when unused, wake when players connect, and persist world data through snapshots.

## Architecture

The system is split into three main parts:

- **Control plane**: Rust/Axum service backed by PostgreSQL. It owns users, servers, placements, lifecycle operations, worker state, and routing information.
- **Gateway**: Native Rust Minecraft gateway that accepts public Java and Bedrock connections, resolves server addresses, and handles sleeping-server wake flows.
- **Workers**: Rust agents responsible for Docker containers, local storage, snapshots, and Pumpkin lifecycle management.

The web panel in `web/` provides the user interface for managing hosted servers.

## Features

- One hosted Minecraft server per account.
- Java and Bedrock routing support.
- Wake-on-join for sleeping servers.
- Automatic sleep after inactivity.
- Worker scheduling with resource reservations.
- Persistent world snapshots using S3-compatible object storage.
- Server lifecycle operations with PostgreSQL-backed locking and recovery.
- Container isolation through Docker.

## Repository Layout

```
.
├── src/              # Rust control plane, gateway, worker, and integrations
├── migrations/       # PostgreSQL schema migrations
├── tests/            # Integration tests
├── web/              # Next.js management panel
├── test-server/      # Pumpkin test server resources
├── PLAN.md           # Detailed architecture and implementation plan
└── PROGRESS.md       # Current development progress
```

## Requirements

- Rust 1.90+
- PostgreSQL
- Docker
- Node.js and npm (for the web panel)

## Development

Build the Rust project:

```bash
cargo build
```

Run Rust tests:

```bash
cargo test
```

Run the web panel:

```bash
cd web
npm install
npm run dev
```

## Configuration

Runtime configuration is loaded through environment variables. See `src/config.rs` for available settings and defaults.

The production deployment uses PostgreSQL for durable state and an S3-compatible object store for snapshots.

## Lifecycle Model

A hosted server moves through states such as:

```
sleeping → queued → restoring → starting → online → stopping → snapshotting → sleeping
```

Starting a server:

1. Acquire the lifecycle lock.
2. Reserve worker capacity.
3. Restore the latest valid snapshot if needed.
4. Start the Pumpkin container.
5. Verify readiness.
6. Publish routing information.

Stopping a server:

1. Stop accepting new routes.
2. Save the world cleanly.
3. Create and validate a snapshot.
4. Release worker resources.
5. Return the server to sleep.

## Testing Goals

The test suite focuses on full lifecycle behavior:

- Concurrent wake requests only start one server.
- Gateway routing survives control-plane restarts.
- Snapshot recovery works after worker loss.
- Worker capacity limits are enforced.
- Invalid routes and failed lifecycle operations recover cleanly.

## Current Status

See [`PROGRESS.md`](PROGRESS.md) for the current implementation state and [`PLAN.md`](PLAN.md) for the full design.
