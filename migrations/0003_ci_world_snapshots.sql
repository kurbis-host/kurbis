ALTER TABLE "public"."ci_runtime_placements"
    ADD COLUMN "snapshot_requested_at" timestamptz;

CREATE TABLE "public"."ci_world_snapshots" (
    "preview_id" uuid PRIMARY KEY REFERENCES "public"."ci_preview_aliases"("id") ON DELETE CASCADE,
    "object_key" text NOT NULL UNIQUE CHECK (length("object_key") BETWEEN 1 AND 1024),
    "size_bytes" bigint NOT NULL CHECK ("size_bytes" BETWEEN 1 AND 5368709120),
    "checksum" text NOT NULL CHECK ("checksum" ~ '^[0-9a-f]{64}$'),
    "source_generation" bigint NOT NULL CHECK ("source_generation" > 0),
    "created_at" timestamptz NOT NULL DEFAULT now()
);
