ALTER TABLE "public"."ci_builds"
    ADD COLUMN "failure_class" text
        CHECK ("failure_class" IS NULL OR "failure_class" IN ('transient', 'permanent')),
    ADD COLUMN "retry_count" integer NOT NULL DEFAULT 0
        CHECK ("retry_count" >= 0),
    ADD COLUMN "retry_at" timestamptz;

CREATE INDEX "ci_builds_retry_idx"
    ON "public"."ci_builds" ("retry_at", "updated_at")
    WHERE "stage" = 'failed' AND "failure_class" = 'transient' AND "retry_at" IS NOT NULL;
