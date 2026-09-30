# Runtime verification

The full runtime harness supports native host execution for local development
and Docker for Linux-only hard-quota verification. Native execution is the
recommended development path on this Mac; Docker is not required to build or
run the control plane, gateway, panel, or native worker.

## Native development and integration verification

Build the pinned Pumpkin host-control binary and run the Java + Bedrock
end-to-end lifecycle suite against a disposable local PostgreSQL database:

```sh
tools/build_pumpkin_native.sh
npm --prefix tools/protocol ci --ignore-scripts
createdb melon_hoster_native_e2e
cargo build --bins
python3 tools/verify_worker_runtime.py --bin-dir target/debug \
  --native-binary output/native-pumpkin/target/debug/pumpkin \
  --database-url postgres://localhost/melon_hoster_native_e2e \
  --protocol-dir tools/protocol --editions both \
  --output output/runtime/native-lifecycle
```

The harness uses actual Java and Bedrock protocol clients with disposable test
identities. It exercises routing, queueing, wake, transfer, snapshots, recovery,
and worker fencing. Read `PROGRESS.md` for the latest run evidence. Native runs
cannot establish Linux cgroup memory enforcement or hard filesystem quotas.

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

Real Java and JavaScript RakNet Bedrock clients are included. The wrapper derives a disposable offline-authentication test image without changing production authentication. Protocol checks cover unknown hostnames, online routing and sleeping wake/native transfer/backend login. Java commands create nonempty inventory and changed world blocks, then check them after cross-worker recovery and clean shutdown. Every client writes event/result JSON.

The harness also includes simultaneous Java/Bedrock capacity waiting on a second hostname and a real five-minute idle/rejoin interval. Read `PROGRESS.md` for executed evidence. Native mode runs these lifecycle scenarios without hard quota checks; see `../pumpkin/NATIVE.md`. Syntax validation alone is not evidence that mounts, quotas, or gameplay passed.
