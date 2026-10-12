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

The active CI watcher is allowlisted to `Pumpkin-MC/Pumpkin`; configure
`CI_REPOSITORY` with that exact owner/repository or startup fails closed.
Pull-request heads may still come from forks discovered by the allowlisted
repository. Those fork archives are fetched by immutable commit SHA with the
same path and response bounds, while branches and full commit aliases stay
lazy until an explicit preview request.

The image's cargo cache is a bootstrap cache for the pinned upstream lockfiles.
If a Pumpkin change adds dependencies not in that cache, its build fails closed
while offline. Updating this image cache is an operator-controlled image rebuild
and must not be triggered by source-controlled build files.

## Compiler cache and BuildKit reuse

The host-owned builder image includes the pinned `sccache` release
(`SCCACHE_VERSION`, default `0.10.0`). Runtime containers mount a rootless-Docker
named volume whose name is derived from the configured builder host and
set `SCCACHE_DIR` to its private `/cache/sccache` directory. The cache is local
only: no sccache remote backend, credentials, or network access is available in
the build namespace. `SCCACHE_CACHE_SIZE` is bounded by
`CI_BUILD_CACHE_MAX_BYTES` (default 8 GiB; accepted range 256 MiB to 64 GiB),
and a failed/corrupt sccache invocation retries the exact Cargo command once
without the wrapper. The normal ELF/Wasm validators still gate all outputs.

Compose forwards `CI_BUILD_CACHE_DIR` and `CI_BUILD_CACHE_MAX_BYTES` to the
control plane and mounts the private named volume `ci-build-cache` at the
image-prepared mode-0700 path. The volume survives control-plane restarts and
is not shared with runtime workers. Native `start.sh` deployments should set
`CI_BUILD_CACHE_DIR` to an equivalent private path below `HOME`.

Set `CI_BUILD_CACHE_DIR` to a mode-0700 directory below the control-plane
`HOME` to enable the optional exact-output cache. `CI_BUILD_CACHE_MAX_BYTES`
applies to both this cache and the compiler cache. Each entry is keyed by the
cache schema, repository, commit, patch fingerprint, target, patched-source
digest, trusted-plugin digest, and immutable builder identity. Entries are
written through a per-key create-exclusive lock and atomic rename, pruned by
oldest modification time at the configured bound, and deleted on metadata or
digest mismatch. Cache hits are restored only after the normal output validators
run; this cache is never trusted as executable source code.

The image build itself should continue to use `tools/docker_build_with_limits.sh`
(BuildKit/buildx). Its layer cache may be retained by the selected BuildKit
builder, but it is not used as a runtime trust boundary. Rebuild the image when
`SCCACHE_VERSION`, the Rust release, the Pumpkin revision, or the host-owned
Dockerfile changes; the sccache key uses the compiler identity and complete rustc invocation, so
changes to the toolchain, profiles, source, and patches naturally miss old
entries within the same bounded host cache.

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
contract above. `CI_BUILDER_BACKEND=bubblewrap` is an explicit opt-in for a Linux native
control plane with private `CI_BUBBLEWRAP_*` paths. Bubblewrap is deliberately
reported as `bubblewrap_namespace_only`: it does not claim cgroup CPU, memory,
or PID enforcement, and there is no automatic Docker fallback. Bubblewrap is
not supported by the Docker Compose deployment: Compose intentionally does not
pass or mount the host executable, immutable rootfs/toolchain, source preparer,
or workspace. Selecting Bubblewrap in Compose therefore fails closed instead
of pretending those host paths exist. Use `./start.sh native ...` on the Linux
worker and configure every `CI_BUBBLEWRAP_*` path there. Native `start.sh` runs
from the checkout root, so set `CI_PLUGIN_SOURCE=plugins/kurbis-control` (or an
absolute checkout path); Compose overrides that setting with the image path
`/opt/kurbis/plugins/kurbis-control`.

The operator must mount the workspace below the build user's private `$HOME`,
for example `$HOME/.cache/kurbis/ci/workspace-tmpfs`, with exactly
`size=5G,nr_inodes=1310720`. Startup and every workload preflight parse
`/proc/self/mountinfo` and fail closed for an ordinary directory, a different
size, or a different inode limit. Compile and patch workloads always use this
fixed 5 GiB workspace contract; any custom disk limit is rejected. Build
input/output files remain separately bounded and are copied with counted reads.
The native `bubblewrap` backend, Docker backend, and remote `bubblewrap_ssh`
transport use `CI_BUILD_CONCURRENCY` (bounded to 1-64) for cross-process
PostgreSQL slot admission; each slot reserves its requested memory plus host
system headroom. The SSH transport samples `/proc/meminfo` on lxhalle through
the pinned SSH connection and fences stale per-job workspaces by lease token,
so independent control-plane processes share the same admission identity.
This is coordination and cleanup, not kernel cgroup enforcement: remote
namespace-only builds still must not be described as memory-, CPU-, or PID-
enforced.

## Remote Bubblewrap mode (respond5 watcher → lxhalle builder)

