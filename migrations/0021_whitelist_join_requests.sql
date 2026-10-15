-- Record who put each player on a server's whitelist and say so in the
-- server's activity, and keep the players the whitelist recently turned away.
--
-- A player on the whitelist may let in someone the server just turned away
-- (see the `whitelist_by_members` server setting), so the owner needs to see
-- who added whom. The entry keeps the name of whoever added it; the activity
-- event the whitelist trigger emits now carries a one-line detail.

-- New servers now start with the whitelist on. The database triggers and the
-- worker read a missing `whitelist` key as off, so write that down for any
-- server that never stored the key before the API's default changes.
UPDATE "public"."servers"
    SET "settings" = "settings" || '{"whitelist": false}'::jsonb
    WHERE NOT ("settings" ? 'whitelist');

ALTER TABLE "public"."whitelist_entries"
    ADD COLUMN "added_by" text,
    ADD COLUMN "added_in_game" boolean NOT NULL DEFAULT false,
    ADD CONSTRAINT "whitelist_entries_added_by_check"
        CHECK ("added_by" IS NULL OR char_length("added_by") BETWEEN 1 AND 64);

ALTER TABLE "public"."server_events"
    ADD COLUMN "detail" text,
    ADD CONSTRAINT "server_events_detail_check"
        CHECK ("detail" IS NULL OR char_length("detail") <= 300);

CREATE FUNCTION "public"."emit_server_event"("target_server" "uuid", "event_name" "text", "event_detail" "text") RETURNS "void"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
DECLARE
    event_id bigint;
BEGIN
    INSERT INTO server_events(server_id, event_type, detail)
    SELECT target_server, event_name, event_detail
    WHERE EXISTS(SELECT 1 FROM servers WHERE id = target_server)
    RETURNING id INTO event_id;

    IF event_id IS NOT NULL THEN
        PERFORM pg_notify('server_events', target_server::text || ':' || event_id::text);
    END IF;
END;
$$;

CREATE OR REPLACE FUNCTION "public"."emit_server_event"("target_server" "uuid", "event_name" "text") RETURNS "void"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
BEGIN
    PERFORM emit_server_event(target_server, event_name, NULL);
END;
$$;

CREATE OR REPLACE FUNCTION "public"."bump_whitelist_revision"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
DECLARE
    target_server uuid := COALESCE(NEW.server_id, OLD.server_id);
    detail text;
BEGIN
    IF NOT EXISTS(SELECT 1 FROM servers WHERE id = target_server) THEN
        RETURN COALESCE(NEW, OLD);
    END IF;

    INSERT INTO server_whitelist_state(server_id, revision, applied_revision, updated_at)
    VALUES(target_server, 1, 0, now())
    ON CONFLICT(server_id) DO UPDATE
      SET revision = server_whitelist_state.revision + 1,
          updated_at = now();
    IF TG_OP = 'INSERT' THEN
        detail := format(
            '%s added %s to the whitelist %s',
            COALESCE(NEW.added_by, 'Someone'),
            NEW.name,
            CASE WHEN NEW.added_in_game THEN 'in game' ELSE 'in the panel' END
        );
    ELSIF TG_OP = 'DELETE' THEN
        detail := format('%s was removed from the whitelist', OLD.name);
    END IF;
    PERFORM emit_server_event(target_server, 'whitelist', detail);
    RETURN COALESCE(NEW, OLD);
END;
$$;

-- Players a server's whitelist turned away recently, as the control plugin
-- reports them. The panel lists them so the owner can let one in even when
-- nobody who could is online; an owner without a linked Minecraft account
-- can mark one as themselves. A request goes once the player is on the
-- whitelist, and the API drops requests older than a day.
CREATE TABLE "public"."whitelist_join_requests" (
    "server_id" uuid NOT NULL REFERENCES "public"."servers"("id") ON DELETE CASCADE,
    "player_id" uuid NOT NULL,
    "name" text NOT NULL,
    "attempts" integer NOT NULL DEFAULT 1,
    "first_attempt_at" timestamptz NOT NULL DEFAULT now(),
    "last_attempt_at" timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY ("server_id", "player_id"),
    CONSTRAINT "whitelist_join_requests_name_check" CHECK (char_length("name") BETWEEN 1 AND 64),
    CONSTRAINT "whitelist_join_requests_attempts_check" CHECK ("attempts" >= 1)
);

CREATE INDEX "whitelist_join_requests_recent_idx"
    ON "public"."whitelist_join_requests" ("server_id", "last_attempt_at" DESC);
