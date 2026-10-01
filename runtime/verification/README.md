# Runtime verification

The hosted runtime runs natively for local development. The isolated Linux
hard-quota harness uses Docker only for its separate container-limit checks.

## Native development and integration verification

Build the pinned Pumpkin host-control binary and run the Java 26.3 end-to-end
lifecycle suite against a disposable local PostgreSQL database:

```sh
tools/build_pumpkin_native.sh
createdb melon_hoster_native_e2e
cargo build --bins
python3 tools/verify_worker_runtime.py --bin-dir target/debug \
  --native-binary output/native-pumpkin/target/debug/pumpkin \
  --database-url postgres://localhost/melon_hoster_native_e2e \
  --protocol-dir tools/protocol \
  --output output/runtime/native-lifecycle
```

The harness uses a real Java 26.3 client with a disposable offline identity. It
exercises routing, queueing, wake, transfer, snapshots, recovery and worker
fencing. Read `PROGRESS.md` for
the latest run evidence. Native runs cannot establish Linux cgroup memory
enforcement or hard filesystem quotas.

## Linux hard-quota verification

The following path builds and runs the isolated Linux runtime on a local Docker
Desktop engine. It is separate from native development and is required only to
verify the production container's memory and disk isolation:

Build the two runtime images on the local Docker Desktop engine:

```sh
docker build -f runtime/Dockerfile -t melon-hoster:local .
docker build -f runtime/pumpkin/Dockerfile -t melon-pumpkin:host-control .
sh tools/verify_local_runtime.sh
```

Large Pumpkin builds should use the dedicated `melon-hoster-local` BuildKit builder with `--load`; it is capped at 6 GiB RAM and two CPUs. Run image builds sequentially to avoid oversubscribing Docker Desktop.

The wrapper refuses a remote Docker context and runs a privileged disposable Linux container with its own inner Docker daemon. Privilege is required for the worker's ext4 loop mounts and real nested cgroup limits. No host Docker socket or existing Docker data volume is passed to it. The inner engine uses VFS for portability through Docker Desktop.

The harness starts actual control-plane and worker binaries, real Postgres, and the patched Pumpkin container. It exercises startup, hard disk/memory pressure, periodic snapshot upload outage/recovery, worker-loss fencing, partial-generation rejection, cross-worker recovery, final save/shutdown and resume. Artifacts remain under `output/runtime/`. The wrapper removes only its own verification container and anonymous volumes.

The wrapper derives a disposable offline-authentication test image without
changing production authentication. Java protocol checks cover unknown
hostnames, online routing and sleeping wake/native transfer/backend login.
Commands create inventory and changed world blocks, which are checked after
cross-worker recovery and clean shutdown. Bedrock 1.26.51 login is blocked by
the pinned upstream NetherNet TLS signaling issue documented in
`../pumpkin/README.md`.

The harness includes Java capacity waiting on a second hostname and a real
five-minute idle/rejoin interval. Read `PROGRESS.md` for executed evidence.
Native mode runs these lifecycle scenarios without hard quota checks; see
`../pumpkin/NATIVE.md`. Syntax validation alone is not evidence that mounts,
quotas or gameplay passed.
