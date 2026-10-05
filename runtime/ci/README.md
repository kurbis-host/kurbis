# Rootless CI build image

The CI coordinator compiles fetched Pumpkin commits in a rootless, network-disabled
container. The commit's Dockerfile and build scripts are never used. The trusted
builder image supplies the exact Rust toolchain, native build tools, Wasm target,
and cargo registry sources for the pinned upstream baseline and trusted Kurbis
control plugin. Runtime cargo commands use `--offline`.

Build and push this host-owned image from the repository root on a trusted Linux
builder. Pin `RUST_TOOLCHAIN` to the exact Rust release that the image must use:

```sh
KURBIS_BUILD_MEMORY_BYTES=10737418240 \
KURBIS_BUILD_SYSTEM_RESERVE_BYTES=4294967296 \
KURBIS_BUILD_OUTPUT_MODE=push \
tools/docker_build_with_limits.sh \
  --platform linux/amd64 \
  --build-arg RUST_TOOLCHAIN=1.98.1 \
  --build-arg PUMPKIN_REVISION="$(cat runtime/pumpkin/REVISION)" \
  --file runtime/ci/builder.Dockerfile \
  --tag registry.example/kurbis/pumpkin-ci-builder:1.98.1 \
  .
```

Take the resulting manifest digest from the registry and configure the control
plane with:

```text
CI_GITHUB_PAT=...
CI_BUILDER_IMAGE=registry.example/kurbis/pumpkin-ci-builder@sha256:<manifest-digest>
CI_TOOLCHAIN_VERSION=1.98.1
CI_BUILDER_DOCKER_HOST=ssh://ci-builder@builder.example.net
```

The image digest is part of the build cache identity. Changing the builder image
or Rust release creates distinct builds, even for the same Pumpkin commit. The
control plane intentionally rejects mutable image tags and unsupported native
Linux architectures. CI source fetching and the Cargo cache seed happen only at
trusted image-build time; build containers have no outbound network access.

The image's cargo cache is a bootstrap cache for the pinned upstream lockfiles.
If a Pumpkin change adds dependencies not in that cache, its build fails closed
while offline. Updating this image cache is an operator-controlled image rebuild
and must not be triggered by source-controlled build files.

## Builder host and control-plane topology

The CI control-plane container does not receive a host Docker socket. It uses
Docker's SSH transport to one dedicated Linux account running a rootless Docker
daemon. Provision the dedicated builder host with enough disk for the pinned toolchain
image and rootless Docker data root. Per-build source, target, and temporary scratch
are volatile bounded tmpfs mounts. The pre-seeded Cargo package/git cache remains
in the immutable image and is read-only to workloads; only the image and bounded
Docker logs persist on the host.

Every CI patch/compile phase, across control-plane processes and coordinated jobs,
first takes one session-scoped PostgreSQL advisory lock. The coordinator also samples
`MemAvailable` on the builder host (not the control-plane machine) and starts a build
only when the requested container memory plus a fixed 4 GiB system/runtime reserve
fits in current headroom. Admission waits at most 10 minutes and then fails closed
with the last measured host-memory figures; it never raises the configured container
limit to work around low headroom. If a coordinator is cancelled or crashes, its
PostgreSQL session releases the lock. The next owner reclaims only CI build containers
carrying Kurbis's build label before admission. A dropped lease also schedules
immediate removal of its lease-owned containers.

The Rust compiler runs with one Cargo job and each rootless task container has a hard
10 GiB memory cap, an equal memory+swap cap (zero swap allowance), 2 CPU, 512 PIDs,
a 90-minute process timeout, read-only root, no network, and bounded tmpfs scratch.
Release LTO is disabled and release codegen units are forced to one to avoid
intra-crate codegen fan-out on top of the single Cargo job. Container failure diagnostics
include Docker's `OOMKilled` state plus cgroup-v2 `memory.max`, `memory.swap.max`,
`memory.current`, `memory.peak`, and `memory.events`, so a cgroup OOM can be
separated from a host-memory admission deferral. Startup also runs a disposable
32 MiB pressure probe and requires the cgroup `oom_kill` counter to increase under
controlled pressure; a successful limit-shaped Docker command without an observed
kernel enforcement event is not accepted.

