-- One durable desired build per pull request. Immutable ci_builds remain the
-- content-addressed build cache; this table owns the mutable scheduling intent
-- so a newer head can supersede an older request without rewriting its build
-- diagnostics or artifact history.
CREATE TABLE public.ci_build_queue (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    repository_id uuid NOT NULL REFERENCES public.ci_watched_repositories(id) ON DELETE CASCADE,
    pull_request_number bigint NOT NULL CHECK (pull_request_number > 0),
    commit_sha text NOT NULL CHECK (commit_sha ~ '^[0-9a-f]{40}$'),
    build_id uuid NOT NULL REFERENCES public.ci_builds(id) ON DELETE CASCADE,
    source_repository text,
    state text NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'running', 'succeeded', 'failed', 'superseded', 'cancelled')),
    attempt integer NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    claim_token uuid,
    claim_expires_at timestamptz,
    retry_at timestamptz,
    failure_class text CHECK (failure_class IS NULL OR failure_class IN ('transient', 'permanent')),
    last_error text,
    requested_at timestamptz NOT NULL DEFAULT now(),
    started_at timestamptz,
    finished_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT ci_build_queue_claim_check
        CHECK ((claim_token IS NULL) = (claim_expires_at IS NULL))
);

-- A PR may have historical terminal rows, but only one desired request may be
-- pending, running, or awaiting a retry at a time.
CREATE UNIQUE INDEX ci_build_queue_current_idx
    ON public.ci_build_queue(repository_id, pull_request_number)
    WHERE state IN ('pending', 'running', 'failed');

CREATE UNIQUE INDEX ci_build_queue_commit_idx
    ON public.ci_build_queue(repository_id, pull_request_number, commit_sha)
    WHERE state IN ('pending', 'running', 'failed');

CREATE INDEX ci_build_queue_claim_idx
    ON public.ci_build_queue(repository_id, requested_at, id)
    WHERE state = 'pending';

CREATE INDEX ci_build_queue_lease_idx
    ON public.ci_build_queue(claim_expires_at, updated_at)
    WHERE state = 'running';

CREATE INDEX ci_build_queue_gc_idx
    ON public.ci_build_queue(finished_at, id)
    WHERE state IN ('succeeded', 'failed', 'superseded', 'cancelled');

-- Recover work admitted by the pre-queue schema without expanding every
-- historical PR. Only the currently open head for each PR is eligible.
INSERT INTO public.ci_build_queue(
    repository_id, pull_request_number, commit_sha, build_id, source_repository,
    state, retry_at, failure_class, last_error, requested_at, started_at, finished_at
)
SELECT DISTINCT ON (watched.id, ref.number)
       watched.id, ref.number, build.commit_sha, build.id, ref.source_repository,
       CASE WHEN build.stage = 'failed' THEN 'failed' ELSE 'pending' END,
       build.retry_at, build.failure_class, build.failure_message,
       build.created_at, NULL, NULL
FROM public.ci_refs ref
JOIN public.ci_watched_repositories watched ON watched.id = ref.repository_id
JOIN public.ci_builds build
  ON build.repository = watched.repository AND build.commit_sha = ref.commit_sha
WHERE ref.kind = 'pull_request' AND ref.number IS NOT NULL
  AND NOT ref.closed AND NOT ref.deleted
  AND build.stage IN ('queued', 'fetching', 'patching', 'compiling', 'packaging', 'failed')
  AND (build.stage <> 'failed' OR build.failure_class = 'transient')
ORDER BY watched.id, ref.number, build.updated_at DESC, build.id
ON CONFLICT DO NOTHING;
