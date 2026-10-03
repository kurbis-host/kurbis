

CREATE TYPE "public"."operation_kind" AS ENUM (
    'start',
    'stop',
    'restart',
    'snapshot',
    'delete',
    'restore',
    'reset'
);

CREATE TYPE "public"."operation_status" AS ENUM (
    'pending',
    'running',
    'succeeded',
    'failed',
    'cancelled'
);

CREATE TYPE "public"."server_desired_state" AS ENUM (
    'running',
    'stopped',
    'deleted'
);

CREATE TYPE "public"."server_observed_state" AS ENUM (
    'sleeping',
    'queued',
    'restoring',
    'starting',
    'online',
    'stopping',
    'snapshotting',
    'failed',
    'deleted',
    'resetting'
);

CREATE TYPE "public"."snapshot_reason" AS ENUM (
    'periodic',
    'shutdown',
    'manual',
    'restore',
    'reset',
    'export'
);

CREATE FUNCTION "public"."bump_owner_access_revisions"() RETURNS "trigger"
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

CREATE FUNCTION "public"."bump_whitelist_revision"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
DECLARE
    target_server uuid := COALESCE(NEW.server_id, OLD.server_id);
BEGIN
    IF NOT EXISTS(SELECT 1 FROM servers WHERE id = target_server) THEN
        RETURN COALESCE(NEW, OLD);
    END IF;

    INSERT INTO server_whitelist_state(server_id, revision, applied_revision, updated_at)
    VALUES(target_server, 1, 0, now())
    ON CONFLICT(server_id) DO UPDATE
      SET revision = server_whitelist_state.revision + 1,
          updated_at = now();
    PERFORM emit_server_event(target_server, 'whitelist');
    RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE FUNCTION "public"."bump_whitelist_setting_revision"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
BEGIN
    IF COALESCE(OLD.settings ->> 'whitelist', 'false')
       IS DISTINCT FROM COALESCE(NEW.settings ->> 'whitelist', 'false') THEN
        INSERT INTO server_whitelist_state(server_id, revision, applied_revision, updated_at)
        VALUES(NEW.id, 1, 0, now())
        ON CONFLICT(server_id) DO UPDATE
          SET revision = server_whitelist_state.revision + 1,
              updated_at = now();
        PERFORM emit_server_event(NEW.id, 'whitelist');
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION "public"."choose_default_server_before_write"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.owner_id = OLD.owner_id THEN
        RETURN NEW;
    END IF;

    PERFORM id FROM users WHERE id = NEW.owner_id FOR UPDATE;
    IF NOT NEW.is_default AND NOT EXISTS (
        SELECT 1 FROM servers WHERE owner_id = NEW.owner_id AND is_default
    ) THEN
        NEW.is_default := true;
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION "public"."emit_log_event"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
BEGIN
    PERFORM emit_server_event(NEW.server_id, 'log');
    RETURN NEW;
END;
$$;

CREATE FUNCTION "public"."emit_operation_event"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
BEGIN
    PERFORM emit_server_event(COALESCE(NEW.server_id, OLD.server_id), 'operation');
    RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE FUNCTION "public"."emit_players_event"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
BEGIN
    IF TG_OP = 'UPDATE'
       AND OLD.name IS NOT DISTINCT FROM NEW.name
       AND OLD.edition IS NOT DISTINCT FROM NEW.edition THEN
        RETURN NEW;
    END IF;
    PERFORM emit_server_event(COALESCE(NEW.server_id, OLD.server_id), 'players');
    RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE FUNCTION "public"."emit_server_event"("target_server" "uuid", "event_name" "text") RETURNS "void"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
DECLARE
    event_id bigint;
BEGIN
    INSERT INTO server_events(server_id, event_type)
    SELECT target_server, event_name
    WHERE EXISTS(SELECT 1 FROM servers WHERE id = target_server)
    RETURNING id INTO event_id;

    IF event_id IS NOT NULL THEN
        PERFORM pg_notify('server_events', target_server::text || ':' || event_id::text);
    END IF;
END;
$$;

CREATE FUNCTION "public"."emit_server_row_event"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
BEGIN
    PERFORM emit_server_event(COALESCE(NEW.id, OLD.id), 'server');
    RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE FUNCTION "public"."emit_snapshot_event"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
BEGIN
    PERFORM emit_server_event(COALESCE(NEW.server_id, OLD.server_id), 'snapshot');
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

CREATE FUNCTION "public"."emit_whitelist_state_event"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    SET "search_path" TO 'pg_catalog', 'public'
    AS $$
BEGIN
    PERFORM emit_server_event(NEW.server_id, 'whitelist');
    RETURN NEW;
END;
$$;

