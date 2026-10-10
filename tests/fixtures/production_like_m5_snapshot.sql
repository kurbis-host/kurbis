-- Production-shaped data captured before the CI preview migration family.
-- IDs are fixed so the upgrade assertions can prove row identity and joins.
INSERT INTO users(id,display_name,account_status)
VALUES ('00000000-0000-0000-0000-000000000101','Production Owner','active');

INSERT INTO workers(
    id,name,address,signal_address,total_memory_bytes,total_disk_bytes,total_cpu_millis,
    reserved_memory_bytes,reserved_disk_bytes,reserved_cpu_millis,heartbeat_at,draining,
    port_min,port_max
) VALUES (
    '00000000-0000-0000-0000-000000000303','production-worker','10.10.0.3','10.10.0.3',
    4294967296,21474836480,4000,1073741824,5368709120,1000,
    now()+interval '5 minutes',false,30000,30100
);

INSERT INTO servers(
    id,owner_id,subdomain,desired_state,observed_state,settings,storage_bytes,generation,
    current_snapshot_generation,is_default
) VALUES (
    '00000000-0000-0000-0000-000000000202',
    '00000000-0000-0000-0000-000000000101',
    'production-world','running','online','{"difficulty":"normal"}',2048,12,7,true
);

INSERT INTO placements(
    server_id,worker_id,generation,lease_token,java_port,bedrock_port,active,signal_port
) VALUES (
    '00000000-0000-0000-0000-000000000202',
    '00000000-0000-0000-0000-000000000303',
    12,'00000000-0000-0000-0000-000000000404',25565,19132,true,25567
);

INSERT INTO snapshots(
    id,server_id,generation,manifest_key,size_bytes,checksum,reason,complete
) VALUES (
    '00000000-0000-0000-0000-000000000505',
    '00000000-0000-0000-0000-000000000202',
    7,'servers/00000000-0000-0000-0000-000000000202/snapshots/7.json',2048,
    'production-checksum-7','shutdown',true
);
