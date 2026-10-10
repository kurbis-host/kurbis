# Production rollout policy

Use `scripts/deploy_dokploy.py` for every Kurbis production release. There is one
canonical release path: a clean checkout exactly matching `origin/main`, a
verified Dokploy Compose configuration, an immutable
`kurbis-release-<full-sha>` tag, and a coordinated Respond5 worker update.
Historical baseline manifests and one-time migration/rollback aliases are not
supported and are intentionally absent from this repository.

The release gate runs the rollout behavior suite and the database-backed
`first_join_flow` regression before mutating Dokploy or Respond5. Set
`TEST_DATABASE_URL` to a disposable PostgreSQL database; if it is unset or the
regression fails, the release stops before production changes.

Dokploy must use `docker-compose` mode with no custom command. The runner
verifies Dokploy's generated build-enabled command, rejects enabled patches for
`compose.yaml`, and keeps `autoDeploy` disabled. Git is the sole source of
truth for Compose, build arguments, CI configuration, TLS mounts, and worker
settings. The release temporarily selects the immutable tag, validates the
healthy stack and worker lease, then restores the configured `main` source
branch. A failed rollout keeps the lock and transaction evidence until both app
and worker recovery are verified.

Respond5 builds the pinned Pumpkin runtime from the exact release source and
checks the executable, WASM plugin, hosted entrypoint, OCI labels, embedded
revision, worker configuration, and renewing lease. Recovery uses only the
verified previous SHA and never removes PostgreSQL data, named volumes, worlds,
or object-store data.

Respond5 writes the accepted-release marker only after app and waiting-Pumpkin OCI labels and embedded revisions match the selected SHA, the waiting container's actual Pumpkin executable matches the independently built host-control image, and the worker binary/environment/lease transaction is verified. If post-deploy worker verification fails or its SSH response is lost, the wrapper idempotently restores the previous worker bundle and asks Dokploy to redeploy the prior app revision. For the one-time legacy path, rollback is constrained to the captured old image digests or images labeled with the attempted candidate SHA; it restores exact local image tags with `docker compose up --no-build --no-deps`, leaves Postgres and named volumes untouched, then restores the captured worker executable and protected legacy config. It verifies all exact service image digests, health, worker ID/executable/config and renewing lease before considering recovery verified. Legacy recovery remains explicitly marked `legacy-unproven`; only a later successful SHA-labeled release creates a source-proven marker. An unverified rollback retains protected transaction state; do not treat it as a successful release.

The first-join regression requires `TEST_DATABASE_URL` to point to a disposable PostgreSQL database. If it is unset or the test fails, the release stops before production changes.

## Immutable provenance contract

The checked-in `migrations/MANIFEST.json` is the canonical inventory of SQLx
migrations. It records each migration filename, version, and SQLx-compatible
SHA-384 checksum, plus a SHA-256 over the ordered manifest. Migration versions
are append-only: an applied file is never edited or reused, duplicate version
numbers are rejected, and a schema change must use a new version. The release
gate validates this manifest before touching Dokploy and the schema tests cover
both a fresh database and a database with the production migration history
already applied.

The Respond5 verifier receives one `kurbis.release-manifest.v1` for each
accepted release. It binds the full Git SHA to the application and panel/waiting
image IDs, compiled control-plane/gateway/worker digests, the independently
built Pumpkin image and source archive digests, the worker environment digest,
the migration-manifest hash, and a deterministic production environment
revision. `status=verified` is only written after every Compose service is
unique, running, and healthy (with the gateway's documented running-only
health contract), every service image and OCI revision matches the manifest,
and the worker executable, protected environment, identity, and database lease
still match. The manifest itself is content-addressed and tamper-checked before
`deployed-release.json` is written.

For stranded CI coordination rows, use the bounded, dry-run-first procedure in [`ci-cleanup.md`](ci-cleanup.md). It releases only expired coordination markers and requires an exact confirmation phrase, an immutable candidate audit file, and a verified PostgreSQL backup reference before applying changes.
