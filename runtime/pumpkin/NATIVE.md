# Native host development

The native backend runs the same pinned and patched Pumpkin directly on macOS
or Linux, without Docker. Native mode is never selected implicitly: a debug
worker must explicitly set `WORKER_RUNTIME=native` and
`KURBIS_ALLOW_UNSANDBOXED_NATIVE_DEV=true`. Release workers reject native mode.
PostgreSQL, lifecycle coordination, snapshot verification, idle policy,
waiting clients and generation fencing are shared with Docker workers.

Build locally with Rust 1.98.1, Git, CMake and a C/C++ compiler installed.
The build script accepts `debug` or `release` (default `debug`):

```sh
tools/build_pumpkin_native.sh debug
tools/build_pumpkin_native.sh release
tools/run_bounded_build.py --memory-bytes 10737418240 -- cargo build --bins
export DATABASE_URL=postgres://localhost/melon_hoster
export KURBIS_ALLOW_UNSANDBOXED_NATIVE_DEV=true
. examples/config/native-worker.env.example
# Replace the example token before sharing this development environment.
target/debug/worker
```

The build script manages the native Pumpkin checkout under output/native-pumpkin/source and builds the first-party Kurbis WASI plugin. It applies the checked-in host-control integration and reuses native Cargo artifacts. PUMPKIN_BUILD_JOBS defaults to 4. The worker example supplies the control-plane URL, public panel URL, and internal token; keep the internal token out of server-container environment variables.

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
python3 tools/verify_worker_runtime.py --bin-dir target/debug \
  --native-binary output/native-pumpkin/target/debug/pumpkin \
  --plugin-wasm plugins/kurbis-control/target/wasm32-wasip2/debug/kurbis_control.wasm \
  --database-url postgres://localhost/melon_native_runtime_test \
  --protocol-dir tools/protocol --output output/runtime/native-lifecycle
```

This harness uses an explicit offline identity fixture only for its disposable servers. It verifies the managed plugin load, OP-only login command, same-server link redemption, replay and origin checks, account/cross-server isolation, and expired-session rejection. It also verifies that unverified players receive no account login link. Results are written to result.json; check passed and individual checks. Native success cannot establish Docker hard-quota enforcement.

## Measure a server start

`tools/measure_server_start.py` brings up the control plane, gateway, waiting
runtime and one native worker against an empty disposable database, joins a
sleeping server through the gateway with the Java protocol client, and prints
where the time between connecting and spawning went:

```sh
createdb kurbis_start_timing
python3 tools/measure_server_start.py --bin-dir target/debug \
  --native-binary output/native-pumpkin/target/release/pumpkin \
  --database-url postgres://localhost/kurbis_start_timing
```

The first run generates a new world; the median it prints covers the later
runs, which restore a snapshot the way a real wake does. Docker-only steps do
not run in native mode. Every worker logs the same split for each start it
performs (`server start completed`, and `Docker runtime started` on Docker
workers), so production numbers come from the worker's log.

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
export BEDROCK_PUBLIC_PORT=19132
export WAITING_SIGNAL_ADDRESS=127.0.0.1:19134
export BEDROCK_TLS_CERT_FILE=/absolute/path/to/mc.example.test.crt
export BEDROCK_TLS_KEY_FILE=/absolute/path/to/mc.example.test.key
export PUMPKIN_NETHERNET_EXTERNAL_IP=127.0.0.1
export PUMPKIN_NETHERNET_EXTERNAL_PORT=19132
# DATABASE_URL, ROOT_DOMAIN and INTERNAL_TOKEN match the control plane.
tools/run_native_waiting.sh
```

The certificate must cover the configured root domain and its server
subdomains. The Gateway process needs read access to both PEM files; keep the
private key owner-only. For remote clients, advertise the actual public UDP
address instead of loopback and provide a certificate trusted for the same
hostnames.

In another terminal with those same variables, use the bounded wrapper if a gateway binary build is needed: `tools/run_bounded_build.py --memory-bytes 10737418240 -- cargo run --bin gateway`.
Run the worker in a third terminal from the native worker example above.
The default waiting ports are 25566/TCP and 19133/TCP+UDP; hosted backend ports
come from the worker’s distinct assigned range. Set wildcard DNS or local
hostnames for your configured root domain to loopback. Java 26.3 is supported.
Bedrock signaling TLS is terminated by the Gateway; its private plaintext
Pumpkin listener is reachable only on the waiting/worker signal interface.
The automated runtime harness covers HTTPS signaling and UDP relay behavior.
The physical Bedrock client was intentionally not exercised at the user's
direction; this is not an unresolved environment prerequisite.

Stop hosted worlds through the panel before shutting down the worker. Native
Pumpkin worlds intentionally survive worker exit for crash recovery. Stop the
waiting runtime with Ctrl-C. Development does not require starting Docker Desktop.
