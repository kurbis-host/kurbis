CREATE TABLE server_events (
    id bigserial PRIMARY KEY,
    server_id uuid NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    event_type text NOT NULL CHECK (event_type IN (
        'server', 'operation', 'snapshot', 'log', 'players', 'whitelist'
    )),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX server_events_server_cursor_idx ON server_events(server_id, id);

CREATE TABLE server_players (
    server_id uuid NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    player_id uuid NOT NULL,
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 64),
    edition text NOT NULL CHECK (edition IN ('java', 'bedrock')),
    connected_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(server_id, player_id)
);
CREATE INDEX server_players_server_seen_idx ON server_players(server_id, last_seen_at DESC);

CREATE TABLE server_whitelist_state (
    server_id uuid PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    applied_revision bigint NOT NULL DEFAULT 0 CHECK (applied_revision >= 0),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE whitelist_entries (
    server_id uuid NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    player_id uuid NOT NULL,
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 64),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(server_id, player_id)
);
CREATE UNIQUE INDEX whitelist_entries_server_name_idx
ON whitelist_entries(server_id, lower(name));

CREATE OR REPLACE FUNCTION emit_server_event(target_server uuid, event_name text)
RETURNS void LANGUAGE plpgsql AS $$
DECLARE
    event_id bigint;
BEGIN
    INSERT INTO server_events(server_id, event_type)
    VALUES(target_server, event_name)
    RETURNING id INTO event_id;
    PERFORM pg_notify('server_events', target_server::text || ':' || event_id::text);
END;
$$;

CREATE OR REPLACE FUNCTION emit_server_row_event()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM emit_server_event(COALESCE(NEW.id, OLD.id), 'server');
    RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION emit_operation_event()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM emit_server_event(COALESCE(NEW.server_id, OLD.server_id), 'operation');
    RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION emit_snapshot_event()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM emit_server_event(COALESCE(NEW.server_id, OLD.server_id), 'snapshot');
    RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION emit_log_event()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM emit_server_event(NEW.server_id, 'log');
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION emit_players_event()
RETURNS trigger LANGUAGE plpgsql AS $$
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

CREATE OR REPLACE FUNCTION bump_whitelist_revision()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    target_server uuid := COALESCE(NEW.server_id, OLD.server_id);
BEGIN
    INSERT INTO server_whitelist_state(server_id, revision, applied_revision, updated_at)
    VALUES(target_server, 1, 0, now())
    ON CONFLICT(server_id) DO UPDATE
      SET revision = server_whitelist_state.revision + 1,
          updated_at = now();
    PERFORM emit_server_event(target_server, 'whitelist');
    RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION bump_whitelist_setting_revision()
RETURNS trigger LANGUAGE plpgsql AS $$
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

CREATE TRIGGER servers_panel_event
AFTER INSERT OR UPDATE OF subdomain, desired_state, observed_state, settings,
    storage_bytes, current_snapshot_generation, last_error ON servers
FOR EACH ROW EXECUTE FUNCTION emit_server_row_event();

CREATE TRIGGER servers_whitelist_setting_revision
AFTER UPDATE OF settings ON servers
FOR EACH ROW EXECUTE FUNCTION bump_whitelist_setting_revision();

CREATE TRIGGER operations_panel_event
AFTER INSERT OR UPDATE OF status, payload, last_error ON operations
FOR EACH ROW EXECUTE FUNCTION emit_operation_event();

CREATE TRIGGER snapshots_panel_event
AFTER INSERT OR UPDATE OF complete ON snapshots
FOR EACH ROW EXECUTE FUNCTION emit_snapshot_event();

CREATE TRIGGER server_logs_panel_event
AFTER INSERT ON server_logs
FOR EACH ROW EXECUTE FUNCTION emit_log_event();

CREATE TRIGGER server_players_panel_event
AFTER INSERT OR DELETE OR UPDATE OF name, edition ON server_players
FOR EACH ROW EXECUTE FUNCTION emit_players_event();

CREATE TRIGGER whitelist_entries_revision
AFTER INSERT OR UPDATE OR DELETE ON whitelist_entries
FOR EACH ROW EXECUTE FUNCTION bump_whitelist_revision();

CREATE OR REPLACE FUNCTION emit_whitelist_state_event()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM emit_server_event(NEW.server_id, 'whitelist');
    RETURN NEW;
END;
$$;

CREATE TRIGGER server_whitelist_state_panel_event
AFTER UPDATE OF applied_revision ON server_whitelist_state
FOR EACH ROW EXECUTE FUNCTION emit_whitelist_state_event();

DROP INDEX one_live_lifecycle_operation_idx;
CREATE UNIQUE INDEX one_live_lifecycle_operation_idx ON operations(server_id)
WHERE status IN ('pending', 'running')
  AND kind IN ('start', 'stop', 'restart', 'restore', 'delete');
