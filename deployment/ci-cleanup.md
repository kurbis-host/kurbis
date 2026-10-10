# CI coordination cleanup

`ci-cleanup` is the only supported manual repair command for stranded CI
coordination state. It is deliberately narrower than preview retention and
object garbage collection:

- it **does not** delete CI builds, preview aliases, runtime worlds, artifacts,
  PostgreSQL databases, Compose volumes, Dokploy deployments, or account-owned
  servers;
- it only releases expired database coordination markers so the existing
  poller/maintenance workers can retry (`ci_builds`, Check Run claims, webhook
  claims, repository poll leases, and stale runtime-deletion claims);
- build-admission leases are report-only unless an operator supplies one exact
  lease token after removing its Docker/Bubblewrap resources outside this
  command.

The normal maintenance loop should recover these rows. Use this command only
when the maintenance loop is disabled, a migration/restart stranded a row, or
an operator needs an auditable repair.

## 1. Preflight and dry run

First take and verify a PostgreSQL backup using the production backup system.
Do not use a filesystem copy of a live PostgreSQL data directory as the backup.
Then run a read-only report from the control-plane environment:

```sh
cargo run --locked --bin ci-cleanup -- \
  --database-url "$DATABASE_URL" \
  > /tmp/kurbis-ci-cleanup-report.json
```

Review every candidate. The report includes the exact row identity, lease token,
repository/build identity, expiry timestamp, and whether an admission lease
requires external resource cleanup. If the report is truncated, increase
`--limit` and repeat; an apply is refused while it is truncated.

## 2. Apply the bounded repair

Applying requires all of the following:

1. a verified PostgreSQL backup reference (backup ID or immutable operator path);
2. an unused audit-file path (the command refuses to overwrite an existing
   file);
3. the exact confirmation phrase below;
4. no active deployment or schema migration using the same database.

```sh
cargo run --locked --bin ci-cleanup -- \
  --database-url "$DATABASE_URL" \
  --apply \
  --confirm 'RECLAIM EXPIRED CI COORDINATION LEASES' \
  --database-backup-reference 'backup-id-or-immutable-path' \
  --backup-file "/srv/kurbis/backups/ci-cleanup-$(date -u +%Y%m%dT%H%M%SZ).json"
```

The command takes a fresh candidate snapshot, writes the audit file with
`create_new`, and then clears only rows that are still expired. Re-running the
same operation is safe: already-cleared rows affect zero rows. Use a new audit
path for each apply attempt.

## Build-admission lease exception

A stale `ci_build_admission_leases` row may represent an orphaned builder
container or Bubblewrap workspace. The command never assumes that external
resources are gone. After inspecting and removing the resources using the
builder's normal cleanup procedure, apply one exact token from the dry-run
report:

```sh
cargo run --locked --bin ci-cleanup -- \
  --database-url "$DATABASE_URL" \
  --apply \
  --confirm 'RECLAIM EXPIRED CI COORDINATION LEASES' \
  --database-backup-reference 'backup-id-or-immutable-path' \
  --backup-file /srv/kurbis/backups/ci-cleanup-admission.json \
  --admission-lease-token '<token-from-report>' \
  --builder-identity '<builder-identity-from-report>' \
  --admission-resources-cleaned
```

This deletes only that token for that builder identity and only while its
heartbeat is still older than the stale threshold. It never cleans arbitrary
containers, workspaces, or volumes. If the external resource cannot be
identified and removed safely, leave the row for the built-in admission
reclaimer instead.

## Production restrictions

Do not run this command during a release, migration, or active CI incident
without recording the operator, backup reference, report path, and resulting
JSON output. Never substitute `DROP DATABASE`, `docker compose down -v`,
`docker system prune`, or a broad `DELETE FROM ci_*` statement. Preview/world
retention and object-store garbage collection remain generation-fenced and are
owned by the control-plane maintenance loop.
