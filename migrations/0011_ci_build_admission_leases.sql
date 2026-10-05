-- Cross-process builder slots and memory reservations. PostgreSQL advisory
-- locks fence the live slot; this row supplies heartbeat state for reclaiming
-- resources after a crashed control-plane process.
CREATE TABLE "public"."ci_build_admission_leases" (
    "builder_identity" text NOT NULL,
    "slot" smallint NOT NULL,
    "lease_token" uuid NOT NULL UNIQUE,
    "owner_pid" integer NOT NULL,
    "requested_memory_bytes" bigint NOT NULL,
    "heartbeat_at" timestamptz NOT NULL DEFAULT now(),
    "created_at" timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT "ci_build_admission_leases_pkey" PRIMARY KEY ("builder_identity", "slot"),
    CONSTRAINT "ci_build_admission_leases_slot_check" CHECK ("slot" >= 0),
    CONSTRAINT "ci_build_admission_leases_memory_check" CHECK ("requested_memory_bytes" > 0)
);

CREATE INDEX "ci_build_admission_leases_heartbeat_idx"
    ON "public"."ci_build_admission_leases" ("builder_identity", "heartbeat_at");
