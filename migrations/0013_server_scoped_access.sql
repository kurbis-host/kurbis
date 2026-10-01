BEGIN;

CREATE TABLE server_instance_capabilities (
    server_id uuid PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
    worker_id uuid NOT NULL REFERENCES workers(id) ON DELETE CASCADE,
    placement_generation bigint NOT NULL,
    token_hash bytea NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE server_login_tokens (
    token_hash bytea PRIMARY KEY,
    server_id uuid NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX server_login_tokens_expiry_idx ON server_login_tokens(expires_at);
CREATE INDEX server_login_tokens_server_created_idx ON server_login_tokens(server_id, created_at DESC);

CREATE TABLE server_sessions (
    token_hash bytea PRIMARY KEY,
    server_id uuid NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX server_sessions_expiry_idx ON server_sessions(expires_at);

CREATE TABLE server_login_rate_limits (
    server_id uuid PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
    window_started_at timestamptz NOT NULL,
    request_count integer NOT NULL CHECK (request_count >= 0)
);

COMMIT;
