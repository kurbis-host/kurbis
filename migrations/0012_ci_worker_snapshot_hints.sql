-- Persist the generation-scoped worker checkpoint hints used to fence
-- snapshot reads and placement reuse.
CREATE TABLE "public"."worker_snapshot_hints" (
    "worker_id" "uuid" NOT NULL,
    "server_id" "uuid" NOT NULL,
    "snapshot_generation" bigint NOT NULL,
    "reported_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    CONSTRAINT "worker_snapshot_hints_snapshot_generation_check" CHECK ("snapshot_generation" > 0)
);

ALTER TABLE ONLY "public"."worker_snapshot_hints"
    ADD CONSTRAINT "worker_snapshot_hints_pkey" PRIMARY KEY ("worker_id", "server_id");

CREATE INDEX "worker_snapshot_hints_generation_idx"
    ON "public"."worker_snapshot_hints" USING "btree"
    ("server_id", "snapshot_generation", "worker_id");

ALTER TABLE ONLY "public"."worker_snapshot_hints"
    ADD CONSTRAINT "worker_snapshot_hints_worker_id_fkey"
    FOREIGN KEY ("worker_id") REFERENCES "public"."workers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."worker_snapshot_hints"
    ADD CONSTRAINT "worker_snapshot_hints_snapshot_fkey"
    FOREIGN KEY ("server_id", "snapshot_generation")
    REFERENCES "public"."snapshots"("server_id", "generation") ON DELETE CASCADE;
