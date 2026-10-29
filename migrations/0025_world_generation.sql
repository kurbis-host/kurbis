-- How a server's world was generated: its world type, the superflat or
-- single biome choices, structures and the seed (WorldGeneration in
-- src/contracts/world.rs). The owner picks them when resetting the world, and
-- Pumpkin reads them only when it creates a new world.
--
-- They describe the world on disk, not a setting that can change under it,
-- so they live beside the settings rather than in them: a settings save or a
-- full reset to default settings never touches them. Every snapshot keeps
-- the world it captured, and a restore brings that back with the files.
--
-- Worlds created before this migration are default worlds whose seed was
-- never recorded; the worker reads it from the world when it next starts.
ALTER TABLE "public"."servers"
    ADD COLUMN "world" "jsonb" DEFAULT '{"world_type": "normal", "structures": true, "seed": null}'::"jsonb" NOT NULL;

ALTER TABLE "public"."snapshots"
    ADD COLUMN "world" "jsonb" DEFAULT '{"world_type": "normal", "structures": true, "seed": null}'::"jsonb" NOT NULL;

DROP TRIGGER "servers_panel_event" ON "public"."servers";
CREATE TRIGGER "servers_panel_event" AFTER INSERT OR UPDATE OF "subdomain", "desired_state", "observed_state", "settings", "world", "storage_bytes", "current_snapshot_generation", "last_error" ON "public"."servers" FOR EACH ROW EXECUTE FUNCTION "public"."emit_server_row_event"();
