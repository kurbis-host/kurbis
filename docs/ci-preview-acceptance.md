# Milestone 5 CI-preview acceptance

The deterministic vertical harness is `tests/milestone5_acceptance.rs`. It
runs against a disposable PostgreSQL database and exercises the real repository,
poller, build pipeline, artifact cache, runtime coordinator, generation fences,
TTL extension, expiry cleanup, and failure rollback. It uses a source-control
fixture and a recording remote-build/runtime host so a unit test never claims
live Linux or Dokploy evidence.

```sh
export TEST_DATABASE_URL=postgres://user@127.0.0.1/postgres
./tools/run_milestone5_acceptance.sh
```

The migration case loads `tests/fixtures/production_like_m5_snapshot.sql` after
migration `0001_fresh_schema.sql`, then runs every append-only migration and
checks row identity, worker/runtime data, migration-one checksum, and forbidden
legacy objects.

## Real Linux / Dokploy hook

Live acceptance is opt-in and fails closed. The hook requires all of:

- `M5_LIVE_LINUX=1`
- `CI_BUILDER_BACKEND=bubblewrap_ssh`
- `CI_BUILDER_SSH_TARGET` set to a non-root SSH target
- `CI_BUILDER_SSH_HOST_DIRECTORY` set to the mounted private SSH directory
- `DOKPLOY_URL` and `DOKPLOY_API_KEY` set for the existing deployment verifier

The script refuses to run live mode for `root@...`, a root Docker socket, or a
missing database URL. It delegates production release checks to the
`tools/test_release.py` unit suite; it never mutates Dokploy or a remote worker from the normal fixture
run. A real build/preview result must include an exact source revision, remote
receipt digest, artifact digest, runtime generation, and cleanup evidence before
it can be recorded as accepted.