CREATE FUNCTION "public"."notify_route_change"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    AS $$
BEGIN
  PERFORM pg_notify('route_changes', COALESCE(NEW.server_id, OLD.server_id)::text);
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE FUNCTION "public"."notify_server_route_change"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    AS $$
BEGIN
  PERFORM pg_notify('route_changes', COALESCE(NEW.id, OLD.id)::text);
  IF TG_OP = 'DELETE' THEN
    RETURN OLD;
  END IF;
  RETURN NEW;
END;
$$;

CREATE FUNCTION "public"."replace_deleted_default_server"() RETURNS "trigger"
    LANGUAGE "plpgsql"
    AS $$
BEGIN
    IF OLD.is_default THEN
        UPDATE servers
        SET is_default = true
        WHERE id = (
            SELECT id FROM servers
            WHERE owner_id = OLD.owner_id
            ORDER BY created_at, id
            LIMIT 1
        );
    END IF;
    RETURN OLD;
END;
$$;

CREATE TABLE "public"."identities" (
    "id" "uuid" NOT NULL,
    "user_id" "uuid" NOT NULL,
    "provider" "text" NOT NULL,
    "subject" "text" NOT NULL,
    "display_name" "text" NOT NULL,
    "email" "text",
    "email_normalized" "text",
    "email_verified" boolean DEFAULT false NOT NULL,
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "updated_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "minecraft_player_id" "uuid",
    CONSTRAINT "identities_check" CHECK (((NOT "email_verified") OR ("email_normalized" IS NOT NULL))),
    CONSTRAINT "identities_minecraft_profile_check" CHECK ((("provider" <> 'minecraft_java'::"text") OR ("minecraft_player_id" IS NOT NULL))),
    CONSTRAINT "identities_provider_check" CHECK (("provider" = ANY (ARRAY['google'::"text", 'discord'::"text", 'minecraft_java'::"text", 'minecraft_bedrock'::"text"])))
);

CREATE TABLE "public"."minecraft_login_tokens" (
    "token_hash" "bytea" NOT NULL,
    "provider" "text" NOT NULL,
    "subject" "text" NOT NULL,
    "display_name" "text" NOT NULL,
    "expires_at" timestamp with time zone NOT NULL,
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "minecraft_player_id" "uuid" NOT NULL,
    CONSTRAINT "minecraft_login_tokens_provider_check" CHECK (("provider" = ANY (ARRAY['minecraft_java'::"text", 'minecraft_bedrock'::"text"])))
);

CREATE TABLE "public"."oauth_states" (
    "state_hash" "bytea" NOT NULL,
    "expires_at" timestamp with time zone NOT NULL,
    "provider" "text" NOT NULL,
    "intent" "text" NOT NULL,
    "user_id" "uuid",
    CONSTRAINT "oauth_states_intent_check" CHECK (("intent" = ANY (ARRAY['login'::"text", 'link'::"text"])))
);

CREATE TABLE "public"."operations" (
    "id" "uuid" NOT NULL,
    "server_id" "uuid" NOT NULL,
    "worker_id" "uuid",
    "kind" "public"."operation_kind" NOT NULL,
    "status" "public"."operation_status" DEFAULT 'pending'::"public"."operation_status" NOT NULL,
    "generation" bigint NOT NULL,
    "attempt" integer DEFAULT 0 NOT NULL,
    "payload" "jsonb" DEFAULT '{}'::"jsonb" NOT NULL,
    "last_error" "text",
    "claimed_at" timestamp with time zone,
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "updated_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "next_attempt_at" timestamp with time zone DEFAULT "now"() NOT NULL
);

CREATE TABLE "public"."placements" (
    "server_id" "uuid" NOT NULL,
    "worker_id" "uuid" NOT NULL,
    "generation" bigint NOT NULL,
    "lease_token" "uuid" NOT NULL,
    "java_port" integer NOT NULL,
    "bedrock_port" integer NOT NULL,
    "active" boolean DEFAULT false NOT NULL,
    "updated_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "signal_port" integer NOT NULL,
    CONSTRAINT "placements_signal_port_range_check" CHECK ((("signal_port" >= 1025) AND ("signal_port" <= 65535)))
);

CREATE TABLE "public"."port_leases" (
    "worker_id" "uuid" NOT NULL,
    "port" integer NOT NULL,
    "server_id" "uuid" NOT NULL,
    "purpose" "text" NOT NULL,
    CONSTRAINT "port_leases_purpose_check" CHECK (("purpose" = ANY (ARRAY['java'::"text", 'bedrock'::"text", 'signal'::"text"])))
);

CREATE TABLE "public"."server_events" (
    "id" bigint NOT NULL,
    "server_id" "uuid" NOT NULL,
    "event_type" "text" NOT NULL,
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    CONSTRAINT "server_events_event_type_check" CHECK (("event_type" = ANY (ARRAY['server'::"text", 'operation'::"text", 'snapshot'::"text", 'log'::"text", 'players'::"text", 'whitelist'::"text", 'operators'::"text"])))
);

