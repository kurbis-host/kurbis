# Native host development

The native backend runs the same pinned and patched Pumpkin directly on macOS
or Linux, without Docker. macOS workers default to `WORKER_RUNTIME=native`;
Linux workers can select it explicitly. PostgreSQL, lifecycle coordination, snapshot verification, idle policy,
waiting clients and generation fencing are shared with Docker workers.

Build locally with Rust 1.98.1, Git, CMake and a C/C++ compiler installed.
The build script accepts `debug` or `release` (default `debug`):

```sh
tools/build_pumpkin_native.sh debug
tools/build_pumpkin_native.sh release
cargo build --bins
export DATABASE_URL=postgres://localhost/melon_hoster
. examples/config/native-worker.env.example
# Replace the example token before sharing this development environment.
target/debug/worker
```

The build script manages only `output/native-pumpkin/source`, initializes the
pinned WIT submodule, applies the checked-in integration, and reuses native Cargo
artifacts. `PUMPKIN_BUILD_JOBS` defaults to 4. `PUMPKIN_BINARY` must point to this
host-control build; an ordinary upstream binary cannot satisfy readiness.

Each worker needs a stable unique `WORKER_ID`, independent `WORKER_ROOT`, and
nonoverlapping port range. If omitted, the native root is
`data/native-worker-<WORKER_ID>`. Server directories retain process generation,
PID and random argv identity under `run/`. A restarted worker recovers matching
processes; it never treats an unrelated reused PID as its runtime. Processes
survive worker termination, so stop servers through the panel before discarding
a development root. The worker retains placement reservations until it confirms
that the previous runtime is inactive.

Unix sockets live under `/tmp/kurbis-native-<worker ID>` with owner-only directory
permissions, avoiding macOS socket-path limits. Server logs are collected from
`run/native.log` with a persistent cursor. Runtime metadata/logs/configuration are
excluded from snapshots; the new worker regenerates its own ports and control
socket. Query and RCON listeners are disabled to prevent unassigned host ports.

Native processes run with the current user's privileges. Capacity reservations
and the 5 GiB storage admission check still apply, but directory storage and host
processes have **no hard filesystem or memory isolation**. Use Docker with
loopback ext4 for the plan's enforced production quotas. Native mode is intended
for trusted local development, with authentication enabled by default.

For integrated real-client verification, create a fresh disposable PostgreSQL
database and run:

```sh
npm --prefix tools/protocol ci --ignore-scripts
python3 tools/verify_worker_runtime.py --bin-dir target/debug \
  --native-binary output/native-pumpkin/target/debug/pumpkin \
  --database-url postgres://localhost/melon_native_runtime_test \
  --protocol-dir tools/protocol --output output/runtime/native-lifecycle
```

This harness uses an explicit offline identity fixture only for its disposable
servers. It does not change normal native or Docker authentication. Results are
written to `result.json`; check `passed` and individual checks. Native success
cannot establish Docker hard-quota enforcement. See PROGRESS.md for executed
results and outstanding plan requirements.

## Run the gateway locally without Docker

Run the real control plane and panel using the README’s OAuth configuration.
Use native PostgreSQL and the local object-store backend from the worker example.
The waiting runtime uses normal Pumpkin authentication; it is not the offline
protocol-test fixture. Generate secrets once and reuse them in all terminals:

```sh
export PUMPKIN_GATEWAY_CONTROL_TOKEN=$(openssl rand -hex 32)
export WAITING_CONTROL_SOCKET="/tmp/kurbis-dev-waiting-$(id -u).sock"
export CONTROL_PLANE_URL=http://127.0.0.1:8080
export JAVA_LISTEN=127.0.0.1:25565
export BEDROCK_LISTEN=127.0.0.1:19132
# DATABASE_URL, ROOT_DOMAIN and INTERNAL_TOKEN match the control plane.
tools/run_native_waiting.sh
```

In another terminal with those same variables, run `cargo run --bin gateway`.
Run the worker in a third terminal from the native worker example above.
The default waiting ports are 25566/TCP and 19133/UDP; hosted backend ports
come from the worker’s distinct assigned range. Set wildcard DNS or local
hostnames for your configured root domain to loopback. Bedrock clients must
send that hostname in ServerAddress.

Stop hosted worlds through the panel before shutting down the worker. Native
Pumpkin worlds intentionally survive worker exit for crash recovery. Stop the
waiting runtime with Ctrl-C. Development does not require starting Docker Desktop.
