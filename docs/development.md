# Development

Local development runs directly on macOS or Linux. See [native setup](../runtime/pumpkin/NATIVE.md) for workers and the waiting gateway runtime. On macOS, workers default to the native backend.

Build the Rust project through host-wide memory admission (10 GiB build cap,
4 GiB reserved for the OS/services):

```bash
tools/run_bounded_build.py --memory-bytes 10737418240 -- cargo build
```

Run Rust tests through the same admission gate:

```bash
TEST_DATABASE_URL=postgres://localhost/melon_hoster_test \
  tools/run_bounded_build.py --memory-bytes 10737418240 -- \
  cargo test --locked --all-targets -- --test-threads=1
```

Use `tools/docker_build_with_limits.sh` rather than raw `docker buildx build` or
`docker compose build`; it serializes independent build clients, samples memory
on the Docker daemon host, reserves 4 GiB of headroom, and verifies BuildKit's
per-RUN cgroup memory/swap/CPU caps under controlled pressure before building.

Use a disposable local database. The schema is bootstrapped from the canonical
empty-database definition; historical migration fixtures and upgrade
compatibility are not supported.

## Frontend-only preview

To preview the marketing landing page locally without PostgreSQL, the control
plane, workers, or OAuth credentials:

```bash
cd web
pnpm install --frozen-lockfile
bun run dev # or pnpm run dev
```

Open `http://localhost:3000`. The landing page, its interactive wake/sleep demo,
and `/login` render without backend requests. `/` always shows the marketing
page, including for signed-in visitors; use `/app` to open the management panel.
Actual sign-in, management routes, and API requests still require the control
plane. Run the web command from `web/`, not the Rust repository root.

## Full local stack

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

To test real Google and Discord sign-in locally, register these exact local callback URLs in the provider consoles:

```text
http://localhost:3000/auth/google/callback
http://localhost:3000/auth/discord/callback
```

Put both client IDs and secrets in the ignored `web/.env.local` file. Remove the provider-specific `*_OAUTH_AUTHORIZE_URL`, `*_OAUTH_TOKEN_URL`, and `*_OAUTH_USERINFO_URL` overrides so the control plane uses Google's and Discord's production OAuth endpoints. For this mode, run the control plane and panel without `examples/browser_fixture.rs`.

## Tests and runtime builds

The local implementation is covered by Rust/Postgres, browser, and real-protocol integration tests; Linux hard-quota and production-provider checks are called out separately where they cannot be established by local fixtures.

Browser lifecycle and OAuth verification uses a disposable database and real panel/API/Postgres, with controlled external provider and worker boundaries. Run it using [the E2E instructions](../web/tests/e2e/README.md).

Build local hoster binaries with `tools/run_bounded_build.py --memory-bytes 10737418240 -- cargo build --bins`. Native workers use the local host-control binary produced by [the native build script](../runtime/pumpkin/NATIVE.md); they do not need Docker. Docker workers require the source-built Pumpkin image containing the host-control interface. Use `tools/docker_build_with_limits.sh --file runtime/Dockerfile --tag melon-hoster:local .` rather than an unbounded Compose/Docker build; [runtime build instructions](../runtime/pumpkin/README.md) pin the upstream revision and include the complete integration source.

To verify authenticated host-control behavior against a built Pumpkin binary, run `python3 tools/verify_host_control.py --binary /absolute/path/to/pumpkin`. This launches an actual disposable server and checks Java 26.3 login, save/shutdown, and player activity. The native lifecycle harness also exercises Java waiting sessions, transfers, and cross-worker world recovery; it does not establish Linux hard-quota behavior.

For faster development on macOS or Linux, use [native host execution](../runtime/pumpkin/NATIVE.md). Build with `tools/build_pumpkin_native.sh` and select `WORKER_RUNTIME=native`; the shared lifecycle and snapshot system works without Docker. Native development mode has no hard memory or filesystem isolation.
