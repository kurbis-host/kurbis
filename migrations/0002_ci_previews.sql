ALTER TABLE "public"."workers"
    ADD COLUMN "runtime_architecture" text NOT NULL DEFAULT 'unknown'
        CHECK ("runtime_architecture" IN ('unknown', 'x86_64-linux', 'aarch64-linux')),
    ADD COLUMN "supports_ci_previews" boolean NOT NULL DEFAULT false;

-- CI previews are intentionally separate from account-owned servers. They have
-- their own source refs, immutable build keys, artifacts, aliases, and runtime
-- generations so CI cleanup can never delete a user's world.
CREATE TABLE "public"."ci_watched_repositories" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    "repository" text NOT NULL UNIQUE,
    "default_branch" text NOT NULL,
    "enabled" boolean NOT NULL DEFAULT true,
    "etag" text,
    "branch_etag" text,
    "pull_request_page" integer NOT NULL DEFAULT 1 CHECK ("pull_request_page" > 0),
    "branch_cursor" text,
    "last_successful_poll" timestamptz,
    "created_at" timestamptz NOT NULL DEFAULT now(),
    "updated_at" timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT "ci_watched_repositories_repository_check" CHECK ("repository" ~ '^[^/[:space:]]+/[^/[:space:]]+$'),
    CONSTRAINT "ci_watched_repositories_default_branch_check" CHECK (length("default_branch") BETWEEN 1 AND 255)
);

CREATE TABLE "public"."ci_refs" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    "repository_id" uuid NOT NULL REFERENCES "public"."ci_watched_repositories"("id") ON DELETE CASCADE,
    "kind" text NOT NULL CHECK ("kind" IN ('pull_request', 'branch')),
    "name" text NOT NULL CHECK (length("name") BETWEEN 1 AND 255),
    "number" bigint,
    "source_repository" text,
    "commit_sha" text NOT NULL CHECK ("commit_sha" ~ '^[0-9a-f]{40}$'),
    "closed" boolean NOT NULL DEFAULT false,
    "deleted" boolean NOT NULL DEFAULT false,
    "updated_at" timestamptz NOT NULL DEFAULT now(),
    UNIQUE ("repository_id", "kind", "name")
);

CREATE TABLE "public"."ci_builds" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    "repository" text NOT NULL,
    "commit_sha" text NOT NULL CHECK ("commit_sha" ~ '^[0-9a-f]{40}$'),
    "patch_fingerprint" text NOT NULL CHECK (length("patch_fingerprint") BETWEEN 1 AND 255),
    "toolchain_version" text NOT NULL CHECK (length("toolchain_version") BETWEEN 1 AND 255),
    "target_architecture" text NOT NULL CHECK (length("target_architecture") BETWEEN 1 AND 255),
    "stage" text NOT NULL DEFAULT 'queued' CHECK ("stage" IN ('queued', 'fetching', 'patching', 'compiling', 'packaging', 'ready', 'failed', 'expired')),
    "artifact_digest" text,
    "lease_token" uuid,
    "lease_expires_at" timestamptz,
    "failure_stage" text CHECK ("failure_stage" IS NULL OR "failure_stage" IN ('queued', 'fetching', 'patching', 'compiling', 'packaging', 'ready', 'failed', 'expired')),
    "failure_message" text,
    "build_log" text NOT NULL DEFAULT '',
    "created_at" timestamptz NOT NULL DEFAULT now(),
    "updated_at" timestamptz NOT NULL DEFAULT now(),
    "expires_at" timestamptz NOT NULL,
    CONSTRAINT "ci_builds_lease_check" CHECK (("lease_token" IS NULL) = ("lease_expires_at" IS NULL)),
    UNIQUE ("repository", "commit_sha", "patch_fingerprint", "toolchain_version", "target_architecture")
);

CREATE TABLE "public"."ci_artifacts" (
    "digest" text PRIMARY KEY CHECK (length("digest") BETWEEN 1 AND 255),
    "object_key" text NOT NULL CHECK (length("object_key") BETWEEN 1 AND 1024),
    "size_bytes" bigint NOT NULL CHECK ("size_bytes" >= 0),
    "created_at" timestamptz NOT NULL DEFAULT now(),
    "expires_at" timestamptz NOT NULL
);

CREATE TABLE "public"."ci_preview_aliases" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    "repository" text NOT NULL,
    "commit_sha" text NOT NULL CHECK ("commit_sha" ~ '^[0-9a-f]{40}$'),
    "kind" text NOT NULL CHECK ("kind" IN ('pull_request', 'branch', 'commit')),
    "ref_name" text,
    "hostname" text NOT NULL UNIQUE CHECK (length("hostname") BETWEEN 1 AND 253),
    "build_id" uuid NOT NULL REFERENCES "public"."ci_builds"("id") ON DELETE RESTRICT,
    "state" text NOT NULL CHECK ("state" IN ('lobby', 'queued', 'fetching', 'patching', 'compiling', 'packaging', 'booting', 'ready', 'running', 'stopping', 'failed', 'expired')),
    "runtime_generation" bigint NOT NULL DEFAULT 0 CHECK ("runtime_generation" >= 0),
    "settings" jsonb NOT NULL DEFAULT '{}'::jsonb,
    "expires_at" timestamptz NOT NULL,
    "created_at" timestamptz NOT NULL DEFAULT now(),
    "updated_at" timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT "ci_preview_aliases_ref_name_check" CHECK (
        ("kind" = 'commit' AND "ref_name" IS NULL)
        OR ("kind" <> 'commit' AND length("ref_name") BETWEEN 1 AND 255)
    )
);

