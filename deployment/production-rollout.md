# Production releases

Release with one command from a clean checkout that is exactly `origin/main`:

```sh
TEST_DATABASE_URL=postgres://... python3 scripts/release.py
```

`TEST_DATABASE_URL` must point to a disposable PostgreSQL database; the gate
runs the script tests and the `first_join_flow` regression against it. The
Dokploy CLI must be logged in (or set `DOKPLOY_URL` and `DOKPLOY_API_KEY`), and
`ssh respond5` must work as root.

## What a release does

1. **Gate.** Refuses a dirty checkout or any commit other than `origin/main`,
   validates `migrations/MANIFEST.json`, and runs the tests.
2. **Builder files.** Installs this commit's `tools/ci_bubblewrap_runner.py`
   and, when the patches or the control plugin changed, builds its
   `ci-source-preparer` on the CI builder, both under content-derived
   directories next to the configured ones. The control plane will not start
   unless the builder's preparer reports the patch fingerprint compiled into
   it, so the release points `CI_BUBBLEWRAP_RUNNER` and
   `CI_BUBBLEWRAP_SOURCE_PREPARER` at these files in the same deployment.
3. **Pumpkin.** Compiles the pinned Pumpkin integration once on the remote CI
   builder, through the same `tools/ci_bubblewrap_runner.py` job the control
   plane uses for CI builds (`scripts/pumpkin_artifacts.py`, configured by the
   `CI_BUILDER_SSH_TARGET` and `CI_BUBBLEWRAP_*` values in Dokploy). The two
   SHA-256-checked binaries are uploaded to
   `/srv/kurbis/release-artifacts/<sha>` and packaged into
   `melon-pumpkin:host-control-<sha>` and `melon-pumpkin:waiting-<sha>`.
   Compose never compiles or builds Pumpkin.
4. **Deploy.** Sets `BUILD_REVISION` in Dokploy, keeps auto-deploy off, queues
   one Dokploy deployment of `main` and waits for it. Dokploy builds one
   deployment at a time across all projects, so the wait (`--deploy-wait`,
   four hours by default) includes its queue.
5. **Verify.** Every Compose service must be running, healthy, and labelled
   with the release SHA, and Dokploy must have deployed that commit.
6. **Worker.** The native `kurbis-worker` service gets the worker binary from
   the deployed app image and the new Pumpkin image. One worker process has to
   stay up for 40 seconds and send heartbeats within four minutes; otherwise
   the previous binary and settings are put back and the release fails. The
   first starts normally exit with `worker_already_active` until the previous
   worker's lease lapses.

7. **Canary.** The server named by `RELEASE_CANARY_SERVER_ID` in Dokploy is
   started with the same command the gateway sends when a player joins, and
   has to reach `online` within six minutes. Healthy containers do not prove a
   server can start; this does. On failure the release fails and prints the
   start error. The canary sleeps again by itself when it has been empty for
   the idle timeout. If the setting is missing the release only warns.

The result is recorded in `/srv/kurbis/deployed-release.json` on the host.

## Things to know

- **The worker has its own Docker daemon.** It starts servers on the rootless
  daemon named by `DOCKER_HOST` in `/srv/kurbis/worker.env`. Images built by a
  release live in the host's main daemon, so the release copies the Pumpkin
  image and the runtime API proxy image across. A worker that reports
  "No such image" is missing that copy.
- **Dokploy auto-deploy stays off.** A plain Dokploy deploy has no Pumpkin
  images for its commit and no matching worker, and fails when Compose tries
  to pull `melon-pumpkin:waiting-<sha>`.
- **Nothing is rolled back automatically.** If the deployment fails, Compose
  keeps the previous containers of any service it could not replace. To go
  back, release the older commit: reset `main` to it and run the script again.
- **Migrations are append-only.** An applied migration file is never edited or
  renumbered; a schema change is a new version in `migrations/MANIFEST.json`.
- **The control plane is slow to become healthy.** It registers the remote CI
  builder before serving HTTP, which has taken two minutes; its Compose health
  check allows five.

For stranded CI coordination rows, see [`ci-cleanup.md`](ci-cleanup.md).
