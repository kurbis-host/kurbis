-- Distinguish an immutable build record observed by the source poller from a
-- build that was actually requested by a join, webhook, or eager head update.
-- The poller may persist thousands of PR aliases without turning every open
-- historical PR into recoverable work. Only requested rows enter recovery.
ALTER TABLE public.ci_builds
    ADD COLUMN requested_at timestamptz;

CREATE INDEX ci_builds_requested_queue_idx
    ON public.ci_builds (requested_at, updated_at, id)
    WHERE requested_at IS NOT NULL
      AND stage IN ('queued', 'fetching', 'patching', 'compiling', 'packaging');
