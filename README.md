<p align="center">
  <img src="assets/kurbis.svg" alt="" width="96" height="96">
</p>

<h1 align="center">Kurbis</h1>

<p align="center">
  Free Minecraft servers that sleep when nobody plays and wake when someone joins.<br>
  Java and Bedrock, on <a href="https://github.com/Pumpkin-MC/Pumpkin">Pumpkin</a>.
</p>

<p align="center">
  <a href="https://kurbis.host">kurbis.host</a> ·
  <a href="docs/self-hosting.md">Self-hosting</a> ·
  <a href="docs/development.md">Development</a> ·
  <a href="LICENSE">AGPL-3.0</a>
</p>

---

Kurbis is the hosting platform behind [kurbis.host](https://kurbis.host). Every
account gets one server. A server costs nothing while nobody is on it: after
five idle minutes it saves, snapshots its world to object storage and gives its
memory back. The next player who joins its address wakes it. They wait in a
lobby while the world is restored and the server starts, then the game
transfers them to it by itself.

There's no sign-up form. Joining a free address with a verified Minecraft
account claims it and creates that player's account and server. Google or
Discord can be linked later to manage the server from the web panel.

## How it works

```
 Java / Bedrock client
          │  join  name.kurbis.host
          ▼
 ┌─────────────────┐  route, wake    ┌───────────────────┐
 │     Gateway     │ ──────────────▶ │   Control plane   │──▶ PostgreSQL
 │ hostname proxy, │                 │ accounts, servers │
 │ waiting lobby   │                 │ placement, locks  │
 └────────┬────────┘                 └─────────┬─────────┘
          │ transfer when ready                │ assignments
          ▼                                    ▼
 ┌─────────────────────────────────────────────────────┐
 │ Worker: runs isolated Pumpkin servers (Docker or    │──▶ S3-compatible
 │ native), restores and publishes world snapshots     │    object storage
 └─────────────────────────────────────────────────────┘
```

A server moves through
`sleeping → queued → restoring → starting → online → stopping → snapshotting → sleeping`.

- **Waking** takes the lifecycle lock, reserves worker capacity, restores the
  latest valid snapshot, starts Pumpkin, checks readiness, then publishes the route.
- **Sleeping** stops new routes, saves the world cleanly, validates a new
  snapshot, then releases the worker's resources.

Lifecycle operations hold a lock in PostgreSQL, so concurrent joins start a
server only once, and a lost worker's servers recover from their last snapshot
on another worker.

## Features

- **Wake on join, sleep when idle.** Java 26.3 hostname routing, a waiting
  lobby and native transfer to the backend once it's up.
- **Accounts from Minecraft identities.** Verified Java and Bedrock players
  register or resume an account just by joining. Google or Discord sign-in
  can be linked, and merging accounts keeps every world.
- **Web panel.** Lifecycle controls, settings, players and whitelist, logs,
  snapshots and plugins, with live updates over SSE.
- **In-game management.** OP-only `/kurbis` opens a chest-menu panel and can
  hand out a one-use login link scoped to that server.
- **Plugins.** Install free plugins from the
  [Pumpkin market](https://market.pumpkinmc.org/) or upload your own
  WebAssembly component. Changes load into a running server without a restart.
- **AI assistant (optional).** OP-only `/ai` runs a model through Anthropic's
  API or any OpenAI-compatible endpoint, with per-server and per-account
  monthly budgets.
- **Pumpkin previews (optional).** Every open pull request on Pumpkin gets a
  joinable preview address. Its code is compiled in a sandbox with no network.
- **Isolation.** Rootless Docker per server, an internal-only network, and a
  firewall that admits only the gateway.

## Repository layout

| Path | What's there |
| --- | --- |
| [`src/`](src) | Rust: control plane, gateway, worker, snapshots, CI previews (binaries in [`src/bin`](src/bin)) |
| [`migrations/`](migrations) | PostgreSQL schema |
| [`web/`](web) | Next.js web panel and the landing page |
| [`plugins/kurbis-control/`](plugins/kurbis-control) | The in-game `/kurbis` and `/ai` plugin (WASI component) |
| [`runtime/`](runtime) | Container images, the Pumpkin integration patches, the CI builder |
| [`tests/`](tests) | Rust integration tests against a real PostgreSQL |
| [`tools/`](tools), [`scripts/`](scripts) | Build, verification and deployment helpers |
| [`contracts/openapi.json`](contracts/openapi.json) | The HTTP API, generated from the Rust router |
| [`docs/`](docs) | Development, self-hosting, configuration and design notes |

## Quick start

You need Rust 1.98.1, Node.js with pnpm 11.2.2, and PostgreSQL. Docker is
only needed for container deployments; local development runs on macOS or
Linux directly.

**Just the web panel and landing page**, with no backend:

```bash
cd web
pnpm install --frozen-lockfile
pnpm run dev
```

Then open http://localhost:3000. The panel itself (`/app`) needs the control plane.

**The whole stack** in a tmux dashboard (also needs tmux, Git and CMake). The
first run writes a private `.env` for you to fill in:

```bash
./start.sh native debug
```

[Development](docs/development.md) covers running the control plane against a
local OAuth fixture, native workers and the test suites.
[Self-hosting](docs/self-hosting.md) covers the Docker deployment, TLS,
firewalling and object storage.

## Tests

```bash
TEST_DATABASE_URL=postgres://localhost/kurbis_test \
  tools/run_bounded_build.py --memory-bytes 10737418240 -- \
  cargo test --locked --all-targets -- --test-threads=1
```

Point it at a disposable database; the tests bootstrap their own schema in
it. The browser suite runs the real panel, API and PostgreSQL in Playwright; see
[its README](web/tests/e2e/README.md).

## Documentation

- [Development](docs/development.md): local stack, builds, tests
- [Self-hosting](docs/self-hosting.md): deployment modes, networking, TLS, releases
- [Configuration](docs/configuration.md): OAuth providers, the AI assistant
- [Pumpkin previews](docs/pumpkin-previews.md): the CI preview service
- [Native host development](runtime/pumpkin/NATIVE.md) and the [hosted Pumpkin runtime](runtime/pumpkin/README.md)
- [Control-plane workers](docs/control-plane-workers.md): background tasks and supervision

## Contributing

Issues and pull requests are welcome. Run the tests above before opening a
pull request, and keep changes focused. By contributing you agree that your
contribution is licensed under the AGPL-3.0.

## License

Kurbis is free software under the [GNU Affero General Public License v3.0](LICENSE).
If you run a modified version as a service, you must offer its users the
source code of your version.

The Pumpkin integration in [`runtime/pumpkin/`](runtime/pumpkin) modifies
Pumpkin and stays under Pumpkin's
[GPL-3.0](runtime/pumpkin/LICENSE).

Kurbis is not an official Minecraft or PumpkinMC product. It is not approved by
or associated with Mojang or Microsoft.
