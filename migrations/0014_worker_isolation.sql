DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'worker_runtime') THEN
        CREATE ROLE worker_runtime
            NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS;
    END IF;
END;
$$;
ALTER ROLE worker_runtime
    WITH NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS;

ALTER TABLE workers ADD COLUMN signal_address text NOT NULL DEFAULT '';
ALTER TABLE workers ALTER COLUMN signal_address DROP DEFAULT;

ALTER TABLE placements ADD COLUMN signal_port integer;
ALTER TABLE port_leases DROP CONSTRAINT port_leases_purpose_check;
ALTER TABLE port_leases ADD CONSTRAINT port_leases_purpose_check
    CHECK (purpose IN ('java', 'bedrock', 'signal'));

DO $$
DECLARE
    assigned RECORD;
    free_port integer;
BEGIN
    FOR assigned IN
        SELECT p.server_id, p.worker_id, p.java_port, p.bedrock_port,
               w.port_min, w.port_max
        FROM placements p
        JOIN workers w ON w.id = p.worker_id
        ORDER BY p.worker_id, p.server_id
    LOOP
        SELECT candidate.port INTO free_port
        FROM generate_series(assigned.port_min, assigned.port_max) AS candidate(port)
        WHERE candidate.port NOT IN (assigned.java_port, assigned.bedrock_port)
          AND NOT EXISTS (
              SELECT 1 FROM port_leases lease
              WHERE lease.worker_id = assigned.worker_id
                AND lease.port = candidate.port
          )
        ORDER BY candidate.port
        LIMIT 1;
        IF free_port IS NULL THEN
            RAISE EXCEPTION 'cannot allocate a private signal port for existing placement % on worker %',
                assigned.server_id, assigned.worker_id;
        END IF;
        UPDATE placements SET signal_port = free_port WHERE server_id = assigned.server_id;
        INSERT INTO port_leases(worker_id, port, server_id, purpose)
        VALUES(assigned.worker_id, free_port, assigned.server_id, 'signal');
    END LOOP;
END;
$$;

ALTER TABLE placements ALTER COLUMN signal_port SET NOT NULL;
ALTER TABLE placements ADD CONSTRAINT placements_signal_port_range_check
    CHECK (signal_port BETWEEN 1025 AND 65535);
-- Port ownership is fenced by port_leases. Keeping a second uniqueness key on
-- historical inactive placement rows would reject valid port reuse after a
-- stop, even though the lease has been released.

CREATE SCHEMA worker_auth;
REVOKE ALL ON SCHEMA worker_auth FROM PUBLIC;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO worker_runtime;
GRANT USAGE ON SCHEMA worker_auth TO worker_runtime;

