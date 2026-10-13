-- Plugins a server's owner installs from the Pumpkin market or uploads.
--
-- A plugin binary is stored once in object storage under
-- `plugins/<sha256>.wasm`, whoever installed it. `plugin_blobs` records each
-- stored binary; maintenance deletes a binary once no server has used it for
-- an hour. `server_plugins` is the control plane's authoritative list of a
-- server's plugins: the worker writes the enabled ones into the runtime before
-- it starts and whenever the list's revision changes while it runs, then
-- reports what Pumpkin loaded or why it could not.

ALTER TABLE "public"."server_events"
    DROP CONSTRAINT "server_events_event_type_check";

ALTER TABLE "public"."server_events"
    ADD CONSTRAINT "server_events_event_type_check"
    CHECK (("event_type" = ANY (ARRAY[
        'server'::text,
        'operation'::text,
        'snapshot'::text,
        'log'::text,
        'players'::text,
        'whitelist'::text,
        'operators'::text,
        'ai'::text,
        'plugins'::text
    ])));

CREATE TABLE "public"."plugin_blobs" (
    "sha256" text PRIMARY KEY,
    "size_bytes" bigint NOT NULL,
    "last_used_at" timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT "plugin_blobs_sha256_check" CHECK ("sha256" ~ '^[0-9a-f]{64}$'),
    CONSTRAINT "plugin_blobs_size_check" CHECK ("size_bytes" BETWEEN 1 AND 33554432)
);

CREATE INDEX "plugin_blobs_last_used_idx" ON "public"."plugin_blobs" ("last_used_at");

CREATE TABLE "public"."server_plugins" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    "server_id" uuid NOT NULL REFERENCES "public"."servers"("id") ON DELETE CASCADE,
    "source" text NOT NULL,
    "market_plugin_id" bigint,
    "name" text NOT NULL,
    "version" text,
    "sha256" text NOT NULL REFERENCES "public"."plugin_blobs"("sha256"),
    "enabled" boolean NOT NULL DEFAULT true,
    -- What the runtime last reported for this plugin, and for which binary.
    "reported_sha256" text,
    "runtime_name" text,
    "runtime_version" text,
    "runtime_error" text,
    "reported_at" timestamptz,
    "created_at" timestamptz NOT NULL DEFAULT now(),
    "updated_at" timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT "server_plugins_source_check" CHECK ("source" IN ('market', 'upload')),
    CONSTRAINT "server_plugins_market_check" CHECK (
        ("source" = 'market') = ("market_plugin_id" IS NOT NULL)
        AND ("market_plugin_id" IS NULL OR "market_plugin_id" > 0)
    ),
    CONSTRAINT "server_plugins_name_check" CHECK (char_length("name") BETWEEN 1 AND 64),
    CONSTRAINT "server_plugins_version_check" CHECK (char_length("version") <= 64),
    CONSTRAINT "server_plugins_runtime_check" CHECK (
        char_length("runtime_name") <= 64
        AND char_length("runtime_version") <= 64
        AND char_length("runtime_error") <= 1024
    )
);

CREATE UNIQUE INDEX "server_plugins_market_idx"
    ON "public"."server_plugins" ("server_id", "market_plugin_id")
    WHERE "market_plugin_id" IS NOT NULL;
CREATE UNIQUE INDEX "server_plugins_binary_idx"
    ON "public"."server_plugins" ("server_id", "sha256");
CREATE INDEX "server_plugins_sha256_idx" ON "public"."server_plugins" ("sha256");

-- `revision` counts changes to what the runtime must load; the worker sets
-- `applied_revision` once it loaded that revision.
CREATE TABLE "public"."server_plugin_state" (
    "server_id" uuid PRIMARY KEY REFERENCES "public"."servers"("id") ON DELETE CASCADE,
    "revision" bigint NOT NULL DEFAULT 0,
    "applied_revision" bigint NOT NULL DEFAULT 0,
    "updated_at" timestamptz NOT NULL DEFAULT now()
);

CREATE FUNCTION "public"."server_plugins_changed"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
DECLARE
    target_server uuid := COALESCE(NEW.server_id, OLD.server_id);
BEGIN
    IF NOT EXISTS(SELECT 1 FROM servers WHERE id = target_server) THEN
        RETURN COALESCE(NEW, OLD);
    END IF;
    IF TG_OP <> 'UPDATE'
       OR OLD.sha256 IS DISTINCT FROM NEW.sha256
       OR OLD.enabled IS DISTINCT FROM NEW.enabled THEN
        INSERT INTO server_plugin_state(server_id, revision, applied_revision)
        VALUES(target_server, 1, 0)
        ON CONFLICT(server_id) DO UPDATE
          SET revision = server_plugin_state.revision + 1,
              updated_at = now();
    END IF;
    PERFORM emit_server_event(target_server, 'plugins');
    RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE FUNCTION "public"."emit_plugin_state_event"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
BEGIN
    IF OLD.applied_revision IS DISTINCT FROM NEW.applied_revision THEN
        PERFORM emit_server_event(NEW.server_id, 'plugins');
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER "server_plugins_revision"
    AFTER INSERT OR DELETE OR UPDATE ON "public"."server_plugins"
    FOR EACH ROW EXECUTE FUNCTION "public"."server_plugins_changed"();

CREATE TRIGGER "server_plugin_state_panel_event"
    AFTER UPDATE OF "applied_revision" ON "public"."server_plugin_state"
    FOR EACH ROW EXECUTE FUNCTION "public"."emit_plugin_state_event"();
