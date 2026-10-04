CREATE TABLE "public"."ci_check_run_tombstones" (
    "build_id" uuid PRIMARY KEY,
    "repository" text NOT NULL,
    "commit_sha" text NOT NULL CHECK ("commit_sha" ~ '^[0-9a-f]{40}$'),
    "patch_fingerprint" text NOT NULL CHECK (length("patch_fingerprint") BETWEEN 1 AND 255),
    "toolchain_version" text NOT NULL CHECK (length("toolchain_version") BETWEEN 1 AND 255),
    "target_architecture" text NOT NULL CHECK (length("target_architecture") BETWEEN 1 AND 255),
    "external_id" bigint NOT NULL UNIQUE,
    "stage" text NOT NULL CHECK ("stage" IN ('ready', 'failed', 'expired')),
    "last_stage" text NOT NULL CHECK ("last_stage" IN ('queued', 'fetching', 'patching', 'compiling', 'packaging', 'ready', 'failed', 'expired')),
    "updated_at" timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX "ci_check_run_tombstones_reconcile_idx"
    ON "public"."ci_check_run_tombstones" ("updated_at", "build_id")
    WHERE "last_stage" IS DISTINCT FROM "stage";