CREATE TABLE worker_auth.credentials (
    worker_id uuid PRIMARY KEY REFERENCES workers(id) ON DELETE CASCADE,
    api_token_hash bytea NOT NULL UNIQUE,
    database_role name NOT NULL UNIQUE,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
REVOKE ALL ON worker_auth.credentials FROM PUBLIC, worker_runtime;

CREATE FUNCTION worker_auth.current_worker_id() RETURNS uuid
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog
AS $$
    SELECT worker_id
    FROM worker_auth.credentials
    WHERE database_role = session_user::name
      AND revoked_at IS NULL
$$;
REVOKE ALL ON FUNCTION worker_auth.current_worker_id() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION worker_auth.current_worker_id() TO worker_runtime;

CREATE FUNCTION worker_auth.can_access_server(target_server uuid) RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog
AS $$
    SELECT target_server IS NOT NULL AND (
        EXISTS (
            SELECT 1 FROM public.placements p
            JOIN public.servers s ON s.id = p.server_id
            WHERE p.server_id = target_server
              AND p.worker_id = worker_auth.current_worker_id()
              AND p.generation = s.generation
        )
        OR EXISTS (
            SELECT 1 FROM public.operations o
            JOIN public.servers s ON s.id = o.server_id
            WHERE o.server_id = target_server
              AND o.worker_id = worker_auth.current_worker_id()
              AND o.generation = s.generation
              AND o.status IN ('pending', 'running')
        )
        OR EXISTS (
            SELECT 1 FROM public.world_exports e
            WHERE e.server_id = target_server
              AND e.worker_id = worker_auth.current_worker_id()
              AND e.status IN ('pending', 'running')
        )
    )
$$;
REVOKE ALL ON FUNCTION worker_auth.can_access_server(uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION worker_auth.can_access_server(uuid) TO worker_runtime;

DO $$
BEGIN
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO worker_runtime', current_database());
END;
$$;

REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA public FROM worker_runtime;
REVOKE ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA public FROM worker_runtime;
REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA public FROM PUBLIC;
REVOKE ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA public FROM PUBLIC;

ALTER TABLE workers ENABLE ROW LEVEL SECURITY;
CREATE POLICY worker_self_row ON workers
    USING (id = worker_auth.current_worker_id())
    WITH CHECK (id = worker_auth.current_worker_id());
GRANT SELECT ON workers TO worker_runtime;

ALTER TABLE servers ENABLE ROW LEVEL SECURITY;
CREATE POLICY worker_assigned_servers ON servers
    USING (worker_auth.can_access_server(id))
    WITH CHECK (worker_auth.can_access_server(id));
GRANT SELECT ON servers TO worker_runtime;

ALTER TABLE placements ENABLE ROW LEVEL SECURITY;
CREATE POLICY worker_assigned_placements ON placements
    USING (worker_id = worker_auth.current_worker_id());
GRANT SELECT ON placements TO worker_runtime;

ALTER TABLE worker_snapshot_cache ENABLE ROW LEVEL SECURITY;
CREATE POLICY worker_own_snapshot_cache ON worker_snapshot_cache
    USING (worker_id = worker_auth.current_worker_id())
    WITH CHECK (worker_id = worker_auth.current_worker_id()
                AND worker_auth.can_access_server(server_id));
GRANT SELECT, INSERT, UPDATE, DELETE ON worker_snapshot_cache TO worker_runtime;

ALTER TABLE snapshots ENABLE ROW LEVEL SECURITY;
CREATE POLICY worker_assigned_snapshots ON snapshots
    USING (worker_auth.can_access_server(server_id))
    WITH CHECK (worker_auth.can_access_server(server_id));
GRANT SELECT, INSERT, UPDATE, DELETE ON snapshots TO worker_runtime;

ALTER TABLE operations ENABLE ROW LEVEL SECURITY;
CREATE POLICY worker_assigned_operations ON operations
    USING (worker_id = worker_auth.current_worker_id()
           AND worker_auth.can_access_server(server_id))
    WITH CHECK (worker_id = worker_auth.current_worker_id());
GRANT SELECT ON operations TO worker_runtime;

ALTER TABLE server_logs ENABLE ROW LEVEL SECURITY;
CREATE POLICY worker_assigned_logs ON server_logs
    USING (worker_auth.can_access_server(server_id))
    WITH CHECK (worker_auth.can_access_server(server_id));
GRANT SELECT, INSERT (server_id, line) ON server_logs TO worker_runtime;
GRANT USAGE ON SEQUENCE server_logs_id_seq TO worker_runtime;

ALTER TABLE server_events ENABLE ROW LEVEL SECURITY;
ALTER FUNCTION public.emit_server_event(uuid, text) SECURITY DEFINER;
ALTER FUNCTION public.emit_server_event(uuid, text) SET search_path = pg_catalog, public;
REVOKE ALL ON FUNCTION public.emit_server_event(uuid, text) FROM PUBLIC, worker_runtime;
ALTER FUNCTION public.emit_server_row_event() SECURITY DEFINER;
ALTER FUNCTION public.emit_server_row_event() SET search_path = pg_catalog, public;
ALTER FUNCTION public.emit_operation_event() SECURITY DEFINER;
ALTER FUNCTION public.emit_operation_event() SET search_path = pg_catalog, public;
ALTER FUNCTION public.emit_snapshot_event() SECURITY DEFINER;
ALTER FUNCTION public.emit_snapshot_event() SET search_path = pg_catalog, public;
ALTER FUNCTION public.emit_log_event() SECURITY DEFINER;
ALTER FUNCTION public.emit_log_event() SET search_path = pg_catalog, public;
ALTER FUNCTION public.emit_players_event() SECURITY DEFINER;
ALTER FUNCTION public.emit_players_event() SET search_path = pg_catalog, public;
ALTER FUNCTION public.emit_whitelist_state_event() SECURITY DEFINER;
ALTER FUNCTION public.emit_whitelist_state_event() SET search_path = pg_catalog, public;

ALTER TABLE server_players ENABLE ROW LEVEL SECURITY;
CREATE POLICY worker_assigned_players ON server_players
    USING (worker_auth.can_access_server(server_id))
    WITH CHECK (worker_auth.can_access_server(server_id));
GRANT SELECT, INSERT, UPDATE, DELETE ON server_players TO worker_runtime;

ALTER TABLE server_whitelist_state ENABLE ROW LEVEL SECURITY;
CREATE POLICY worker_assigned_whitelist_state ON server_whitelist_state
    USING (worker_auth.can_access_server(server_id))
    WITH CHECK (worker_auth.can_access_server(server_id));
GRANT SELECT ON server_whitelist_state TO worker_runtime;
GRANT UPDATE (applied_revision) ON server_whitelist_state TO worker_runtime;
GRANT INSERT (server_id, applied_revision) ON server_whitelist_state TO worker_runtime;
ALTER FUNCTION public.bump_whitelist_revision() SECURITY DEFINER;
ALTER FUNCTION public.bump_whitelist_revision() SET search_path = pg_catalog, public;
ALTER FUNCTION public.bump_whitelist_setting_revision() SECURITY DEFINER;
ALTER FUNCTION public.bump_whitelist_setting_revision() SET search_path = pg_catalog, public;
REVOKE ALL ON FUNCTION public.bump_whitelist_revision() FROM PUBLIC, worker_runtime;
REVOKE ALL ON FUNCTION public.bump_whitelist_setting_revision() FROM PUBLIC, worker_runtime;

ALTER TABLE whitelist_entries ENABLE ROW LEVEL SECURITY;
CREATE POLICY worker_assigned_whitelist_entries ON whitelist_entries
    USING (worker_auth.can_access_server(server_id))
    WITH CHECK (worker_auth.can_access_server(server_id));
GRANT SELECT ON whitelist_entries TO worker_runtime;

ALTER TABLE server_operator_state ENABLE ROW LEVEL SECURITY;
CREATE POLICY worker_assigned_operator_state ON server_operator_state
    USING (worker_auth.can_access_server(server_id))
    WITH CHECK (worker_auth.can_access_server(server_id));
GRANT SELECT ON server_operator_state TO worker_runtime;
GRANT UPDATE (applied_revision) ON server_operator_state TO worker_runtime;
GRANT INSERT (server_id, applied_revision) ON server_operator_state TO worker_runtime;

ALTER TABLE world_exports ENABLE ROW LEVEL SECURITY;
CREATE POLICY worker_assigned_exports ON world_exports
    USING (worker_id = worker_auth.current_worker_id()
           AND status IN ('pending', 'running'))
    WITH CHECK (worker_id = worker_auth.current_worker_id());
GRANT SELECT ON world_exports TO worker_runtime;

ALTER TABLE identities ENABLE ROW LEVEL SECURITY;
CREATE POLICY worker_assigned_server_owner_identities ON identities
    USING (provider IN ('minecraft_java', 'minecraft_bedrock')
           AND minecraft_player_id IS NOT NULL
           AND EXISTS (
        SELECT 1 FROM public.servers s
        WHERE s.owner_id = identities.user_id
          AND worker_auth.can_access_server(s.id)
    ));
GRANT SELECT (id, user_id, provider, display_name, created_at, minecraft_player_id)
    ON identities TO worker_runtime;
