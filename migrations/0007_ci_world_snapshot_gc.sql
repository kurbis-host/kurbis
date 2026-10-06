CREATE TABLE "public"."ci_world_snapshot_gc" (
    "object_key" text PRIMARY KEY CHECK (length("object_key") BETWEEN 1 AND 1024),
    "queued_at" timestamptz NOT NULL DEFAULT now(),
    "attempts" integer NOT NULL DEFAULT 0 CHECK ("attempts" >= 0),
    "claim_token" uuid,
    "claimed_at" timestamptz,
    "last_error" text,
    CONSTRAINT "ci_world_snapshot_gc_claim_check"
        CHECK (("claim_token" IS NULL) = ("claimed_at" IS NULL))
);

CREATE INDEX "ci_world_snapshot_gc_retry_idx"
    ON "public"."ci_world_snapshot_gc" ("queued_at", "object_key")
    WHERE "claim_token" IS NULL;
