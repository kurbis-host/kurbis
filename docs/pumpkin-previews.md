# Pumpkin pull-request and branch previews

Set `CI_GITHUB_PAT` to enable the CI poller. The watcher is intentionally
allowlisted to `Pumpkin-MC/Pumpkin`; `CI_REPOSITORY` is accepted only when it
matches that exact owner/repository. The default branch is `master` (override it
only to follow another branch in that same allowlisted repository). Periodic
GitHub polling is the active event source and runs every 60 seconds with ETag
checkpoints. It always builds the latest default-branch head, and eagerly
builds and caches only the exact current PR heads that polling observes as
opened, reopened, or updated. Open pull requests that have not changed since
the previous poll get a preview address but are built on the first join. No PR
ancestors, unrelated commits, branches, or arbitrary commit aliases are
proactively built.

Kurbis writes nothing to the watched repository by default. To post a Check
Run with the preview address on each pull-request head, set
`CI_GITHUB_CHECK_RUNS_ENABLED=true` and `CI_GITHUB_CHECK_RUNS_TOKEN`; startup
fails if the flag is true without a token. GitHub only lets GitHub App tokens
write Check Runs, not personal access tokens.

Pull-request code is untrusted. On the remote builder the network-enabled fetch
phase executes none of it, and the phase that compiles it has no network and
only read access to the dependency cache shared with default-branch and release
builds. A pull request that the checked-in patches do not apply to fails in the
patch step with a message asking for a rebase.

The signed webhook implementation remains available but dormant. It is not
configured or started by default; only an explicit
`CI_GITHUB_WEBHOOK_ENABLED=true` plus `CI_GITHUB_WEBHOOK_SECRET` starts the
webhook consumer. Leave both webhook settings disabled for the polling-only
deployment.

Builds use the checked-in Pumpkin patch and pinned rootless builder image, then
publish isolated previews below `pumpkin.kurbis.host` (the CI hostname suffix is
fixed in the service and separate from `ROOT_DOMAIN`). Configure wildcard DNS
and TLS before enabling the polling service. Do not configure a GitHub
repository webhook for the default polling-only deployment.
`CI_BUILDER_DOCKER_HOST` must point to a dedicated rootless Docker daemon over
restricted SSH; never give the control plane a host Docker socket. Repository
Dockerfiles are not used.
The builder backend is explicit and fail-closed. Docker remains the default and requires a dedicated non-root rootless daemon over restricted SSH; never use `ssh://root@host` or the rootful `/var/run/docker.sock`. A Bubblewrap backend is available only with `CI_BUILDER_BACKEND=bubblewrap` and never falls back to Docker. It provides `bubblewrap_namespace_only` isolation (private paths, read-only root/toolchain, no network, dropped capabilities, `NoNewPrivs`, a kernel-enforced size-bounded `/work` tmpfs, timeout/descendant cleanup, and cross-process memory admission) but does not claim cgroup CPU, memory, or PID enforcement. Bubblewrap's writable build tree is never checked with post-hoc `du`, `statvfs`, or shell `ulimit` claims: startup/build preflight must find the operator-mounted tmpfs below private `$HOME` with exactly `size=5G,nr_inodes=1310720`, or the backend fails closed. Configure its immutable rootfs/toolchain paths and a pre-created private `CI_BUBBLEWRAP_WORKSPACE` mount such as `$HOME/.cache/kurbis/ci/workspace-tmpfs`; ordinary host directories and other tmpfs limits are rejected. The direct `bubblewrap` backend is native-control-plane-only: Compose deliberately does not pass or mount these host fixtures, so select it only with `./start.sh native ...` on Linux; a direct Compose Bubblewrap selection fails closed. Native `start.sh` resolves `CI_PLUGIN_SOURCE=plugins/kurbis-control` from the checkout, while Compose supplies `/opt/kurbis/plugins/kurbis-control` inside the control-plane image. For the explicit `bubblewrap_ssh` Compose deployment, set the absolute remote `CI_BUBBLEWRAP_*` values and `CI_BUILDER_SSH_TARGET` in the deployment environment and set `CI_BUILDER_SSH_HOST_DIRECTORY` to the private read-only SSH material directory; Compose mounts only that SSH directory, never builder rootfs or workspaces.

Branch previews remain lobbies and build lazily on first join. A full immutable
`commit-<sha>` hostname also builds lazily when a player joins; repeat joins use
the same build/artifact cache. A new observed PR head moves the stable PR alias
only after that head is ready, and late/out-of-order older events can never move
it backward. Deleted branches expire their stable alias and enter
 generation-fenced runtime cleanup. Closing a pull request does not currently
expire its preview. Check Run details are read-only and do not expose build logs,
runtime capabilities, or controls.

The deterministic acceptance harness for this path is described in [CI preview acceptance](ci-preview-acceptance.md); the builder runtime in [runtime/ci](../runtime/ci/README.md).