A control plane running in the isolated rootless respond5 stack can set
`CI_BUILDER_BACKEND=bubblewrap_ssh`. Set `CI_GITHUB_API_BASE_URL` only when using a loopback fixture; production defaults to `https://api.github.com`. In this mode the control plane remains the
poller, Check Run client, artifact cache, and preview coordinator. It sends one
framed JSON-plus-archive job over a pinned non-root SSH account to the checked-in
`tools/ci_bubblewrap_runner.py` on lxhalle. The runner enters a private user and
mount namespace, mounts the operator workspace as an exact 5 GiB/1,310,720-inode
tmpfs below that account's HOME, and runs the pinned Bubblewrap build with
read-only root/toolchain inputs. A compile job has two phases. The fetch phase
has network access but executes no fetched code: it unpacks the archives, runs
`cargo fetch --locked` into `compiler-cache/cargo-home`, and the runner
downloads the SHA-1-checked Minecraft server jar that upstream's
`pumpkin-data` build script unpacks into `compiler-cache/minecraft`. The build
phase then compiles `--offline` in a namespace with no network. The rootfs
must carry empty `etc/resolv.conf` and `etc/ssl/certs/ca-certificates.crt`
files as mount points for the fetch phase. It returns only bounded build logs
and SHA-256-labelled artifacts; the respond5 coordinator verifies each stream
before the normal BLAKE3 artifact-cache verification and runtime registration.

The SSH transport must use a dedicated non-root key, `BatchMode=yes`,
`IdentitiesOnly=yes`, and strict host-key verification from the control-plane
user's private `known_hosts`; unknown or changed host keys fail closed. Do not
mount `/var/run/docker.sock`, use rootful Compose, or send GitHub credentials to
the builder. `CI_BUILDER_SSH_TARGET` is a single host target, and all
`CI_BUBBLEWRAP_*` paths are remote builder paths. The runner's namespace
preflight also performs a C compiler/sysroot link probe, so a runtime-only
rootfs (for example, one missing GCC `libexec`, linker startup objects, or C
headers) fails before source transfer/build admission. The helper removes its
unique workspace in a `finally` path after every job.

### Persistent operator configuration

The deployment must persist the operator values outside the checkout and inject
them through the service's real deployment environment. On lxhalle, keep the
non-secret configuration at `$HOME/.config/kurbis/ci-bubblewrap/env` (mode `0600`)
and keep immutable fixtures on a user-owned local filesystem, for example
`/var/tmp/kurbis-ci-bubblewrap-20261007` (mode `0700`). The accepted fixture
uses `/usr/bin/bwrap`, a rootfs and Rust 1.98.1 toolchain under that staging
root, the checked-in source preparer, and the private HOME workspace
`$HOME/.cache/kurbis/ci/workspace-tmpfs`. The Bubblewrap and SSH backends derive
the persistent compiler cache from its sibling
`$HOME/.cache/kurbis/ci/compiler-cache`; keep that directory mode 0700 on local
operator-owned storage, not public `/tmp`. `$HOME` on lxhalle is NFS; therefore
the workspace path is only the operator namespace anchor. The runner mounts an
exact `size=5G,nr_inodes=1310720,mode=700` tmpfs over each per-job workspace
inside its private user/mount namespace. Do not place source, artifacts, or
Cargo state in bare public `/tmp`; any local staging path must be a dedicated
mode-0700 operator directory with private files. The `/var/tmp` path above is
only that private immutable fixture root, never the host-wide directory itself.

On respond5, store the deployment-only SSH material at
`/etc/dokploy/secrets/kurbis-hoster/ci-builder-ssh` with directory mode `0700`,
files mode `0600`, and ownership `65532:65532` so the unprivileged
control-plane can read it after a read-only bind mount. The directory contains a
dedicated `id_ed25519`, a `config` with `IdentitiesOnly=yes`,
`BatchMode=yes`, and `StrictHostKeyChecking=yes`, and a `known_hosts` file
containing only the pinned lxhalle Ed25519 key. The corresponding public key is
installed in the `ci` account with forwarding and TTY disabled. Set
`CI_BUILDER_SSH_HOST_DIRECTORY` to this host path and set the
`CI_BUBBLEWRAP_*` variables to the absolute lxhalle paths above in Dokploy's
service environment; never commit either the private key or a PAT.

This is deliberately **namespace-only**. The preflight proves user/PID/mount/IPC/
UTS/network isolation, read-only root and toolchain inputs, dropped
capabilities, `NoNewPrivs`, no route, and the exact namespace-local workspace
tmpfs. lxhalle does not delegate per-build cgroup controllers, so this backend
must not claim hard CPU, memory, swap, or PID limits. Cross-process admission
and process timeouts remain coordination controls, not kernel cgroup quotas.
If any immutable path, pinned host key, workspace contract, or toolchain probe
is missing, startup fails closed; it never falls back to Docker or rootful
execution.

To roll back only this operator setup, stop the deployment before unmounting
its read-only SSH directory, then remove the named lxhalle fixture/config paths
and the respond5 secret directory. Do not remove unrelated Docker resources or
edit `/etc/docker`, system services, or rootful production sockets.