CREATE SEQUENCE "public"."server_events_id_seq"
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE "public"."server_events_id_seq" OWNED BY "public"."server_events"."id";

CREATE TABLE "public"."server_instance_capabilities" (
    "server_id" "uuid" NOT NULL,
    "worker_id" "uuid" NOT NULL,
    "placement_generation" bigint NOT NULL,
    "token_hash" "bytea" NOT NULL,
    "updated_at" timestamp with time zone DEFAULT "now"() NOT NULL
);

CREATE TABLE "public"."server_login_rate_limits" (
    "server_id" "uuid" NOT NULL,
    "window_started_at" timestamp with time zone NOT NULL,
    "request_count" integer NOT NULL,
    CONSTRAINT "server_login_rate_limits_request_count_check" CHECK (("request_count" >= 0))
);

CREATE TABLE "public"."server_login_tokens" (
    "token_hash" "bytea" NOT NULL,
    "server_id" "uuid" NOT NULL,
    "expires_at" timestamp with time zone NOT NULL,
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL
);

CREATE TABLE "public"."server_logs" (
    "id" bigint NOT NULL,
    "server_id" "uuid" NOT NULL,
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "line" "text" NOT NULL
);

CREATE SEQUENCE "public"."server_logs_id_seq"
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE "public"."server_logs_id_seq" OWNED BY "public"."server_logs"."id";

CREATE TABLE "public"."server_operator_state" (
    "server_id" "uuid" NOT NULL,
    "revision" bigint DEFAULT 1 NOT NULL,
    "applied_revision" bigint DEFAULT 0 NOT NULL,
    "updated_at" timestamp with time zone DEFAULT "now"() NOT NULL
);

CREATE TABLE "public"."operator_entries" (
    "server_id" "uuid" NOT NULL,
    "player_id" "uuid" NOT NULL,
    "name" "text" NOT NULL,
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "updated_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    CONSTRAINT "operator_entries_name_check" CHECK (("char_length"("name") >= 1) AND ("char_length"("name") <= 64))
);

CREATE TABLE "public"."server_known_players" (
    "server_id" "uuid" NOT NULL,
    "player_id" "uuid" NOT NULL,
    "name" "text" NOT NULL,
    "edition" "text" NOT NULL,
    "first_seen_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "last_seen_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    CONSTRAINT "server_known_players_edition_check" CHECK (("edition" = ANY (ARRAY['java'::"text", 'bedrock'::"text"]))),
    CONSTRAINT "server_known_players_name_check" CHECK (("char_length"("name") >= 1) AND ("char_length"("name") <= 64))
);

CREATE TABLE "public"."server_players" (
    "server_id" "uuid" NOT NULL,
    "player_id" "uuid" NOT NULL,
    "name" "text" NOT NULL,
    "edition" "text" NOT NULL,
    "connected_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "last_seen_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    CONSTRAINT "server_players_edition_check" CHECK (("edition" = ANY (ARRAY['java'::"text", 'bedrock'::"text"]))),
    CONSTRAINT "server_players_name_check" CHECK ((("char_length"("name") >= 1) AND ("char_length"("name") <= 64)))
);

CREATE TABLE "public"."server_session_grants" (
    "id" "uuid" NOT NULL,
    "token_hash" "bytea" NOT NULL,
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    CONSTRAINT "server_session_grants_token_hash_check" CHECK (("octet_length"("token_hash") = 32))
);

CREATE TABLE "public"."server_sessions" (
    "id" "uuid" NOT NULL,
    "grant_id" "uuid" NOT NULL,
    "server_id" "uuid" NOT NULL,
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL
);

CREATE TABLE "public"."server_whitelist_state" (
    "server_id" "uuid" NOT NULL,
    "revision" bigint DEFAULT 0 NOT NULL,
    "applied_revision" bigint DEFAULT 0 NOT NULL,
    "updated_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    CONSTRAINT "server_whitelist_state_applied_revision_check" CHECK (("applied_revision" >= 0)),
    CONSTRAINT "server_whitelist_state_revision_check" CHECK (("revision" >= 0))
);

CREATE TABLE "public"."servers" (
    "id" "uuid" NOT NULL,
    "owner_id" "uuid" NOT NULL,
    "subdomain" "text" NOT NULL,
    "desired_state" "public"."server_desired_state" DEFAULT 'stopped'::"public"."server_desired_state" NOT NULL,
    "observed_state" "public"."server_observed_state" DEFAULT 'sleeping'::"public"."server_observed_state" NOT NULL,
    "settings" "jsonb" DEFAULT '{}'::"jsonb" NOT NULL,
    "storage_bytes" bigint DEFAULT 0 NOT NULL,
    "generation" bigint DEFAULT 0 NOT NULL,
    "current_snapshot_generation" bigint,
    "last_error" "text",
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "updated_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "is_default" boolean DEFAULT false NOT NULL,
    CONSTRAINT "servers_generation_check" CHECK (("generation" >= 0)),
    CONSTRAINT "servers_storage_bytes_check" CHECK (("storage_bytes" >= 0)),
    CONSTRAINT "servers_subdomain_check" CHECK (("subdomain" ~ '^[a-z0-9][a-z0-9-]{1,30}[a-z0-9]$'::"text"))
);

