-- Move panel/operator and durable console-command schema changes out of the
-- immutable production baseline migration. This migration is safe for a
-- database whose only applied migration is 0001_fresh_schema.sql.

CREATE OR REPLACE FUNCTION "public"."bump_owner_access_revisions"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    AS $$
DECLARE
  old_owner uuid;
  new_owner uuid;
BEGIN
  IF TG_OP <> 'INSERT' THEN
    old_owner := OLD.user_id;
  END IF;
  IF TG_OP <> 'DELETE' THEN
    new_owner := NEW.user_id;
  END IF;
  IF (TG_OP = 'DELETE' AND OLD.provider NOT IN ('minecraft_java', 'minecraft_bedrock')) OR
     (TG_OP = 'INSERT' AND NEW.provider NOT IN ('minecraft_java', 'minecraft_bedrock')) OR
     (TG_OP = 'UPDATE' AND OLD.provider NOT IN ('minecraft_java', 'minecraft_bedrock') AND NEW.provider NOT IN ('minecraft_java', 'minecraft_bedrock')) THEN
    IF TG_OP = 'DELETE' THEN
      RETURN OLD;
    END IF;
    RETURN NEW;
  END IF;
  INSERT INTO server_whitelist_state(server_id, revision, applied_revision)
    SELECT id, 1, 0 FROM servers WHERE owner_id = old_owner AND old_owner IS NOT NULL
    ON CONFLICT(server_id) DO UPDATE SET revision = server_whitelist_state.revision + 1, updated_at = now();
  INSERT INTO server_operator_state(server_id, revision, applied_revision)
    SELECT id, 1, 0 FROM servers WHERE owner_id = old_owner AND old_owner IS NOT NULL
    ON CONFLICT(server_id) DO UPDATE SET revision = server_operator_state.revision + 1, updated_at = now();
  PERFORM emit_server_event(id, 'operators') FROM servers WHERE owner_id = old_owner;
  IF new_owner IS DISTINCT FROM old_owner THEN
    INSERT INTO server_whitelist_state(server_id, revision, applied_revision)
      SELECT id, 1, 0 FROM servers WHERE owner_id = new_owner AND new_owner IS NOT NULL
      ON CONFLICT(server_id) DO UPDATE SET revision = server_whitelist_state.revision + 1, updated_at = now();
    INSERT INTO server_operator_state(server_id, revision, applied_revision)
      SELECT id, 1, 0 FROM servers WHERE owner_id = new_owner AND new_owner IS NOT NULL
      ON CONFLICT(server_id) DO UPDATE SET revision = server_operator_state.revision + 1, updated_at = now();
    PERFORM emit_server_event(id, 'operators') FROM servers WHERE owner_id = new_owner;
  END IF;
  IF TG_OP = 'DELETE' THEN
    RETURN OLD;
  END IF;
  RETURN NEW;
END;
$$;

CREATE FUNCTION "public"."initialize_server_panel_state"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
BEGIN
    INSERT INTO server_whitelist_state(server_id, revision, applied_revision)
    VALUES(NEW.id, 0, 0)
    ON CONFLICT(server_id) DO NOTHING;
    INSERT INTO server_operator_state(server_id, revision, applied_revision)
    VALUES(NEW.id, 1, 0)
    ON CONFLICT(server_id) DO NOTHING;
    RETURN NEW;
END;
$$;

CREATE FUNCTION "public"."bump_operator_revision"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
DECLARE
    target_server uuid := COALESCE(NEW.server_id, OLD.server_id);
BEGIN
    IF TG_OP = 'UPDATE'
       AND OLD.player_id IS NOT DISTINCT FROM NEW.player_id
       AND OLD.name IS NOT DISTINCT FROM NEW.name THEN
        RETURN NEW;
    END IF;
    IF NOT EXISTS(SELECT 1 FROM servers WHERE id = target_server) THEN
        RETURN COALESCE(NEW, OLD);
    END IF;

    INSERT INTO server_operator_state(server_id, revision, applied_revision, updated_at)
    VALUES(target_server, 1, 0, now())
    ON CONFLICT(server_id) DO UPDATE
      SET revision = server_operator_state.revision + 1,
          updated_at = now();
    PERFORM emit_server_event(target_server, 'operators');
    RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE FUNCTION "public"."emit_operator_state_event"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
BEGIN
    IF OLD.applied_revision IS DISTINCT FROM NEW.applied_revision THEN
        PERFORM emit_server_event(NEW.server_id, 'operators');
    END IF;
    RETURN NEW;
END;
$$;

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
        'operators'::text
    ])));

CREATE TABLE "public"."operator_entries" (
    "server_id" "uuid" NOT NULL,
    "player_id" "uuid" NOT NULL,
    "name" "text" NOT NULL,
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "updated_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    CONSTRAINT "operator_entries_name_check"
        CHECK (("char_length"("name") >= 1) AND ("char_length"("name") <= 64))
);

