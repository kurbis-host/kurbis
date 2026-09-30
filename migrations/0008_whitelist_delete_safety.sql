CREATE OR REPLACE FUNCTION bump_whitelist_revision()
RETURNS trigger LANGUAGE plpgsql AS $$
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
