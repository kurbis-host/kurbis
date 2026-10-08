-- One current-generation runtime sample per account-owned server. The
-- measured_at column is assigned by the control plane database clock; the
-- timestamp nested in stats is kept in sync by the worker write path.
CREATE TABLE "public"."server_runtime_stats" (
    "server_id" uuid PRIMARY KEY REFERENCES "public"."servers"("id") ON DELETE CASCADE,
    "generation" bigint NOT NULL,
    "stats" jsonb NOT NULL,
    "measured_at" timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT "server_runtime_stats_generation_check" CHECK ("generation" > 0),
    CONSTRAINT "server_runtime_stats_object_check" CHECK (jsonb_typeof("stats") = 'object')
);

CREATE INDEX "server_runtime_stats_measured_at_idx"
    ON "public"."server_runtime_stats" ("measured_at");

COMMENT ON TABLE "public"."server_runtime_stats" IS
    'Latest runtime resource sample for each account-owned server; stale generations are hidden by the API.';
COMMENT ON COLUMN "public"."server_runtime_stats"."measured_at" IS
    'Control-plane database timestamp at which this sample was accepted.';
