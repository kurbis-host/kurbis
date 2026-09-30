CREATE OR REPLACE FUNCTION emit_server_event(target_server uuid, event_name text)
RETURNS void LANGUAGE plpgsql AS $$
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
