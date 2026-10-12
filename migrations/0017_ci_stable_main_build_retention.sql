-- The watched default branch uses the same immutable build pipeline as PRs, but
-- its artifact is the host's stable Pumpkin runtime input rather than a 72-hour
-- preview. A nullable expiry is intentional: NULL means permanent retention.
ALTER TABLE public.ci_builds
    ADD COLUMN retention_policy text NOT NULL DEFAULT 'preview'
        CHECK (retention_policy IN ('preview', 'permanent'));

ALTER TABLE public.ci_builds
    ALTER COLUMN expires_at DROP NOT NULL;

ALTER TABLE public.ci_artifacts
    ALTER COLUMN expires_at DROP NOT NULL;

CREATE INDEX ci_builds_retention_idx
    ON public.ci_builds (retention_policy, updated_at);
