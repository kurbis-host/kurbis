ALTER TABLE "public"."ci_runtime_placements"
    ADD COLUMN "world_initialized" boolean NOT NULL DEFAULT false,
    ADD COLUMN "stale_cleanup_claimed_at" timestamptz,
    ADD COLUMN "stale_cleanup_claim_token" uuid;

-- Every placement already existed before this migration and may have a
-- worker-local world even when its state was transitional.
UPDATE "public"."ci_runtime_placements" SET "world_initialized" = true;
