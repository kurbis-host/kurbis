# Kurbis Hoster

Kurbis Hoster is a Minecraft hosting platform built around a Rust control plane, a native Minecraft gateway, and worker agents that run isolated Pumpkin servers.

The project is designed for a free single-region hosting service where servers can sleep when unused, wake when players connect, and persist world data through snapshots.

## Architecture

The system is split into three main parts:

- **Control plane**: Rust/Axum service backed by PostgreSQL. It owns users, servers, placements, lifecycle operations, worker state, and routing information.
- **Gateway**: Rust Java hostname proxy, Bedrock UDP/RakNet transport, and authenticated Pumpkin waiting-session orchestration. Local protocol coverage uses real Java and Bedrock clients with disposable test identities; account-login verification is tracked in PROGRESS.md.
- **Workers**: Rust agents responsible for Docker containers or native host processes, local storage, snapshots, and Pumpkin lifecycle management.

The web panel in `web/` provides the user interface for managing hosted servers.

## Features

- One new hosted Minecraft server per account; account merges preserve all existing worlds.
- Java and Bedrock hostname routing, sleeping-server wake, waiting sessions, and native backend transfer, exercised with real protocol clients.
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

- Rust 1.98.1
- PostgreSQL
- Native host processes for local development (Docker is not needed)
- Docker only for isolated production workers and Linux hard-quota verification
- Node.js and npm (for the web panel)

## Development

Local development runs directly on macOS or Linux. See [native setup](runtime/pumpkin/NATIVE.md) for workers and the waiting gateway runtime. On macOS, workers default to the native backend.

Build the Rust project:

```bash
cargo build
```

Run Rust tests:

```bash
TEST_DATABASE_URL=postgres://localhost/melon_hoster_test cargo test --all-targets
```

Local panel development uses an isolated PostgreSQL database and the loopback OAuth fixture, so it does not need production OAuth credentials. The checked-out local settings live in the ignored `web/.env.local` file. Create the database once:

```bash
createdb kurbis_dev_e2e
```

Start the local OAuth fixture and control plane in separate terminals:

```bash
set -a; source web/.env.local; set +a
cargo run --example browser_fixture
```

```bash
set -a; source web/.env.local; set +a
cargo run --bin control-plane
```

Then run the panel:

```bash
cd web
npm install
npm run dev
```

The fixture signs in as a local development identity (`Browser Player`); it does not contact a real OAuth provider. The control plane listens on `127.0.0.1:8080`, and the panel on `http://localhost:3000`.

To test real Google and Discord sign-in on this PC, register these exact local callback URLs in the provider consoles:

```text
http://localhost:3000/auth/google/callback
http://localhost:3000/auth/discord/callback
```

Put both client IDs and secrets in the ignored `web/.env.local` file. Remove the provider-specific `*_OAUTH_AUTHORIZE_URL`, `*_OAUTH_TOKEN_URL`, and `*_OAUTH_USERINFO_URL` overrides so the control plane uses Google's and Discord's production OAuth endpoints. For this mode, run the control plane and panel without `examples/browser_fixture.rs`.

## Configuration

Runtime configuration is loaded through environment variables. See `src/config.rs` for available settings and defaults.

The control plane requires a Google OAuth client and a Discord OAuth application. Set `GOOGLE_OAUTH_CLIENT_ID`, `GOOGLE_OAUTH_CLIENT_SECRET`, `DISCORD_OAUTH_CLIENT_ID`, and `DISCORD_OAUTH_CLIENT_SECRET` in its runtime environment. Keep client secrets in the deployment secret store; never put production credentials in source control. Register these exact callback URLs with the providers:

```text
https://<public-panel-origin>/auth/google/callback
https://<public-panel-origin>/auth/discord/callback
```

Google requests `openid email profile`; Discord requests only `identify email`. The control plane supplies each provider's standard authorization, token, and profile endpoints. The corresponding `*_OAUTH_AUTHORIZE_URL`, `*_OAUTH_TOKEN_URL`, and `*_OAUTH_USERINFO_URL` settings can point to a local OAuth fixture during development and testing.

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
4. Start the assigned Pumpkin runtime.
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


## Verification and runtime builds

`PROGRESS.md` records completed checks and remaining requirements. The local implementation is covered by Rust/Postgres, browser, and real-protocol integration tests; Linux hard-quota and production-provider checks are called out separately where they cannot be established by local fixtures.

Browser lifecycle and OAuth verification uses a disposable database and real panel/API/Postgres, with controlled external provider and worker boundaries. Run it using [the E2E instructions](web/tests/e2e/README.md).

Build local hoster binaries with `cargo build --bins`. Native workers use the local host-control binary produced by [the native build script](runtime/pumpkin/NATIVE.md); they do not need Docker. Docker workers require the source-built Pumpkin image containing the host-control interface. Its Linux image can be built with `docker build -f runtime/Dockerfile -t melon-hoster:local .`; [runtime build instructions](runtime/pumpkin/README.md) pin the upstream revision and include the complete integration source.

To verify authenticated host-control behavior against a built Pumpkin binary, run `python3 tools/verify_host_control.py --binary /absolute/path/to/pumpkin`. This launches an actual disposable server and checks save/shutdown. The native lifecycle harness additionally exercises real Java and Bedrock protocol clients, waiting sessions, transfers, and cross-worker world recovery; it does not establish Linux hard-quota behavior.

For faster development on this Mac or Linux, use [native host execution](runtime/pumpkin/NATIVE.md). Build with `tools/build_pumpkin_native.sh` and select `WORKER_RUNTIME=native`; the shared lifecycle and snapshot system works without Docker. Native development mode has no hard memory or filesystem isolation.
