-- Placements retain the last assignment for stopped servers. Only port_leases
-- reserve sockets: inactive placement history must not prevent port reuse.
ALTER TABLE placements DROP CONSTRAINT placements_worker_id_java_port_key;
ALTER TABLE placements DROP CONSTRAINT placements_worker_id_bedrock_port_key;