CREATE TABLE "public"."server_known_players" (
    "server_id" "uuid" NOT NULL,
    "player_id" "uuid" NOT NULL,
    "name" "text" NOT NULL,
    "edition" "text" NOT NULL,
    "first_seen_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "last_seen_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    CONSTRAINT "server_known_players_edition_check"
        CHECK (("edition" = ANY (ARRAY['java'::text, 'bedrock'::text]))),
    CONSTRAINT "server_known_players_name_check"
        CHECK (("char_length"("name") >= 1) AND ("char_length"("name") <= 64))
);

ALTER TABLE ONLY "public"."server_known_players"
    ADD CONSTRAINT "server_known_players_pkey" PRIMARY KEY ("server_id", "player_id");

CREATE UNIQUE INDEX "operator_entries_server_name_idx"
    ON "public"."operator_entries" USING "btree" ("server_id", "lower"("name"));

CREATE UNIQUE INDEX "operator_entries_server_player_idx"
    ON "public"."operator_entries" USING "btree" ("server_id", "player_id");

CREATE INDEX "server_known_players_name_idx"
    ON "public"."server_known_players" USING "btree"
    ("server_id", "lower"("name"), "player_id");

CREATE TRIGGER "servers_initialize_panel_state"
    AFTER INSERT ON "public"."servers"
    FOR EACH ROW EXECUTE FUNCTION "public"."initialize_server_panel_state"();

CREATE TRIGGER "operator_entries_revision"
    AFTER INSERT OR DELETE OR UPDATE OF "player_id", "name" ON "public"."operator_entries"
    FOR EACH ROW EXECUTE FUNCTION "public"."bump_operator_revision"();

CREATE TRIGGER "server_operator_state_panel_event"
    AFTER UPDATE OF "applied_revision" ON "public"."server_operator_state"
    FOR EACH ROW EXECUTE FUNCTION "public"."emit_operator_state_event"();

ALTER TABLE ONLY "public"."operator_entries"
    ADD CONSTRAINT "operator_entries_server_id_fkey"
    FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."server_known_players"
    ADD CONSTRAINT "server_known_players_server_id_fkey"
    FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

CREATE TYPE "public"."console_command_status" AS ENUM (
    'queued',
    'running',
    'succeeded',
    'failed',
    'cancelled'
);

CREATE TABLE "public"."console_commands" (
    "id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    "server_id" uuid NOT NULL REFERENCES "public"."servers"("id") ON DELETE CASCADE,
    "request_id" uuid NOT NULL,
    "worker_id" uuid REFERENCES "public"."workers"("id") ON DELETE SET NULL,
    "generation" bigint NOT NULL CHECK ("generation" > 0),
    "attempt" integer NOT NULL DEFAULT 0 CHECK ("attempt" >= 0),
    "command" text NOT NULL CHECK (length("command") BETWEEN 1 AND 512),
    "status" "public"."console_command_status" NOT NULL DEFAULT 'queued',
    "output" text[] NOT NULL DEFAULT ARRAY[]::text[],
    "error" text,
    "claimed_at" timestamptz,
    "completed_at" timestamptz,
    "created_at" timestamptz NOT NULL DEFAULT now(),
    "updated_at" timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT "console_commands_completion_check" CHECK (
        ("status" IN ('succeeded', 'failed', 'cancelled')) = ("completed_at" IS NOT NULL)
    )
);

CREATE UNIQUE INDEX "console_commands_request_id_idx"
    ON "public"."console_commands" ("server_id", "request_id");
CREATE INDEX "console_commands_claim_idx"
    ON "public"."console_commands" ("worker_id", "created_at", "id")
    WHERE "status" = 'queued';
CREATE INDEX "console_commands_server_history_idx"
    ON "public"."console_commands" ("server_id", "created_at" DESC, "id" DESC);

CREATE FUNCTION "public"."cancel_stale_console_commands"() RETURNS trigger
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
BEGIN
    UPDATE console_commands
    SET status = 'cancelled',
        error = 'server_assignment_changed',
        completed_at = now(),
        updated_at = now()
    WHERE server_id = NEW.id
      AND (generation <> NEW.generation
           OR NEW.desired_state <> 'running'
           OR NEW.observed_state <> 'online')
      AND status IN ('queued', 'running');
    RETURN NEW;
END;
$$;

CREATE TRIGGER "servers_cancel_stale_console_commands"
    AFTER UPDATE OF "generation", "desired_state", "observed_state" ON "public"."servers"
    FOR EACH ROW
    WHEN (OLD."generation" IS DISTINCT FROM NEW."generation"
          OR NEW."desired_state" <> 'running'
          OR NEW."observed_state" <> 'online')
    EXECUTE FUNCTION "public"."cancel_stale_console_commands"();