CREATE TABLE "public"."sessions" (
    "token_hash" "bytea" NOT NULL,
    "user_id" "uuid" NOT NULL,
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "id" "uuid" NOT NULL,
    "last_seen_at" timestamp with time zone DEFAULT "now"() NOT NULL
);

CREATE TABLE "public"."snapshots" (
    "id" "uuid" NOT NULL,
    "server_id" "uuid" NOT NULL,
    "generation" bigint NOT NULL,
    "manifest_key" "text" NOT NULL,
    "size_bytes" bigint NOT NULL,
    "checksum" "text" NOT NULL,
    "reason" "public"."snapshot_reason" NOT NULL,
    "complete" boolean DEFAULT false NOT NULL,
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    CONSTRAINT "snapshots_size_bytes_check" CHECK (("size_bytes" >= 0))
);

CREATE TABLE "public"."start_requests" (
    "server_id" "uuid" NOT NULL,
    "reason" "text" NOT NULL,
    "requested_at" timestamp with time zone DEFAULT "now"() NOT NULL
);

CREATE TABLE "public"."users" (
    "id" "uuid" NOT NULL,
    "display_name" "text" NOT NULL,
    "account_status" "text" DEFAULT 'active'::"text" NOT NULL,
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "updated_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    CONSTRAINT "users_account_status_check" CHECK (("account_status" = ANY (ARRAY['active'::"text", 'suspended'::"text"])))
);

CREATE TABLE "public"."whitelist_entries" (
    "server_id" "uuid" NOT NULL,
    "player_id" "uuid" NOT NULL,
    "name" "text" NOT NULL,
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "updated_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    CONSTRAINT "whitelist_entries_name_check" CHECK ((("char_length"("name") >= 1) AND ("char_length"("name") <= 64)))
);

CREATE TABLE "public"."worker_credentials" (
    "worker_id" "uuid" NOT NULL,
    "api_token_hash" "bytea" NOT NULL,
    "lease_id" "uuid",
    "lease_expires_at" timestamp with time zone,
    "revoked_at" timestamp with time zone,
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    CONSTRAINT "worker_credentials_api_token_hash_check" CHECK (("octet_length"("api_token_hash") = 32)),
    CONSTRAINT "worker_credentials_check" CHECK ((("lease_id" IS NULL) = ("lease_expires_at" IS NULL)))
);

CREATE TABLE "public"."workers" (
    "id" "uuid" NOT NULL,
    "name" "text" NOT NULL,
    "address" "text" NOT NULL,
    "total_memory_bytes" bigint NOT NULL,
    "total_disk_bytes" bigint NOT NULL,
    "total_cpu_millis" bigint NOT NULL,
    "reserved_memory_bytes" bigint DEFAULT 0 NOT NULL,
    "reserved_disk_bytes" bigint DEFAULT 0 NOT NULL,
    "reserved_cpu_millis" bigint DEFAULT 0 NOT NULL,
    "heartbeat_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "draining" boolean DEFAULT false NOT NULL,
    "port_min" integer DEFAULT 20000 NOT NULL,
    "port_max" integer DEFAULT 40000 NOT NULL,
    "signal_address" "text" NOT NULL,
    CONSTRAINT "workers_check" CHECK ((("port_min" > 1024) AND ("port_max" <= 65535) AND ("port_min" < "port_max"))),
    CONSTRAINT "workers_reserved_cpu_millis_check" CHECK (("reserved_cpu_millis" >= 0)),
    CONSTRAINT "workers_reserved_disk_bytes_check" CHECK (("reserved_disk_bytes" >= 0)),
    CONSTRAINT "workers_reserved_memory_bytes_check" CHECK (("reserved_memory_bytes" >= 0)),
    CONSTRAINT "workers_total_cpu_millis_check" CHECK (("total_cpu_millis" > 0)),
    CONSTRAINT "workers_total_disk_bytes_check" CHECK (("total_disk_bytes" > 0)),
    CONSTRAINT "workers_total_memory_bytes_check" CHECK (("total_memory_bytes" > 0))
);

CREATE TABLE "public"."worker_snapshot_hints" (
    "worker_id" "uuid" NOT NULL,
    "server_id" "uuid" NOT NULL,
    "snapshot_generation" bigint NOT NULL,
    "reported_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    CONSTRAINT "worker_snapshot_hints_snapshot_generation_check" CHECK (("snapshot_generation" > 0))
);

