ALTER TABLE "public"."ci_runtime_placements"
    ADD COLUMN "capacity_reserved" boolean NOT NULL DEFAULT false;

-- Starting runtimes reserve memory and CPU before becoming routable; stopped
-- runtimes retain only their worker-local disk reservation.
UPDATE "public"."ci_runtime_placements"
SET "capacity_reserved" = true
WHERE "state" IN ('starting', 'running', 'stopping');
