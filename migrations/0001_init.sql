CREATE TYPE server_desired_state AS ENUM ('running', 'stopped', 'deleted');
CREATE TYPE server_observed_state AS ENUM ('sleeping', 'queued', 'restoring', 'starting', 'online', 'stopping', 'snapshotting', 'failed', 'deleted');
CREATE TYPE operation_kind AS ENUM ('start', 'stop', 'restart', 'snapshot', 'delete');
CREATE TYPE operation_status AS ENUM ('pending', 'running', 'succeeded', 'failed', 'cancelled');
CREATE TYPE snapshot_reason AS ENUM ('periodic', 'shutdown', 'manual');

CREATE TABLE users (
    id uuid PRIMARY KEY,
    oauth_provider text NOT NULL,
    oauth_subject text NOT NULL,
    email text,
    display_name text NOT NULL,
    account_status text NOT NULL DEFAULT 'active' CHECK (account_status IN ('active', 'suspended')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (oauth_provider, oauth_subject)
);

CREATE TABLE oauth_states (
    state_hash bytea PRIMARY KEY,
    expires_at timestamptz NOT NULL
);

CREATE TABLE sessions (
    token_hash bytea PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX sessions_user_idx ON sessions(user_id);

CREATE TABLE servers (
    id uuid PRIMARY KEY,
    owner_id uuid NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    subdomain text NOT NULL UNIQUE CHECK (subdomain ~ '^[a-z0-9][a-z0-9-]{1,30}[a-z0-9]$'),
    desired_state server_desired_state NOT NULL DEFAULT 'stopped',
    observed_state server_observed_state NOT NULL DEFAULT 'sleeping',
    settings jsonb NOT NULL DEFAULT '{}'::jsonb,
    storage_bytes bigint NOT NULL DEFAULT 0 CHECK (storage_bytes >= 0),
    generation bigint NOT NULL DEFAULT 0 CHECK (generation >= 0),
    current_snapshot_generation bigint,
    last_error text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE workers (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    address text NOT NULL,
    total_memory_bytes bigint NOT NULL CHECK (total_memory_bytes > 0),
    total_disk_bytes bigint NOT NULL CHECK (total_disk_bytes > 0),
    total_cpu_millis bigint NOT NULL CHECK (total_cpu_millis > 0),
    reserved_memory_bytes bigint NOT NULL DEFAULT 0 CHECK (reserved_memory_bytes >= 0),
    reserved_disk_bytes bigint NOT NULL DEFAULT 0 CHECK (reserved_disk_bytes >= 0),
    reserved_cpu_millis bigint NOT NULL DEFAULT 0 CHECK (reserved_cpu_millis >= 0),
    heartbeat_at timestamptz NOT NULL DEFAULT now(),
    draining boolean NOT NULL DEFAULT false,
    port_min integer NOT NULL DEFAULT 20000,
    port_max integer NOT NULL DEFAULT 40000,
    CHECK (port_min > 1024 AND port_max <= 65535 AND port_min < port_max)
);
CREATE INDEX workers_healthy_idx ON workers(heartbeat_at) WHERE NOT draining;

CREATE TABLE placements (
    server_id uuid PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
    worker_id uuid NOT NULL REFERENCES workers(id) ON DELETE RESTRICT,
    generation bigint NOT NULL,
    lease_token uuid NOT NULL,
    java_port integer NOT NULL,
    bedrock_port integer NOT NULL,
    active boolean NOT NULL DEFAULT false,
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (worker_id, java_port),
    UNIQUE (worker_id, bedrock_port)
);
CREATE INDEX placements_worker_idx ON placements(worker_id);

CREATE TABLE port_leases (
    worker_id uuid NOT NULL REFERENCES workers(id) ON DELETE CASCADE,
    port integer NOT NULL,
    server_id uuid NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    purpose text NOT NULL CHECK (purpose IN ('java', 'bedrock')),
    PRIMARY KEY(worker_id, port),
    UNIQUE(server_id, purpose)
);

CREATE TABLE worker_snapshot_cache (
    worker_id uuid NOT NULL REFERENCES workers(id) ON DELETE CASCADE,
    server_id uuid NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    generation bigint NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(worker_id, server_id)
);

CREATE TABLE snapshots (
    id uuid PRIMARY KEY,
    server_id uuid NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    generation bigint NOT NULL,
    manifest_key text NOT NULL,
    size_bytes bigint NOT NULL CHECK (size_bytes >= 0),
    checksum text NOT NULL,
    reason snapshot_reason NOT NULL,
    complete boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(server_id, generation)
);
CREATE INDEX snapshots_latest_idx ON snapshots(server_id, generation DESC) WHERE complete;

CREATE TABLE start_requests (
    server_id uuid PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
    reason text NOT NULL,
    requested_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX start_requests_order_idx ON start_requests(requested_at);

CREATE TABLE operations (
    id uuid PRIMARY KEY,
    server_id uuid NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    worker_id uuid REFERENCES workers(id) ON DELETE SET NULL,
    kind operation_kind NOT NULL,
    status operation_status NOT NULL DEFAULT 'pending',
    generation bigint NOT NULL,
    attempt integer NOT NULL DEFAULT 0,
    payload jsonb NOT NULL DEFAULT '{}'::jsonb,
    last_error text,
    claimed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX operations_pending_worker_idx ON operations(worker_id, created_at) WHERE status = 'pending';
CREATE UNIQUE INDEX one_live_lifecycle_operation_idx ON operations(server_id)
WHERE status IN ('pending', 'running') AND kind IN ('start', 'stop', 'restart', 'delete');

CREATE TABLE server_logs (
    id bigserial PRIMARY KEY,
    server_id uuid NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    line text NOT NULL
);
CREATE INDEX server_logs_recent_idx ON server_logs(server_id, id DESC);

CREATE OR REPLACE FUNCTION notify_route_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  PERFORM pg_notify('route_changes', COALESCE(NEW.server_id, OLD.server_id)::text);
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE TRIGGER placements_route_change
AFTER INSERT OR UPDATE OR DELETE ON placements
FOR EACH ROW EXECUTE FUNCTION notify_route_change();

CREATE OR REPLACE FUNCTION notify_server_route_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  PERFORM pg_notify('route_changes', COALESCE(NEW.id, OLD.id)::text);
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE TRIGGER servers_route_change
AFTER UPDATE OF observed_state, subdomain ON servers
FOR EACH ROW EXECUTE FUNCTION notify_server_route_change();