CREATE TABLE "public"."world_exports" (
    "id" "uuid" NOT NULL,
    "server_id" "uuid" NOT NULL,
    "requested_by" "uuid" NOT NULL,
    "status" "text" NOT NULL,
    "stage" "text" DEFAULT 'requested'::"text" NOT NULL,
    "source_generation" bigint,
    "worker_id" "uuid",
    "attempt" integer DEFAULT 0 NOT NULL,
    "archive_key" "text",
    "archive_size" bigint,
    "archive_checksum" "text",
    "last_error" "text",
    "created_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "updated_at" timestamp with time zone DEFAULT "now"() NOT NULL,
    "expires_at" timestamp with time zone,
    CONSTRAINT "world_exports_check" CHECK ((("status" <> 'completed'::"text") OR (("archive_key" IS NOT NULL) AND ("archive_size" IS NOT NULL) AND ("archive_checksum" IS NOT NULL) AND ("expires_at" IS NOT NULL)))),
    CONSTRAINT "world_exports_status_check" CHECK (("status" = ANY (ARRAY['pending'::"text", 'running'::"text", 'completed'::"text", 'failed'::"text", 'cancelled'::"text", 'expired'::"text"])))
);

ALTER TABLE ONLY "public"."server_events" ALTER COLUMN "id" SET DEFAULT "nextval"('"public"."server_events_id_seq"'::"regclass");

ALTER TABLE ONLY "public"."server_logs" ALTER COLUMN "id" SET DEFAULT "nextval"('"public"."server_logs_id_seq"'::"regclass");

ALTER TABLE ONLY "public"."identities"
    ADD CONSTRAINT "identities_pkey" PRIMARY KEY ("id");

ALTER TABLE ONLY "public"."identities"
    ADD CONSTRAINT "identities_provider_subject_key" UNIQUE ("provider", "subject");

ALTER TABLE ONLY "public"."minecraft_login_tokens"
    ADD CONSTRAINT "minecraft_login_tokens_pkey" PRIMARY KEY ("token_hash");

ALTER TABLE ONLY "public"."oauth_states"
    ADD CONSTRAINT "oauth_states_pkey" PRIMARY KEY ("state_hash");

ALTER TABLE ONLY "public"."operations"
    ADD CONSTRAINT "operations_pkey" PRIMARY KEY ("id");

ALTER TABLE ONLY "public"."placements"
    ADD CONSTRAINT "placements_pkey" PRIMARY KEY ("server_id");

ALTER TABLE ONLY "public"."port_leases"
    ADD CONSTRAINT "port_leases_pkey" PRIMARY KEY ("worker_id", "port");

ALTER TABLE ONLY "public"."port_leases"
    ADD CONSTRAINT "port_leases_server_id_purpose_key" UNIQUE ("server_id", "purpose");

ALTER TABLE ONLY "public"."server_events"
    ADD CONSTRAINT "server_events_pkey" PRIMARY KEY ("id");

ALTER TABLE ONLY "public"."server_instance_capabilities"
    ADD CONSTRAINT "server_instance_capabilities_pkey" PRIMARY KEY ("server_id");

ALTER TABLE ONLY "public"."server_login_rate_limits"
    ADD CONSTRAINT "server_login_rate_limits_pkey" PRIMARY KEY ("server_id");

ALTER TABLE ONLY "public"."server_login_tokens"
    ADD CONSTRAINT "server_login_tokens_pkey" PRIMARY KEY ("token_hash");

ALTER TABLE ONLY "public"."server_logs"
    ADD CONSTRAINT "server_logs_pkey" PRIMARY KEY ("id");

ALTER TABLE ONLY "public"."server_operator_state"
    ADD CONSTRAINT "server_operator_state_pkey" PRIMARY KEY ("server_id");

ALTER TABLE ONLY "public"."server_known_players"
    ADD CONSTRAINT "server_known_players_pkey" PRIMARY KEY ("server_id", "player_id");

ALTER TABLE ONLY "public"."server_players"
    ADD CONSTRAINT "server_players_pkey" PRIMARY KEY ("server_id", "player_id");

ALTER TABLE ONLY "public"."server_session_grants"
    ADD CONSTRAINT "server_session_grants_pkey" PRIMARY KEY ("id");

ALTER TABLE ONLY "public"."server_session_grants"
    ADD CONSTRAINT "server_session_grants_token_hash_key" UNIQUE ("token_hash");

ALTER TABLE ONLY "public"."server_sessions"
    ADD CONSTRAINT "server_sessions_grant_id_server_id_key" UNIQUE ("grant_id", "server_id");

ALTER TABLE ONLY "public"."server_sessions"
    ADD CONSTRAINT "server_sessions_pkey" PRIMARY KEY ("id");

ALTER TABLE ONLY "public"."server_whitelist_state"
    ADD CONSTRAINT "server_whitelist_state_pkey" PRIMARY KEY ("server_id");

ALTER TABLE ONLY "public"."servers"
    ADD CONSTRAINT "servers_pkey" PRIMARY KEY ("id");

