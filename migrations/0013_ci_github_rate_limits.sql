-- Durable provider pacing survives control-plane restarts and is shared by
-- source polling and Check Run reconciliation.
CREATE TABLE "public"."ci_github_rate_limits" (
    "scope" text PRIMARY KEY CHECK (length("scope") BETWEEN 1 AND 512),
    "remaining" bigint CHECK ("remaining" IS NULL OR "remaining" >= 0),
    "reset_at" timestamptz,
    "paused_until" timestamptz,
    "next_allowed_at" timestamptz,
    "consecutive_failures" integer NOT NULL DEFAULT 0 CHECK ("consecutive_failures" >= 0),
    "updated_at" timestamptz NOT NULL DEFAULT now()
);
