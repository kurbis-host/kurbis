# Rootless CI build image

The CI coordinator compiles fetched Pumpkin commits in a rootless, network-disabled
container. The commit's Dockerfile and build scripts are never used. The trusted
builder image supplies the exact Rust toolchain, native build tools, Wasm target,
and cargo registry sources for the pinned upstream baseline and trusted Kurbis
control plugin. Runtime cargo commands use `--offline`.

Build and push this host-owned image from the repository root on a trusted Linux
builder. Pin `RUST_TOOLCHAIN` to the exact Rust release that the image must use:

```sh
docker buildx build \
  --build-arg RUST_TOOLCHAIN=1.98.1 \
  --build-arg PUMPKIN_REVISION="$(cat runtime/pumpkin/REVISION)" \
  -f runtime/ci/builder.Dockerfile \
  -t registry.example/kurbis/pumpkin-ci-builder:1.98.1 \
  --push .
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
Docker logs persist on the host. Compilation is serialized, with a 10 GiB
memory cap, release LTO disabled, and 16 codegen units. Provision the dedicated
builder with at least 16 GiB RAM so its host and Docker daemon retain headroom. The Kurbis
startup probe rejects builders whose cgroup v2, user namespace, CPU/memory/PID
controls, no-egress policy, read-only image root, or bounded workspace tmpfs do
not pass disposable workload tests. Builder workspaces use a per-container
tmpfs cap sized to the configured build disk budget; input, `/tmp`, and
`/dev/shm` caps are deducted from that same budget. This avoids reliance on
rootless Docker's unsupported overlay `--storage-opt` while preserving a hard
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