ALTER TABLE ONLY "public"."servers"
    ADD CONSTRAINT "servers_subdomain_key" UNIQUE ("subdomain");

ALTER TABLE ONLY "public"."sessions"
    ADD CONSTRAINT "sessions_id_key" UNIQUE ("id");

ALTER TABLE ONLY "public"."sessions"
    ADD CONSTRAINT "sessions_pkey" PRIMARY KEY ("token_hash");

ALTER TABLE ONLY "public"."snapshots"
    ADD CONSTRAINT "snapshots_pkey" PRIMARY KEY ("id");

ALTER TABLE ONLY "public"."snapshots"
    ADD CONSTRAINT "snapshots_server_id_generation_key" UNIQUE ("server_id", "generation");

ALTER TABLE ONLY "public"."start_requests"
    ADD CONSTRAINT "start_requests_pkey" PRIMARY KEY ("server_id");

ALTER TABLE ONLY "public"."users"
    ADD CONSTRAINT "users_pkey" PRIMARY KEY ("id");

ALTER TABLE ONLY "public"."whitelist_entries"
    ADD CONSTRAINT "whitelist_entries_pkey" PRIMARY KEY ("server_id", "player_id");

ALTER TABLE ONLY "public"."worker_credentials"
    ADD CONSTRAINT "worker_credentials_api_token_hash_key" UNIQUE ("api_token_hash");

ALTER TABLE ONLY "public"."worker_credentials"
    ADD CONSTRAINT "worker_credentials_pkey" PRIMARY KEY ("worker_id");

ALTER TABLE ONLY "public"."workers"
    ADD CONSTRAINT "workers_name_key" UNIQUE ("name");

ALTER TABLE ONLY "public"."workers"
    ADD CONSTRAINT "workers_pkey" PRIMARY KEY ("id");

ALTER TABLE ONLY "public"."worker_snapshot_hints"
    ADD CONSTRAINT "worker_snapshot_hints_pkey" PRIMARY KEY ("worker_id", "server_id");

ALTER TABLE ONLY "public"."world_exports"
    ADD CONSTRAINT "world_exports_pkey" PRIMARY KEY ("id");

CREATE INDEX "identities_user_idx" ON "public"."identities" USING "btree" ("user_id", "created_at", "id");

CREATE INDEX "identities_verified_email_idx" ON "public"."identities" USING "btree" ("email_normalized") WHERE ("email_verified" AND ("email_normalized" IS NOT NULL));

CREATE INDEX "minecraft_login_tokens_expiry_idx" ON "public"."minecraft_login_tokens" USING "btree" ("expires_at");

CREATE UNIQUE INDEX "operator_entries_server_name_idx" ON "public"."operator_entries" USING "btree" ("server_id", "lower"("name"));

CREATE UNIQUE INDEX "operator_entries_server_player_idx" ON "public"."operator_entries" USING "btree" ("server_id", "player_id");

CREATE UNIQUE INDEX "one_active_world_export_per_server_idx" ON "public"."world_exports" USING "btree" ("server_id") WHERE ("status" = ANY (ARRAY['pending'::"text", 'running'::"text"]));

CREATE UNIQUE INDEX "one_live_lifecycle_operation_idx" ON "public"."operations" USING "btree" ("server_id") WHERE (("status" = ANY (ARRAY['pending'::"public"."operation_status", 'running'::"public"."operation_status"])) AND ("kind" = ANY (ARRAY['start'::"public"."operation_kind", 'stop'::"public"."operation_kind", 'restart'::"public"."operation_kind", 'reset'::"public"."operation_kind", 'delete'::"public"."operation_kind"])));

CREATE INDEX "operations_pending_worker_idx" ON "public"."operations" USING "btree" ("worker_id", "created_at") WHERE ("status" = 'pending'::"public"."operation_status");

CREATE INDEX "operations_retry_worker_idx" ON "public"."operations" USING "btree" ("worker_id", "next_attempt_at", "created_at") WHERE ("status" = 'pending'::"public"."operation_status");

CREATE INDEX "placements_worker_idx" ON "public"."placements" USING "btree" ("worker_id");

CREATE INDEX "server_events_server_cursor_idx" ON "public"."server_events" USING "btree" ("server_id", "id");

CREATE INDEX "server_login_tokens_expiry_idx" ON "public"."server_login_tokens" USING "btree" ("expires_at");

CREATE INDEX "server_login_tokens_server_created_idx" ON "public"."server_login_tokens" USING "btree" ("server_id", "created_at" DESC);

CREATE INDEX "server_logs_recent_idx" ON "public"."server_logs" USING "btree" ("server_id", "id" DESC);

CREATE TRIGGER "servers_initialize_panel_state" AFTER INSERT ON "public"."servers" FOR EACH ROW EXECUTE FUNCTION "public"."initialize_server_panel_state"();

