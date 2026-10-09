ALTER TABLE public.ci_watched_repositories
    ADD COLUMN poll_lease_token uuid,
    ADD COLUMN poll_lease_expires_at timestamptz,
    ADD CONSTRAINT ci_watched_repositories_poll_lease_check
        CHECK ((poll_lease_token IS NULL) = (poll_lease_expires_at IS NULL));
