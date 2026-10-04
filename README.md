# Kurbis Hoster

Kurbis Hoster is a Minecraft hosting platform built around a Rust control plane, a native Minecraft gateway, and worker agents that run isolated Pumpkin servers.

The project is designed for a free single-region hosting service where servers can sleep when unused, wake when players connect, and persist world data through snapshots.

## Architecture

The system is split into three main parts:

- **Control plane**: Rust/Axum service backed by PostgreSQL. It owns users, servers, placements, lifecycle operations, worker state, and routing information.
- **Gateway**: Rust Java hostname proxy, Bedrock transport, and authenticated Pumpkin waiting-session orchestration. Local protocol coverage uses a real Java 26.3 client with disposable test identities; Bedrock 1.26.51 login is blocked by the pinned NetherNet signaling issue.
- **Workers**: Rust agents responsible for Docker containers or native host processes, local storage, snapshots, and Pumpkin lifecycle management.

The web panel in `web/` provides the user interface for managing hosted servers.

## Features

- Verified online Minecraft Java and Bedrock identities automatically register or resume an account on join.
- The first valid address creates that account’s server; each account has one server, and account merges preserve all existing worlds.
- Java 26.3 hostname routing, sleeping-server wake, waiting sessions, and native backend transfer, exercised with a real protocol client.
- Wake-on-join for sleeping servers.
- Automatic sleep after inactivity.
- Worker scheduling with resource reservations.
- Persistent world snapshots using S3-compatible object storage.
- Server lifecycle operations with PostgreSQL-backed locking and recovery.
- OP-only `/kurbis` in-game management, with a one-use 15-minute login link that opens a persistent, independently revocable panel grant for that server without signing into a Kurbis account. Grant authorization remains bound to one server and is also denied after server deletion or owner suspension.
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
- Docker Engine and Compose v2 for the container deployment and Docker workers
- Node.js and pnpm 11.2.2 (for the web panel)

## Development

Local development runs directly on macOS or Linux. See [native setup](runtime/pumpkin/NATIVE.md) for workers and the waiting gateway runtime. On macOS, workers default to the native backend.

Build the Rust project:

```bash
cargo build
```

Run Rust tests:

```bash
TEST_DATABASE_URL=postgres://localhost/melon_hoster_test \
  cargo test --locked --all-targets -- --test-threads=1
```

Use a disposable local database. The schema is bootstrapped from the canonical
empty-database definition; historical migration fixtures and upgrade
compatibility are not supported.

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
pnpm install --frozen-lockfile
pnpm run dev
```

The fixture signs in as a local development identity (`Browser Player`); it does not contact a real OAuth provider. The control plane listens on `127.0.0.1:8080`, and the panel on `http://localhost:3000`.

To test real Google and Discord sign-in on this PC, register these exact local callback URLs in the provider consoles:

```text
http://localhost:3000/auth/google/callback
http://localhost:3000/auth/discord/callback
```

Put both client IDs and secrets in the ignored `web/.env.local` file. Remove the provider-specific `*_OAUTH_AUTHORIZE_URL`, `*_OAUTH_TOKEN_URL`, and `*_OAUTH_USERINFO_URL` overrides so the control plane uses Google's and Discord's production OAuth endpoints. For this mode, run the control plane and panel without `examples/browser_fixture.rs`.

## Start a deployment

The root launcher starts the full service set in a tmux dashboard. The first
run creates a private `.env`; set the public panel URL, Minecraft root domain,
worker's reachable IP, and Google/Discord OAuth credentials in that file.
Register these callback URLs with both providers:

```text
https://<public-panel-origin>/auth/google/callback
https://<public-panel-origin>/auth/discord/callback
```

Choose one deployment mode:

```sh
./start.sh native debug    # host processes, fast Rust and Next.js iteration
./start.sh native release  # host processes, optimized Rust and production panel
./start.sh docker          # Compose services plus the host Docker worker
```