CREATE TRIGGER "operator_entries_revision" AFTER INSERT OR DELETE OR UPDATE OF "player_id", "name" ON "public"."operator_entries" FOR EACH ROW EXECUTE FUNCTION "public"."bump_operator_revision"();

CREATE INDEX "server_known_players_name_idx" ON "public"."server_known_players" USING "btree" ("server_id", "lower"("name"), "player_id");

CREATE INDEX "server_players_server_seen_idx" ON "public"."server_players" USING "btree" ("server_id", "last_seen_at" DESC);

CREATE INDEX "server_sessions_grant_idx" ON "public"."server_sessions" USING "btree" ("grant_id");

CREATE INDEX "server_sessions_server_idx" ON "public"."server_sessions" USING "btree" ("server_id");

CREATE UNIQUE INDEX "servers_one_default_per_owner_idx" ON "public"."servers" USING "btree" ("owner_id") WHERE "is_default";

CREATE INDEX "sessions_user_idx" ON "public"."sessions" USING "btree" ("user_id");

CREATE INDEX "snapshots_latest_idx" ON "public"."snapshots" USING "btree" ("server_id", "generation" DESC) WHERE "complete";

CREATE INDEX "start_requests_order_idx" ON "public"."start_requests" USING "btree" ("requested_at");

CREATE UNIQUE INDEX "whitelist_entries_server_name_idx" ON "public"."whitelist_entries" USING "btree" ("server_id", "lower"("name"));

CREATE INDEX "workers_healthy_idx" ON "public"."workers" USING "btree" ("heartbeat_at") WHERE (NOT "draining");

CREATE INDEX "worker_snapshot_hints_generation_idx" ON "public"."worker_snapshot_hints" USING "btree" ("server_id", "snapshot_generation", "worker_id");

CREATE INDEX "world_exports_expiry_idx" ON "public"."world_exports" USING "btree" ("expires_at") WHERE ("status" = 'completed'::"text");

CREATE INDEX "world_exports_pending_idx" ON "public"."world_exports" USING "btree" ("created_at", "id") WHERE ("status" = 'pending'::"text");

CREATE INDEX "world_exports_pin_idx" ON "public"."world_exports" USING "btree" ("server_id", "source_generation") WHERE (("status" = ANY (ARRAY['pending'::"text", 'running'::"text"])) AND ("source_generation" IS NOT NULL));

CREATE TRIGGER "identities_bump_owner_access_revisions" AFTER INSERT OR DELETE OR UPDATE OF "user_id", "minecraft_player_id", "provider", "display_name" ON "public"."identities" FOR EACH ROW EXECUTE FUNCTION "public"."bump_owner_access_revisions"();

CREATE TRIGGER "operations_panel_event" AFTER INSERT OR UPDATE OF "status", "payload", "last_error" ON "public"."operations" FOR EACH ROW EXECUTE FUNCTION "public"."emit_operation_event"();

CREATE TRIGGER "placements_route_change" AFTER INSERT OR DELETE OR UPDATE ON "public"."placements" FOR EACH ROW EXECUTE FUNCTION "public"."notify_route_change"();

CREATE TRIGGER "server_logs_panel_event" AFTER INSERT ON "public"."server_logs" FOR EACH ROW EXECUTE FUNCTION "public"."emit_log_event"();

CREATE TRIGGER "server_players_panel_event" AFTER INSERT OR DELETE OR UPDATE OF "name", "edition" ON "public"."server_players" FOR EACH ROW EXECUTE FUNCTION "public"."emit_players_event"();

CREATE TRIGGER "server_operator_state_panel_event" AFTER UPDATE OF "applied_revision" ON "public"."server_operator_state" FOR EACH ROW EXECUTE FUNCTION "public"."emit_operator_state_event"();

CREATE TRIGGER "server_whitelist_state_panel_event" AFTER UPDATE OF "applied_revision" ON "public"."server_whitelist_state" FOR EACH ROW EXECUTE FUNCTION "public"."emit_whitelist_state_event"();

CREATE TRIGGER "servers_choose_default_before_write" BEFORE INSERT OR UPDATE OF "owner_id" ON "public"."servers" FOR EACH ROW EXECUTE FUNCTION "public"."choose_default_server_before_write"();

CREATE TRIGGER "servers_panel_event" AFTER INSERT OR UPDATE OF "subdomain", "desired_state", "observed_state", "settings", "storage_bytes", "current_snapshot_generation", "last_error" ON "public"."servers" FOR EACH ROW EXECUTE FUNCTION "public"."emit_server_row_event"();

CREATE TRIGGER "servers_replace_deleted_default" AFTER DELETE ON "public"."servers" FOR EACH ROW EXECUTE FUNCTION "public"."replace_deleted_default_server"();