CREATE TABLE "public"."ci_check_runs" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    "build_id" uuid NOT NULL REFERENCES "public"."ci_builds"("id") ON DELETE CASCADE,
    "repository" text NOT NULL,
    "commit_sha" text NOT NULL CHECK ("commit_sha" ~ '^[0-9a-f]{40}$'),
    "provider" text NOT NULL DEFAULT 'github' CHECK ("provider" = 'github'),
    "external_id" bigint,
    "external_url" text,
    "last_stage" text NOT NULL CHECK ("last_stage" IN ('queued', 'fetching', 'patching', 'compiling', 'packaging', 'ready', 'failed', 'expired')),
    "claim_token" uuid,
    "claim_expires_at" timestamptz,
    "updated_at" timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT "ci_check_runs_claim_check" CHECK (("claim_token" IS NULL) = ("claim_expires_at" IS NULL)),
    UNIQUE ("repository", "commit_sha")
);
CREATE UNIQUE INDEX "ci_check_runs_external_id_idx" ON "public"."ci_check_runs" ("external_id") WHERE "external_id" IS NOT NULL;

CREATE INDEX "ci_refs_active_idx" ON "public"."ci_refs" ("repository_id", "kind", "updated_at" DESC) WHERE NOT "deleted";
CREATE INDEX "ci_builds_expiry_idx" ON "public"."ci_builds" ("expires_at");
CREATE INDEX "ci_builds_stage_idx" ON "public"."ci_builds" ("stage", "updated_at");
CREATE INDEX "ci_artifacts_expiry_idx" ON "public"."ci_artifacts" ("expires_at");
CREATE INDEX "ci_preview_aliases_expiry_idx" ON "public"."ci_preview_aliases" ("expires_at");
CREATE INDEX "ci_preview_aliases_build_idx" ON "public"."ci_preview_aliases" ("build_id");

-- CI runtime state stays independent from account server rows and placements.
-- A stopped preview retains its worker-local durable world/disk reservation until
-- its 72-hour sliding TTL is explicitly deleted.
CREATE TABLE "public"."ci_runtime_placements" (
    "preview_id" uuid PRIMARY KEY REFERENCES "public"."ci_preview_aliases"("id") ON DELETE CASCADE,
    "worker_id" uuid NOT NULL REFERENCES "public"."workers"("id") ON DELETE RESTRICT,
    "generation" bigint NOT NULL CHECK ("generation" > 0),
    "lease_token" uuid NOT NULL UNIQUE,
    "java_port" integer NOT NULL CHECK ("java_port" BETWEEN 1025 AND 65533),
    "bedrock_port" integer NOT NULL CHECK ("bedrock_port" BETWEEN 1026 AND 65534),
    "signal_port" integer NOT NULL CHECK ("signal_port" BETWEEN 1027 AND 65535),
    "state" text NOT NULL CHECK ("state" IN ('starting', 'running', 'stopping', 'stopped', 'deleting')),
    "active" boolean NOT NULL DEFAULT false,
    "last_activity_at" timestamptz NOT NULL DEFAULT now(),
    "capability_hash" bytea NOT NULL CHECK (octet_length("capability_hash") = 32),
    "reported_players" jsonb NOT NULL DEFAULT '[]'::jsonb,
    "created_at" timestamptz NOT NULL DEFAULT now(),
    "updated_at" timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT "ci_runtime_port_order_check" CHECK (
        "bedrock_port" = "java_port" + 1 AND "signal_port" = "java_port" + 2
    ),
    CONSTRAINT "ci_runtime_active_state_check" CHECK (
        "state" = 'deleting' OR "active" = ("state" IN ('running', 'stopping'))
    )
);

CREATE TABLE "public"."ci_port_leases" (
    "worker_id" uuid NOT NULL REFERENCES "public"."workers"("id") ON DELETE CASCADE,
    "port" integer NOT NULL CHECK ("port" BETWEEN 1025 AND 65535),
    "preview_id" uuid NOT NULL REFERENCES "public"."ci_preview_aliases"("id") ON DELETE CASCADE,
    "purpose" text NOT NULL CHECK ("purpose" IN ('java', 'bedrock', 'signal')),
    PRIMARY KEY ("worker_id", "port"),
    UNIQUE ("preview_id", "purpose")
);

CREATE INDEX "ci_runtime_idle_idx"
    ON "public"."ci_runtime_placements" ("last_activity_at", "preview_id")
    WHERE "active" AND "state" = 'running';
CREATE INDEX "ci_runtime_worker_idx"
    ON "public"."ci_runtime_placements" ("worker_id", "state");