Provision the dedicated builder with at least 16 GiB RAM so its host and Docker
daemon retain headroom. The Kurbis startup probe rejects builders whose cgroup v2,
user namespace, CPU/memory/PID controls, no-egress policy, read-only image root, or
bounded workspace tmpfs do not pass disposable workload tests. Builder workspaces
use a per-container tmpfs cap sized to the configured build disk budget; input,
`/tmp`, and `/dev/shm` caps are deducted from that same budget. This avoids reliance
on rootless Docker's unsupported overlay `--storage-opt` while preserving a hard
bound on writable build storage.

Use a dedicated SSH key with access only to this rootless builder account. In the
repository's ignored `deployment/ci-builder-ssh/` directory, provide an OpenSSH
`config`, the matching private key (mode `0600`), and a pinned `known_hosts`
file. The `control-plane` service mounts this directory read-only at its
unprivileged home. Ensure the mounted directory and key are readable by the
container's UID 65532 (for example, set host ownership to `65532:65532` and the
directory mode to `0700`). Do not mount `/var/run/docker.sock` into the control
plane or worker. The remote Docker host is configured with
`CI_BUILDER_DOCKER_HOST`; it must resolve to a local Unix socket on the builder,
never a TCP Docker API.

The builder data root itself still needs a host-level capacity limit because
images and logs persist on the dedicated builder disk. Build task writable
storage is separately capped by read-only image roots plus bounded, per-task
tmpfs mounts; do not replace those caps with an unbounded writable layer.

The immutable builder container uses no network, no source-controlled Dockerfile,
and no source-controlled build scripts. A small trusted preprocessor binary,
built into the pinned builder image from this checkout and the checked-in
patches, validates/extracts/patches source archives inside the rootless,
quota-bounded workload before compilation. The trusted plugin source is copied
from the image build context into the control-plane image; it is not accepted
from the Pumpkin commit.

## Preview world snapshots and worker loss

CI previews now publish full 72-hour world checkpoints into the existing object
store under a preview-specific `ci-previews/<preview-id>/worlds/` namespace.
A small `ci_world_snapshots` row stores the latest verified size, checksum, and
source runtime generation; it is separate from account-owned `snapshots` and
`servers`. The control plane requests periodic checkpoints every ten minutes,
and an idle stop saves and publishes one before releasing active capacity. The
managed plugin's `snapshot` command requests the same worker-authenticated
checkpoint path. Archive packing excludes generated `pumpkin.toml`, host-control
sockets, the managed plugin binary, and its capability-bearing config; these are
regenerated from the trusted runtime assignment on start.

If a worker heartbeat expires, the runtime may be assigned to a compatible
worker only if a complete shared checkpoint exists. The replacement downloads
and verifies the snapshot digest before extracting into its quota volume. A
worker without an assignment removes its orphaned local preview world on
reconciliation. If no checkpoint exists, the system fails closed rather than
silently starting an empty world on a different host. Expired-preview cleanup
removes the checkpoint object and metadata along with the generation-fenced
worker world, port leases, and resource reservations.

## Builder backend and concurrency

`CI_BUILDER_BACKEND=docker` is the default and keeps the rootless Docker
contract above. `CI_BUILDER_BACKEND=bubblewrap` is an explicit opt-in for a
Linux worker with private `CI_BUBBLEWRAP_*` paths. Bubblewrap is deliberately
reported as `bubblewrap_namespace_only`: it does not claim cgroup CPU, memory,
or PID enforcement, and there is no automatic Docker fallback. Each workload
must pass a real kernel-enforced, size-bounded `/work` tmpfs mount; if the
configured Bubblewrap cannot create that mount, preflight fails closed. The backend does not set an independent per-build inode quota; the operator
tmpfs mount must declare a non-zero `nr_inodes=` ceiling. Build input/output files remain separately bounded and are
copied with counted reads. Both backends use `CI_BUILD_CONCURRENCY` (bounded to
1-64) for cross-process PostgreSQL slot admission; each slot reserves its
requested memory plus host system headroom.