CREATE TRIGGER "servers_route_change" AFTER INSERT OR DELETE OR UPDATE OF "observed_state", "desired_state", "subdomain" ON "public"."servers" FOR EACH ROW EXECUTE FUNCTION "public"."notify_server_route_change"();

CREATE TRIGGER "servers_whitelist_setting_revision" AFTER UPDATE OF "settings" ON "public"."servers" FOR EACH ROW EXECUTE FUNCTION "public"."bump_whitelist_setting_revision"();

CREATE TRIGGER "snapshots_panel_event" AFTER INSERT OR UPDATE OF "complete" ON "public"."snapshots" FOR EACH ROW EXECUTE FUNCTION "public"."emit_snapshot_event"();

CREATE TRIGGER "users_route_change" AFTER UPDATE OF "account_status" ON "public"."users" FOR EACH ROW EXECUTE FUNCTION "public"."notify_server_route_change"();

CREATE TRIGGER "whitelist_entries_revision" AFTER INSERT OR DELETE OR UPDATE ON "public"."whitelist_entries" FOR EACH ROW EXECUTE FUNCTION "public"."bump_whitelist_revision"();

CREATE TRIGGER "workers_route_change" AFTER UPDATE OF "address", "heartbeat_at" ON "public"."workers" FOR EACH ROW EXECUTE FUNCTION "public"."notify_server_route_change"();

ALTER TABLE ONLY "public"."identities"
    ADD CONSTRAINT "identities_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "public"."users"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."oauth_states"
    ADD CONSTRAINT "oauth_states_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "public"."users"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."operations"
    ADD CONSTRAINT "operations_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."operations"
    ADD CONSTRAINT "operations_worker_id_fkey" FOREIGN KEY ("worker_id") REFERENCES "public"."workers"("id") ON DELETE SET NULL;

ALTER TABLE ONLY "public"."operator_entries"
    ADD CONSTRAINT "operator_entries_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."placements"
    ADD CONSTRAINT "placements_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."placements"
    ADD CONSTRAINT "placements_worker_id_fkey" FOREIGN KEY ("worker_id") REFERENCES "public"."workers"("id") ON DELETE RESTRICT;

ALTER TABLE ONLY "public"."port_leases"
    ADD CONSTRAINT "port_leases_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."port_leases"
    ADD CONSTRAINT "port_leases_worker_id_fkey" FOREIGN KEY ("worker_id") REFERENCES "public"."workers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."server_events"
    ADD CONSTRAINT "server_events_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."server_instance_capabilities"
    ADD CONSTRAINT "server_instance_capabilities_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."server_instance_capabilities"
    ADD CONSTRAINT "server_instance_capabilities_worker_id_fkey" FOREIGN KEY ("worker_id") REFERENCES "public"."workers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."server_login_rate_limits"
    ADD CONSTRAINT "server_login_rate_limits_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."server_login_tokens"
    ADD CONSTRAINT "server_login_tokens_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."server_logs"
    ADD CONSTRAINT "server_logs_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."server_operator_state"
    ADD CONSTRAINT "server_operator_state_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."server_known_players"
    ADD CONSTRAINT "server_known_players_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."server_players"
    ADD CONSTRAINT "server_players_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."server_sessions"
    ADD CONSTRAINT "server_sessions_grant_id_fkey" FOREIGN KEY ("grant_id") REFERENCES "public"."server_session_grants"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."server_sessions"
    ADD CONSTRAINT "server_sessions_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."server_whitelist_state"
    ADD CONSTRAINT "server_whitelist_state_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."servers"
    ADD CONSTRAINT "servers_owner_id_fkey" FOREIGN KEY ("owner_id") REFERENCES "public"."users"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."sessions"
    ADD CONSTRAINT "sessions_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "public"."users"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."snapshots"
    ADD CONSTRAINT "snapshots_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."worker_snapshot_hints"
    ADD CONSTRAINT "worker_snapshot_hints_worker_id_fkey" FOREIGN KEY ("worker_id") REFERENCES "public"."workers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."worker_snapshot_hints"
    ADD CONSTRAINT "worker_snapshot_hints_snapshot_fkey" FOREIGN KEY ("server_id", "snapshot_generation") REFERENCES "public"."snapshots"("server_id", "generation") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."start_requests"
    ADD CONSTRAINT "start_requests_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."whitelist_entries"
    ADD CONSTRAINT "whitelist_entries_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."worker_credentials"
    ADD CONSTRAINT "worker_credentials_worker_id_fkey" FOREIGN KEY ("worker_id") REFERENCES "public"."workers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."world_exports"
    ADD CONSTRAINT "world_exports_server_id_fkey" FOREIGN KEY ("server_id") REFERENCES "public"."servers"("id") ON DELETE CASCADE;

ALTER TABLE ONLY "public"."world_exports"
    ADD CONSTRAINT "world_exports_worker_id_fkey" FOREIGN KEY ("worker_id") REFERENCES "public"."workers"("id") ON DELETE SET NULL;

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