Native mode needs Rust 1.98.1, Git, CMake, Node.js/pnpm, tmux, and a local
PostgreSQL server. If `DATABASE_URL_NATIVE` cannot connect, the launcher uses
the current operating-system PostgreSQL role to create or update the configured
database role and create the database. That local role must have PostgreSQL
administrator permissions; remote PostgreSQL databases must be provisioned
separately. Native debug accepts an HTTP loopback `PUBLIC_BASE_URL`; native
release and Docker require HTTPS. Native mode does not use Docker. Docker mode
builds the pinned Pumpkin images and application images, starts PostgreSQL,
control plane, gateway, waiting runtime, and panel in Compose, then runs the
worker on the Linux host. The worker stays on the host because it controls the
host Docker engine and worker storage mounts. Docker mode needs Linux, Docker
Engine with Compose v2, Rust/Cargo, and tmux; use native mode on macOS. The
first Pumpkin image build takes substantially longer than later starts.

For native debug with `ROOT_DOMAIN=localhost`, the launcher creates a local
self-signed Bedrock TLS certificate when certificate paths are not configured.
Clients will need to trust that certificate to validate the local server. For
other hostnames, configure `BEDROCK_TLS_CERT_FILE` and
`BEDROCK_TLS_KEY_FILE` with a certificate trusted for the root domain and its
server subdomains.

The Docker Compose deployment publishes the panel on `PANEL_PORT`, Java on
`JAVA_PORT`, and Bedrock TCP and UDP on `BEDROCK_PORT`. For Docker deployments,
`BEDROCK_TLS_HOST_DIRECTORY` must point to a host directory containing
`fullchain.pem` and `privkey.pem`; Compose mounts that directory read-only at
`/run/kurbis-tls` and configures the gateway to use those files. Provide a
certificate trusted for `ROOT_DOMAIN` and its server subdomains. The private key
must be readable by the gateway's container UID/GID `2613:2613` without granting
world access (for example, a root-owned key with group `2613`, mode `0640`, and
traversable parent directories or an equivalent ACL). Do not make the key
world-readable to work around container permissions. The Docker gateway does not
generate production certificates.

Put an HTTPS reverse proxy in front of the panel port and point `PUBLIC_BASE_URL`
at that HTTPS origin. Open
the worker's configured TCP/UDP port range (`WORKER_PORT_MIN` through
`WORKER_PORT_MAX`) as well as the gateway ports. For enforced Linux filesystem
quotas, set `WORKER_STORAGE_MODE=loopback_ext4` and run the worker with the
permissions needed to mount ext4 loop devices; `directory` mode has no hard
disk quota. The starter `.env` uses local snapshot storage under `data/objects`;
for production durability, set `OBJECT_STORE_TYPE=s3` and configure the `S3_*`
values. Set `DOCKER_SUBNET`, `WAITING_IP`, and `GATEWAY_IP` if the default
Compose subnet conflicts with an existing network.

Use `./start.sh attach` to reopen the dashboard and `./start.sh stop` to stop
the dashboard and Compose services. In tmux, use the mouse or `Ctrl-b` followed
by an arrow key to select a process pane; `Ctrl-b z` zooms the selected pane,
and `Ctrl-b d` detaches. Persistent PostgreSQL data is kept when stopping
Compose. Stop hosted worlds from the panel before shutting down their worker.

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

To verify authenticated host-control behavior against a built Pumpkin binary, run `python3 tools/verify_host_control.py --binary /absolute/path/to/pumpkin`. This launches an actual disposable server and checks Java 26.3 login, save/shutdown, and player activity. The native lifecycle harness also exercises Java waiting sessions, transfers, and cross-worker world recovery; it does not establish Linux hard-quota behavior.

For faster development on this Mac or Linux, use [native host execution](runtime/pumpkin/NATIVE.md). Build with `tools/build_pumpkin_native.sh` and select `WORKER_RUNTIME=native`; the shared lifecycle and snapshot system works without Docker. Native development mode has no hard memory or filesystem isolation.
