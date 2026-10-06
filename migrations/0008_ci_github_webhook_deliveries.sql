-- Signed GitHub PR lifecycle events preserve every synchronized head/range,
-- including heads replaced before the independent 60-second poller sees them.
CREATE TABLE public.ci_github_webhook_deliveries (
    delivery_id uuid PRIMARY KEY,
    repository_id uuid NOT NULL REFERENCES public.ci_watched_repositories(id) ON DELETE CASCADE,
    action text NOT NULL CHECK (action IN ('opened', 'reopened', 'synchronize')),
    pull_request_number bigint NOT NULL CHECK (pull_request_number > 0),
    source_repository text NOT NULL CHECK (source_repository ~ '^[^/[:space:]]+/[^/[:space:]]+$'),
    base_sha text NOT NULL CHECK (base_sha ~ '^[0-9a-f]{40}$'),
    head_sha text NOT NULL CHECK (head_sha ~ '^[0-9a-f]{40}$'),
    payload_digest bytea NOT NULL CHECK (octet_length(payload_digest) = 32),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    claim_token uuid,
    claim_expires_at timestamptz,
    last_error text,
    received_at timestamptz NOT NULL DEFAULT now(),
    processed_at timestamptz,
    CONSTRAINT ci_github_webhook_claim_check CHECK ((claim_token IS NULL) = (claim_expires_at IS NULL))
);

CREATE INDEX ci_github_webhook_pending_idx
    ON public.ci_github_webhook_deliveries (next_attempt_at, received_at)
    WHERE processed_at IS NULL;

CREATE INDEX ci_github_webhook_retention_idx
    ON public.ci_github_webhook_deliveries (processed_at)
    WHERE processed_at IS NOT NULL;
