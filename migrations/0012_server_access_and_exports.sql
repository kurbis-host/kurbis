ALTER TABLE identities ADD COLUMN minecraft_player_id uuid;
ALTER TYPE snapshot_reason ADD VALUE IF NOT EXISTS 'export';
UPDATE identities SET minecraft_player_id = subject::uuid
WHERE provider = 'minecraft_java';
ALTER TABLE identities ADD CONSTRAINT identities_minecraft_profile_check
    CHECK (provider <> 'minecraft_java' OR minecraft_player_id IS NOT NULL);
ALTER TABLE minecraft_login_tokens ADD COLUMN minecraft_player_id uuid;
UPDATE minecraft_login_tokens SET minecraft_player_id = subject::uuid
WHERE provider = 'minecraft_java';
DELETE FROM minecraft_login_tokens WHERE provider = 'minecraft_bedrock';
ALTER TABLE minecraft_login_tokens ALTER COLUMN minecraft_player_id SET NOT NULL;

DROP INDEX one_live_lifecycle_operation_idx;
CREATE UNIQUE INDEX one_live_lifecycle_operation_idx ON operations(server_id)
WHERE status IN ('pending', 'running') AND kind IN ('start', 'stop', 'restart', 'reset', 'delete');

CREATE TABLE server_operator_state (
    server_id uuid PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
    revision bigint NOT NULL DEFAULT 1,
    applied_revision bigint NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO server_operator_state(server_id)
SELECT id FROM servers ON CONFLICT DO NOTHING;

CREATE TABLE world_exports (
    id uuid PRIMARY KEY,
    server_id uuid NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    requested_by uuid NOT NULL,
    status text NOT NULL CHECK (status IN ('pending', 'running', 'completed', 'failed', 'cancelled', 'expired')),
    stage text NOT NULL DEFAULT 'requested',
    source_generation bigint,
    worker_id uuid REFERENCES workers(id) ON DELETE SET NULL,
    attempt integer NOT NULL DEFAULT 0,
    archive_key text,
    archive_size bigint,
    archive_checksum text,
    last_error text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz,
    CHECK ((status <> 'completed') OR (archive_key IS NOT NULL AND archive_size IS NOT NULL AND archive_checksum IS NOT NULL AND expires_at IS NOT NULL))
);
CREATE INDEX world_exports_pending_idx ON world_exports(created_at, id) WHERE status = 'pending';
CREATE INDEX world_exports_pin_idx ON world_exports(server_id, source_generation)
    WHERE status IN ('pending', 'running') AND source_generation IS NOT NULL;
CREATE INDEX world_exports_expiry_idx ON world_exports(expires_at) WHERE status = 'completed';
CREATE UNIQUE INDEX one_active_world_export_per_server_idx ON world_exports(server_id)
    WHERE status IN ('pending', 'running');

CREATE FUNCTION bump_owner_access_revisions() RETURNS trigger LANGUAGE plpgsql AS $$
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
  IF new_owner IS DISTINCT FROM old_owner THEN
    INSERT INTO server_whitelist_state(server_id, revision, applied_revision)
      SELECT id, 1, 0 FROM servers WHERE owner_id = new_owner AND new_owner IS NOT NULL
      ON CONFLICT(server_id) DO UPDATE SET revision = server_whitelist_state.revision + 1, updated_at = now();
    INSERT INTO server_operator_state(server_id, revision, applied_revision)
      SELECT id, 1, 0 FROM servers WHERE owner_id = new_owner AND new_owner IS NOT NULL
      ON CONFLICT(server_id) DO UPDATE SET revision = server_operator_state.revision + 1, updated_at = now();
  END IF;
  IF TG_OP = 'DELETE' THEN
    RETURN OLD;
  END IF;
  RETURN NEW;
END;
$$;
CREATE TRIGGER identities_bump_owner_access_revisions
AFTER INSERT OR UPDATE OF user_id, minecraft_player_id, provider, display_name OR DELETE ON identities
FOR EACH ROW EXECUTE FUNCTION bump_owner_access_revisions();

CREATE OR REPLACE FUNCTION notify_server_route_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  PERFORM pg_notify('route_changes', COALESCE(NEW.id, OLD.id)::text);
  IF TG_OP = 'DELETE' THEN
    RETURN OLD;
  END IF;
  RETURN NEW;
END;
$$;
DROP TRIGGER servers_route_change ON servers;
CREATE TRIGGER servers_route_change
AFTER INSERT OR DELETE OR UPDATE OF observed_state, desired_state, subdomain ON servers
FOR EACH ROW EXECUTE FUNCTION notify_server_route_change();
