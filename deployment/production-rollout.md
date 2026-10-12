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
2. **Pumpkin.** Compiles the pinned Pumpkin integration once on the remote CI
   builder, through the same `tools/ci_bubblewrap_runner.py` job the control
   plane uses for CI builds (`scripts/pumpkin_artifacts.py`, configured by the
   `CI_BUILDER_SSH_TARGET` and `CI_BUBBLEWRAP_*` values in Dokploy). The two
   SHA-256-checked binaries are uploaded to
   `/srv/kurbis/release-artifacts/<sha>` and packaged into
   `melon-pumpkin:host-control-<sha>` and `melon-pumpkin:waiting-<sha>`.
   Compose never compiles or builds Pumpkin.
3. **Deploy.** Sets `BUILD_REVISION` in Dokploy, keeps auto-deploy off, queues
   one Dokploy deployment of `main` and waits for it. Dokploy builds one
   deployment at a time across all projects, so the wait (`--deploy-wait`,
   four hours by default) includes its queue.
4. **Verify.** Every Compose service must be running, healthy, and labelled
   with the release SHA, and Dokploy must have deployed that commit.
5. **Worker.** The native `kurbis-worker` service gets the worker binary from
   the deployed app image and the new Pumpkin image. It has to stay active and
   send heartbeats for 40 seconds; otherwise the previous binary and settings
   are put back and the release fails.

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
